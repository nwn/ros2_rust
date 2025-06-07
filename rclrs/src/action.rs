use crate::{rcl_bindings::*, Node, RclrsError};

use std::marker::PhantomData;

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
    // This uses pub(crate) visibility to avoid instantiating this struct outside
    // [`Node::create_client`], see the struct's documentation for the rationale
    where
        T: rosidl_runtime_rs::Action,
    {
        Ok(Self {
            _marker: Default::default(),
        })
    }
}
