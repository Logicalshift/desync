use super::job::*;

use std::mem;
use futures::future::{Future, FutureObj, FutureExt};
use futures::task::{Context, Poll};

enum JobState<TFn> {
    /// Need to call the function to create the underlying future
    FutureNotCreated(TFn),

    /// The future is waiting to be evaluated
    WaitingForFuture(FutureObj<'static, ()>),

    /// The future has completed
    Completed
}

impl<TFn, TFuture> JobState<TFn>
where   TFn:        FnOnce() -> TFuture+Send,
        TFuture:    'static+Send+Future<Output=()> {
    fn take(&mut self) -> Option<FutureObj<'static, ()>> {
        // Move the value out of this object
        let mut value = JobState::Completed;
        mem::swap(self, &mut value);

        // Result depends on the current state
        match value {
            // Create the futureobj if we're in the 'not created' state, create the future
            JobState::FutureNotCreated(create_fn)   => Some(FutureObj::new(Box::new(create_fn()))),

            // Return the active future if there is one
            JobState::WaitingForFuture(future)      => Some(future),

            // The future has gone away in the completed state
            JobState::Completed                     => None
        }
    }
}

///
/// Job that evaluates a future
///
pub struct FutureJob<TFn> {
    action: JobState<TFn>
}

impl<TFn, TFuture> FutureJob<TFn>
where   TFn:        FnOnce() -> TFuture+Send,
        TFuture:    'static+Send+Future<Output=()> {
    pub fn new(create_future: TFn) -> FutureJob<TFn> {
        FutureJob { action: JobState::FutureNotCreated(create_future) }
    }
}

impl<TFn, TFuture> ScheduledJob for FutureJob<TFn>
where   TFn:        FnOnce() -> TFuture+Send,
        TFuture:    'static+Send+Future<Output=()> {
    fn run(&mut self, context: &mut Context) -> Poll<()> {
        // Consume the action when it's run
        let action = self.action.take();

        if let Some(mut action) = action {
            match action.poll_unpin(context) {
                Poll::Ready(()) => Poll::Ready(()),
                Poll::Pending   => {
                    self.action = JobState::WaitingForFuture(action);
                    Poll::Pending
                }
            }
        } else {
            panic!("Cannot schedule an action twice");
        }
    }
}
