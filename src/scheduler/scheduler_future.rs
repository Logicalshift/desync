use super::job_queue::*;

use futures::prelude::*;
use futures::task;

use std::sync::*;
use std::pin::{Pin};

///
/// Signalling structure used to return the result of a scheduler future
///
pub (super) struct SchedulerFutureResult<T> {
    /// The result of the future, or None if it has not been generated yet
    result: Option<T>,

    /// The waker to be called when the future is available
    waker: Option<task::Waker>
}

///
/// Future representing a task pending on a scheduler
/// 
/// If polled when no threads are available, this future will run synchronously on the current thread
/// (stealing its execution time rather than blocking)
///
pub struct SchedulerFuture<T> {
    /// The queue which will eventually evaluate the result of this future
    queue: Arc<JobQueue>,

    /// A container for the result of this scheduler future
    result: Arc<Mutex<SchedulerFutureResult<T>>>
}

impl<T> SchedulerFuture<T> {
    pub (super) fn new() -> (SchedulerFuture<T>, Arc<Mutex<SchedulerFutureResult<T>>>) {

    }
}

impl<T> Future for SchedulerFuture<T> {
    type Output = T;

    ///
    /// Polls this future
    ///
    fn poll(self: Pin<&mut Self>, context: &mut task::Context) -> task::Poll<T> {
        unimplemented!()
    }
}
