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

    /// The sync future has completed and now we're waiting for the scheduler to complete
    WaitingForScheduler(Box<TFuture::Output>),

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
        let mut result;
        let mut state = Completed;
        mem::swap(&mut state, &mut self.state);

        // Update the state now we own it
        loop {
            // Set to true if a 'pending' result should trigger another state update (eg, if we start a new future, we should poll it)
            let mut retry = false;

            state = match state {
                WaitingForQueue(mut recv, create_future) => {
                    // Check the scheduler future (give it a chance to steal the thread)
                    if let Poll::Ready(Err(_)) = self.scheduler_future.poll_unpin(context) {
                        // The queue will never get to the point of polling this future
                        result = Poll::Ready(Err(oneshot::Canceled));
                        self.task_finished.take().map(|finished| finished.send(()));
                        Completed
                    } else {
                        // Poll the receiver
                        match recv.poll_unpin(context) {
                            Poll::Ready(Ok(())) => {
                                // Start the future
                                let mut future = create_future();

                                // Poll it immediately to determine its status
                                if let Poll::Ready(future_result) = future.poll_unpin(context) {
                                    // Future has completed
                                    result = Poll::Pending;
                                    self.task_finished.take().map(|finished| finished.send(()));

                                    retry = true;
                                    WaitingForScheduler(Box::new(future_result))
                                } else {
                                    // Future is still running
                                    result = Poll::Pending;
                                    WaitingForFuture(future)
                                }
                            }

                            Poll::Ready(Err(_)) => {
                                // Future never became ready
                                result = Poll::Ready(Err(oneshot::Canceled));
                                Completed
                            }

                            Poll::Pending => {
                                // Waiting for the queue to start this future
                                result = Poll::Pending;
                                WaitingForQueue(recv, create_future)
                            }
                        }
                    }
                }

                WaitingForFuture(mut future) => {
                    if let Poll::Ready(future_result) = future.poll_unpin(context) {
                        // Future has completed
                        result = Poll::Pending;
                        self.task_finished.take().map(|finished| finished.send(()));

                        retry = true;
                        WaitingForScheduler(Box::new(future_result))
                    } else {
                        // Future is still running
                        result = Poll::Pending;
                        WaitingForFuture(future)
                    }
                }

                WaitingForScheduler(future_result) => {
                    // Poll until the scheduler has finished running the task entirely (can deadlock while draining if we don't)
                    if let Poll::Ready(_) = self.scheduler_future.poll_unpin(context) {
                        result = Poll::Ready(Ok(*future_result));
                        Completed
                    } else {
                        result = Poll::Pending;
                        WaitingForScheduler(future_result)
                    }
                }

                Completed => {
                    // Polling after the result has been returned
                    result = Poll::Ready(Err(oneshot::Canceled));
                    Completed
                }
            };

            // Finish once retry is left at false
            if !retry {
                break;
            }
        }

        // Swap the state back into the structure
        self.state = state;

        result
    }
}
