use super::job_queue::*;

use futures::prelude::*;
use futures::channel::oneshot;
use futures::task;

use std::mem;
use std::sync::*;
use std::pin::{Pin};

///
/// The possible states of a future result
///
enum FutureResultState<T> {
    None,
    Some(T),
    ReturnedViaFuture
}

///
/// Signalling structure used to return the result of a scheduler future
///
struct SchedulerFutureResult<T> {
    /// The result of the future, or None if it has not been generated yet
    result: FutureResultState<Result<T, oneshot::Canceled>>,

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
            result: FutureResultState::None,
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

impl<T> FutureResultState<T> {
    ///
    /// Returns true if no result has ever been generated for this future
    ///
    fn is_none(&self) -> bool {
        match self {
            FutureResultState::None                 => true,
            FutureResultState::Some(_)              => false,
            FutureResultState::ReturnedViaFuture    => false
        }
    }

    ///
    /// If a result has been generated by this future and has not previously been taken, returns it
    ///
    fn take(&mut self) -> Option<T> {
        // Move the value out of this object
        let mut new_value = FutureResultState::ReturnedViaFuture;
        mem::swap(self, &mut new_value);

        // Return it if it contains a value
        match new_value {
            FutureResultState::None                 => { *self = FutureResultState::None; None },
            FutureResultState::ReturnedViaFuture    => { *self = FutureResultState::ReturnedViaFuture; None },
            FutureResultState::Some(value)          => { Some(value) }
        }
    }
}

impl<T> Drop for SchedulerFutureSignaller<T> {
    fn drop(&mut self) {
        let waker = {
            let mut future_result = self.0.lock().unwrap();

            // If no result has been generated, then mark the future as canceled
            if future_result.result.is_none() {
                // Mark the future as canceled
                future_result.result = FutureResultState::Some(Err(oneshot::Canceled));

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
    pub (super) fn signal(self, result: T) {
        let waker = {
            let mut future_result = self.0.lock().unwrap();

            // Set the result
            future_result.result = FutureResultState::Some(Ok(result));

            // Retrieve the waker
            future_result.waker.take()
        };

        // If we retrieved a waker from the result, wake it up
        waker.map(|waker| waker.wake());
    }
}

///
/// Possible actions we can take based on the state of the signal for the future
///
enum SchedulerAction<T> {
    /// Queue is running elsewhere: we should wait for it to reach the point where the future's result is available
    WaitForCompletion,

    /// Future has already completed: we should return the value to sender
    ReturnValue(T),

    /// We've claimed the 'running' state of the queue and should drain it
    DrainQueue
}

impl<T> Future for SchedulerFuture<T> {
    type Output = Result<T, oneshot::Canceled>;

    ///
    /// Polls this future
    ///
    fn poll(self: Pin<&mut Self>, context: &mut task::Context) -> task::Poll<Self::Output> {
        // Lock the result and determine which action to take
        let next_action = {
            let mut future_result = self.result.lock().unwrap();

            if let Some(result) = future_result.result.take() {
                // The result is available: we should return it immediately
                SchedulerAction::ReturnValue(result)
            } else {
                // Wake us up when the future is available
                future_result.waker = Some(context.waker().clone());

                SchedulerAction::WaitForCompletion
            }
        };

        match next_action {
            SchedulerAction::WaitForCompletion  => task::Poll::Pending,
            SchedulerAction::ReturnValue(value) => task::Poll::Ready(value),
            SchedulerAction::DrainQueue         => unimplemented!()
        }
    }
}
