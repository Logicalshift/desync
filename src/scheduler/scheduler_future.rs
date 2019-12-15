use super::job_queue::*;

use futures::prelude::*;
use futures::channel::oneshot;
use futures::task;

use std::sync::*;
use std::pin::{Pin};

///
/// Signalling structure used to return the result of a scheduler future
///
struct SchedulerFutureResult<T> {
    /// The result of the future, or None if it has not been generated yet
    result: Option<Result<T, oneshot::Canceled>>,

    /// The waker to be called when the future is available
    waker: Option<task::Waker>
}

///
/// Wrapper that cancels the future if it is dropped before it is signalled
///
pub (super) struct SchedulerFutureSignaller<T>(Arc<Mutex<SchedulerFutureResult<T>>>);

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
    ///
    /// Creates a new scheduler future and the result needed to signal it
    ///
    pub (super) fn new(queue: &Arc<JobQueue>) -> (SchedulerFuture<T>, SchedulerFutureSignaller<T>) {
        // Create an unfinished result
        let result = SchedulerFutureResult {
            result: None,
            waker:  None
        };
        let result = Arc::new(Mutex::new(result));

        // Insert into a future
        let future = SchedulerFuture {
            queue:  Arc::clone(queue),
            result: Arc::clone(&result)
        };

        (future, SchedulerFutureSignaller(result))
    }
}

impl<T> Drop for SchedulerFutureSignaller<T> {
    fn drop(&mut self) {
        let waker = {
            let mut future_result = self.0.lock().unwrap();

            // If no result has been generated
            if future_result.result.is_none() {
                // Mark the future as canceled
                future_result.result = Some(Err(oneshot::Canceled));

                // Wake up anything that was polling it
                let waker = future_result.waker.take();
                waker
            } else {
                // Result is already set, so don't wake anything up
                None
            }
        };

        // If we need to wake the future, then do so here (note that we're outside of the lock when we do this)
        waker.map(|waker| waker.wake());
    }
}

impl<T> SchedulerFutureSignaller<T> {
    ///
    /// Signals that the result of the calculation is available
    ///
    fn signal(self, result: T) {
        let waker = {
            let mut future_result = self.0.lock().unwrap();

            // Set the result
            future_result.result = Some(Ok(result));

            // Retrieve the waker
            future_result.waker.take()
        };

        // If we retrieved a waker from the result, wake it up
        waker.map(|waker| waker.wake());
    }
}
impl<T> Future for SchedulerFuture<T> {
    type Output = Result<T, oneshot::Canceled>;

    ///
    /// Polls this future
    ///
    fn poll(self: Pin<&mut Self>, context: &mut task::Context) -> task::Poll<Self::Output> {
        unimplemented!()
    }
}
