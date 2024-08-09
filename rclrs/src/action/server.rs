use crate::{
    action::{CancelResponse, GoalResponse, GoalUuid, ServerGoalHandle},
    error::{RclReturnCode, ToResult},
    rcl_bindings::*,
    wait::WaitableNumEntities,
    Clock, DropGuard, NodeHandle, RclrsError, ENTITY_LIFECYCLE_MUTEX,
};
use rosidl_runtime_rs::{Action, ActionImpl, Message, Service};
use std::{
    collections::HashMap,
    ffi::CString,
    sync::{atomic::AtomicBool, Arc, Mutex, MutexGuard},
};

// SAFETY: The functions accessing this type, including drop(), shouldn't care about the thread
// they are running in. Therefore, this type can be safely sent to another thread.
unsafe impl Send for rcl_action_server_t {}

/// Manage the lifecycle of an `rcl_action_server_t`, including managing its dependencies
/// on `rcl_node_t` and `rcl_context_t` by ensuring that these dependencies are
/// [dropped after][1] the `rcl_action_server_t`.
///
/// [1]: <https://doc.rust-lang.org/reference/destructors.html>
pub struct ActionServerHandle {
    rcl_action_server: Mutex<rcl_action_server_t>,
    node_handle: Arc<NodeHandle>,
    pub(crate) in_use_by_wait_set: Arc<AtomicBool>,
}

impl ActionServerHandle {
    pub(crate) fn lock(&self) -> MutexGuard<rcl_action_server_t> {
        self.rcl_action_server.lock().unwrap()
    }
}

impl Drop for ActionServerHandle {
    fn drop(&mut self) {
        let rcl_action_server = self.rcl_action_server.get_mut().unwrap();
        let mut rcl_node = self.node_handle.rcl_node.lock().unwrap();
        let _lifecycle_lock = ENTITY_LIFECYCLE_MUTEX.lock().unwrap();
        // SAFETY: The entity lifecycle mutex is locked to protect against the risk of
        // global variables in the rmw implementation being unsafely modified during cleanup.
        unsafe {
            rcl_action_server_fini(rcl_action_server, &mut *rcl_node);
        }
    }
}

/// Trait to be implemented by concrete ActionServer structs.
///
/// See [`ActionServer<T>`] for an example
pub trait ActionServerBase: Send + Sync {
    /// Internal function to get a reference to the `rcl` handle.
    fn handle(&self) -> &ActionServerHandle;
    /// Returns the number of underlying entities for the action server.
    fn num_entities(&self) -> &WaitableNumEntities;
    /// Tries to run the callback for the given readiness mode.
    fn execute(&self, mode: ReadyMode) -> Result<(), RclrsError>;
}

pub(crate) enum ReadyMode {
    GoalRequest,
    CancelRequest,
    ResultRequest,
    GoalExpired,
}

pub type GoalCallback<ActionT> = dyn Fn(GoalUuid, <ActionT as rosidl_runtime_rs::Action>::Goal) -> GoalResponse + 'static + Send + Sync;
pub type CancelCallback<ActionT> = dyn Fn(ServerGoalHandle<ActionT>) -> CancelResponse + 'static + Send + Sync;
pub type AcceptedCallback<ActionT> = dyn Fn(ServerGoalHandle<ActionT>) + 'static + Send + Sync;

pub struct ActionServer<ActionT>
where
    ActionT: rosidl_runtime_rs::Action,
{
    pub(crate) handle: Arc<ActionServerHandle>,
    num_entities: WaitableNumEntities,
    goal_callback: Box<GoalCallback<ActionT>>,
    cancel_callback: Box<CancelCallback<ActionT>>,
    accepted_callback: Box<AcceptedCallback<ActionT>>,
    goal_handles: Mutex<HashMap<GoalUuid, Arc<ServerGoalHandle<ActionT>>>>,
}

impl<T> ActionServer<T>
where
    T: rosidl_runtime_rs::Action + rosidl_runtime_rs::ActionImpl,
{
    /// Creates a new action server.
    pub(crate) fn new(
        node_handle: Arc<NodeHandle>,
        clock: Clock,
        topic: &str,
        goal_callback: impl Fn(GoalUuid, T::Goal) -> GoalResponse + 'static + Send + Sync,
        cancel_callback: impl Fn(ServerGoalHandle<T>) -> CancelResponse + 'static + Send + Sync,
        accepted_callback: impl Fn(ServerGoalHandle<T>) + 'static + Send + Sync,
    ) -> Result<Self, RclrsError>
    where
        T: rosidl_runtime_rs::Action + rosidl_runtime_rs::ActionImpl,
    {
        // SAFETY: Getting a zero-initialized value is always safe.
        let mut rcl_action_server = unsafe { rcl_action_get_zero_initialized_server() };
        let type_support = T::get_type_support() as *const rosidl_action_type_support_t;
        let topic_c_string = CString::new(topic).map_err(|err| RclrsError::StringContainsNul {
            err,
            s: topic.into(),
        })?;

        // SAFETY: No preconditions for this function.
        let action_server_options = unsafe { rcl_action_server_get_default_options() };

        {
            let mut rcl_node = node_handle.rcl_node.lock().unwrap();
            let rcl_clock = clock.rcl_clock();
            let mut rcl_clock = rcl_clock.lock().unwrap();
            let _lifecycle_lock = ENTITY_LIFECYCLE_MUTEX.lock().unwrap();

            // SAFETY:
            // * The rcl_action_server is zero-initialized as mandated by this function.
            // * The rcl_node is kept alive by the NodeHandle because it is a dependency of the action server.
            // * The topic name and the options are copied by this function, so they can be dropped
            //   afterwards.
            // * The entity lifecycle mutex is locked to protect against the risk of global
            //   variables in the rmw implementation being unsafely modified during initialization.
            unsafe {
                rcl_action_server_init(
                    &mut rcl_action_server,
                    &mut *rcl_node,
                    &mut *rcl_clock,
                    type_support,
                    topic_c_string.as_ptr(),
                    &action_server_options,
                )
                .ok()?;
            }
        }

        let handle = Arc::new(ActionServerHandle {
            rcl_action_server: Mutex::new(rcl_action_server),
            node_handle,
            in_use_by_wait_set: Arc::new(AtomicBool::new(false)),
        });

        let mut num_entities = WaitableNumEntities::default();
        unsafe {
            rcl_action_server_wait_set_get_num_entities(
                &*handle.lock(),
                &mut num_entities.num_subscriptions,
                &mut num_entities.num_guard_conditions,
                &mut num_entities.num_timers,
                &mut num_entities.num_clients,
                &mut num_entities.num_services,
            )
            .ok()?;
        }

        Ok(Self {
            handle,
            num_entities,
            goal_callback: Box::new(goal_callback),
            cancel_callback: Box::new(cancel_callback),
            accepted_callback: Box::new(accepted_callback),
            goal_handles: Mutex::new(HashMap::new()),
        })
    }

    fn take_goal_request(&self) -> Result<(<<T::SendGoalService as Service>::Request as Message>::RmwMsg, rmw_request_id_t), RclrsError> {
        let mut request_id = rmw_request_id_t {
            writer_guid: [0; 16],
            sequence_number: 0,
        };
        type RmwRequest<T> = <<<T as ActionImpl>::SendGoalService as Service>::Request as Message>::RmwMsg;
        let mut request_rmw = RmwRequest::<T>::default();
        let handle = &*self.handle.lock();
        unsafe {
            // SAFETY: The three pointers are valid/initialized
            rcl_action_take_goal_request(
                handle,
                &mut request_id,
                &mut request_rmw as *mut RmwRequest<T> as *mut _,
            )
        }
        .ok()?;

        Ok((request_rmw, request_id))
    }

    fn send_goal_response(
        &self,
        mut request_id: rmw_request_id_t,
        accepted: bool,
    ) -> Result<(), RclrsError> {
        type RmwResponse<T> = <<<T as ActionImpl>::SendGoalService as Service>::Response as Message>::RmwMsg;
        let mut response_rmw = RmwResponse::<T>::default();
        <T as ActionImpl>::set_goal_response_accepted(&mut response_rmw, accepted);
        let handle = &*self.handle.lock();
        let result = unsafe {
            // SAFETY: The action server handle is locked and so synchronized with other
            // functions. The request_id and response message are uniquely owned, and so will
            // not mutate during this function call.
            // Also, when appropriate, `rcl_action_accept_new_goal()` has been called beforehand,
            // as specified in the `rcl_action` docs.
            rcl_action_send_goal_response(
                handle,
                &mut request_id,
                &mut response_rmw as *mut RmwResponse<T> as *mut _,
            )
        }
        .ok();
        match result {
            Ok(()) => Ok(()),
            Err(RclrsError::RclError {
                code: RclReturnCode::Timeout,
                ..
            }) => {
                // TODO(nwn): Log an error and continue.
                // (See https://github.com/ros2/rclcpp/pull/2215 for reasoning.)
                Ok(())
            }
            _ => result,
        }
    }

    fn execute_goal_request(&self) -> Result<(), RclrsError> {
        let (request, request_id) = match self.take_goal_request() {
            Ok(res) => res,
            Err(RclrsError::RclError {
                code: RclReturnCode::ServiceTakeFailed,
                ..
            }) => {
                // Spurious wakeup – this may happen even when a waitset indicated that this
                // action was ready, so it shouldn't be an error.
                return Ok(());
            }
            Err(err) => return Err(err),
        };

        let uuid = GoalUuid(<T as ActionImpl>::get_goal_request_uuid(&request));

        let response: GoalResponse = {
            todo!("Optionally convert request to an idiomatic type for the user's callback.");
            todo!("Call self.goal_callback(uuid, request)");
        };

        // Don't continue if the goal was rejected by the user.
        if response == GoalResponse::Reject {
            return self.send_goal_response(request_id, false);
        }

        let goal_handle = {
            // SAFETY: No preconditions
            let mut goal_info = unsafe { rcl_action_get_zero_initialized_goal_info() };
            // Only populate the goal UUID; the timestamp will be set internally by
            // rcl_action_accept_new_goal().
            goal_info.goal_id.uuid = uuid.0;

            let server_handle = &mut *self.handle.lock();
            let goal_handle_ptr = unsafe {
                // SAFETY: The action server handle is locked and so synchronized with other
                // functions. The request_id and response message are uniquely owned, and so will
                // not mutate during this function call. The returned goal handle pointer should be
                // valid unless it is null.
                rcl_action_accept_new_goal(server_handle, &goal_info)
            };
            if goal_handle_ptr.is_null() {
                // Other than rcl_get_error_string(), there's no indication what happened.
                panic!("Failed to accept goal");
            } else {
                Arc::new(ServerGoalHandle::<T>::new(
                    goal_handle_ptr,
                    todo!("Create an Arc holding the goal message"),
                    uuid,
                ))
            }
        };

        self.send_goal_response(request_id, true)?;

        self.goal_handles.lock().unwrap().insert(uuid, Arc::clone(&goal_handle));

        if response == GoalResponse::AcceptAndExecute {
            goal_handle.execute()?;
        }

        self.publish_status()?;

        // TODO: Call the user's goal_accepted callback.
        todo!("Call self.accepted_callback(goal_handle)");

        Ok(())
    }

    fn execute_cancel_request(&self) -> Result<(), RclrsError> {
        todo!()
    }

    fn execute_result_request(&self) -> Result<(), RclrsError> {
        todo!()
    }

    fn execute_goal_expired(&self) -> Result<(), RclrsError> {
        // We assume here that only one goal expires at a time. If not, the only consequence is
        // that we'll call rcl_action_expire_goals() more than necessary.

        // SAFETY: No preconditions
        let mut expired_goal = unsafe { rcl_action_get_zero_initialized_goal_info() };
        let mut num_expired = 1;

        loop {
            unsafe {
                // SAFETY: The action server is locked through the handle. The `expired_goal`
                // argument points to an array of one rcl_action_goal_info_t and num_expired points
                // to a `size_t`.
                rcl_action_expire_goals(&*self.handle.lock(), &mut expired_goal, 1, &mut num_expired)
            }
            .ok()?;

            if num_expired > 0 {
                // Clean up the expired goal.
                let uuid = GoalUuid(expired_goal.goal_id.uuid);
                self.goal_handles.lock().unwrap().remove(&uuid);
            } else {
                break
            }
        }

        Ok(())
    }

    fn publish_status(&self) -> Result<(), RclrsError> {
        let mut goal_statuses = DropGuard::new(
            unsafe {
                // SAFETY: No preconditions
                rcl_action_get_zero_initialized_goal_status_array()
            },
            |mut goal_statuses| unsafe {
                // SAFETY: The goal_status array is either zero-initialized and empty or populated by
                // `rcl_action_get_goal_status_array`. In either case, it can be safely finalized.
                rcl_action_goal_status_array_fini(&mut goal_statuses);
            },
        );

        unsafe {
            // SAFETY: The action server is locked through the handle and goal_statuses is
            // zero-initialized.
            rcl_action_get_goal_status_array(&*self.handle.lock(), &mut *goal_statuses)
        }
        .ok()?;

        unsafe {
            // SAFETY: The action server is locked through the handle and goal_statuses.msg is a
            // valid `action_msgs__msg__GoalStatusArray` by construction.
            rcl_action_publish_status(
                &*self.handle.lock(),
                &goal_statuses.msg as *const _ as *const std::ffi::c_void,
            )
        }
        .ok()
    }

    pub(crate) fn publish_feedback(&self, goal_id: &GoalUuid, feedback: &<T as rosidl_runtime_rs::Action>::Feedback) -> Result<(), RclrsError> {
        let feedback_rmw = <<T as rosidl_runtime_rs::Action>::Feedback as Message>::into_rmw_message(std::borrow::Cow::Borrowed(feedback));
        let mut feedback_msg = <T as rosidl_runtime_rs::ActionImpl>::create_feedback_message(&goal_id.0, &*feedback_rmw);
        unsafe {
            // SAFETY: The action server is locked through the handle, meaning that no other
            // non-thread-safe functions can be called on it at the same time. The feedback_msg is
            // exclusively owned here, ensuring that it won't be modified during the call.
            // rcl_action_publish_feedback() guarantees that it won't modify `feedback_msg`.
            rcl_action_publish_feedback(
                &*self.handle.lock(),
                &mut feedback_msg as *mut _ as *mut std::ffi::c_void,
            )
        }
        .ok()
    }
}

impl<T> ActionServerBase for ActionServer<T>
where
    T: rosidl_runtime_rs::Action + rosidl_runtime_rs::ActionImpl,
{
    fn handle(&self) -> &ActionServerHandle {
        &self.handle
    }

    fn num_entities(&self) -> &WaitableNumEntities {
        &self.num_entities
    }

    fn execute(&self, mode: ReadyMode) -> Result<(), RclrsError> {
        match mode {
            ReadyMode::GoalRequest => self.execute_goal_request(),
            ReadyMode::CancelRequest => self.execute_cancel_request(),
            ReadyMode::ResultRequest => self.execute_result_request(),
            ReadyMode::GoalExpired => self.execute_goal_expired(),
        }
    }
}
