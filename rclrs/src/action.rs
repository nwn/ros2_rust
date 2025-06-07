use crate::{rcl_bindings::*, Node, RclrsError};
use std::sync::Arc;

// SAFETY: The functions accessing this type, including drop(), shouldn't care about the thread
// they are running in. Therefore, this type can be safely sent to another thread.
unsafe impl Send for rcl_action_goal_handle_t {}

unsafe impl Sync for rcl_action_goal_handle_t {}

use std::marker::PhantomData;

pub type GoalUUID = [u8; RCL_ACTION_UUID_SIZE];

pub enum GoalResponse {
    Reject = 1,
    AcceptAndExecute = 2,
    AcceptAndDefer = 3,
}

pub enum CancelResponse {
    Reject = 1,
    Accept = 2,
}

pub struct ActionClient<T>
where
    T: rosidl_runtime_rs::Action,
{
    _marker: PhantomData<T>,
}

impl<T> ActionClient<T>
where
    T: rosidl_runtime_rs::Action,
{
    /// Creates a new action client.
    pub(crate) fn new(node: &Node, topic: &str) -> Result<Self, RclrsError>
    where
        T: rosidl_runtime_rs::Action,
    {
        Ok(Self {
            _marker: Default::default(),
        })
    }
}

pub struct ActionServer<T>
where
    T: rosidl_runtime_rs::Action,
{
    _marker: PhantomData<T>,
}

impl<T> ActionServer<T>
where
    T: rosidl_runtime_rs::Action,
{
    /// Creates a new action server.
    pub(crate) fn new(node: &Node, topic: &str) -> Result<Self, RclrsError>
    where
        T: rosidl_runtime_rs::Action,
    {
        Ok(Self {
            _marker: Default::default(),
        })
    }
}

pub struct ServerGoalHandle<T>
where
    T: rosidl_runtime_rs::Action,
{
    rcl_handle: Arc<rcl_action_goal_handle_t>,
    _marker: PhantomData<T>,
}

impl<T> ServerGoalHandle<T>
where
    T: rosidl_runtime_rs::Action,
{
    pub(crate) fn new(rcl_handle: Arc<rcl_action_goal_handle_t>) {}

    pub(crate) fn is_canceling(&self) -> bool {
        false
    }

    pub(crate) fn is_active(&self) -> bool {
        false
    }

    pub(crate) fn is_executing(&self) -> bool {
        false
    }
}
