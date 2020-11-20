use super::scheduler_future::*;

use futures::prelude::*;
use futures::task;
use futures::task::{Poll};
use futures::channel::oneshot;

use std::mem;
use std::pin::*;

///
/// The state of a SyncFuture operation
///
enum SyncFutureState<TFn, TFuture>
where TFuture: Future {
    /// Waiting for the queue to start running the future
    WaitingForQueue(oneshot::Receiver<()>, TFn),

    /// Evaluating an active future for its result
    WaitingForFuture(TFuture),

    /// Finished evaluating
    Completed
}

///
/// Represents a future that runs synchronously with a queue
///
/// The future is only run in an exclusive time slot within the queue - this property is used to guarantee the
/// safety of `Desync::future_sync`, which provides exclusive access to the data store while the future is running.
/// The queue is allowed to continue once the returned future has completed.
///
pub struct SyncFuture<TFn, TFuture>
where   TFn:                Send+FnOnce() -> TFuture,
        TFuture:            Send+Future,
        TFuture::Output:    Send {
    /// The state of this future
    state: SyncFutureState<TFn, TFuture>,

    /// Tracks this future on the scheduler (this allows polling this future to invoke desync's thread-stealing semantics instead of leaving the queue scheduling to a separate thread)
    scheduler_future: SchedulerFuture<()>,

    /// Signals when the future has finished running (None if this future is completed)
    task_finished: Option<oneshot::Sender<()>>
}

impl<TFn, TFuture> SyncFuture<TFn, TFuture>
where   TFn:                Send+FnOnce() -> TFuture,
        TFuture:            Send+Future,
        TFuture::Output:    Send {
    ///
    /// Creates a new SyncFuture
    ///
    pub fn new(create_future: TFn, scheduler_future: SchedulerFuture<()>, queue_ready: oneshot::Receiver<()>, task_finished: oneshot::Sender<()>) -> SyncFuture<TFn, TFuture> {
        SyncFuture {
            state:              SyncFutureState::WaitingForQueue(queue_ready, create_future),
            scheduler_future:   scheduler_future,
            task_finished:      Some(task_finished)
        }
    }
}

impl<TFn, TFuture> Future for SyncFuture<TFn, TFuture>
where   TFn:                Unpin+Send+FnOnce() -> TFuture,
        TFuture:            Unpin+Send+Future,
        TFuture::Output:    Send {
    type Output = Result<TFuture::Output, oneshot::Canceled>;

    fn poll(mut self: Pin<&mut Self>, context: &mut task::Context) -> Poll<Self::Output> {
        use self::SyncFutureState::*;

        // Rust doesn't seem to have a way to let us update the state in-place, so we need to swap out the old state and swap in the new state
        let mut state = Completed;
        mem::swap(&mut state, &mut self.state);

        // Update the state now we own it
        state = match state {
            WaitingForQueue(recv, create_future) => {
                WaitingForQueue(recv, create_future)
            }

            WaitingForFuture(future) => {
                WaitingForFuture(future)
            }

            Completed => {
                Completed
            }
        };

        // Swap the state back into the structure
        self.state = Completed;

        unimplemented!()
    }
}
