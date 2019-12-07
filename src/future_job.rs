use super::job::*;

use futures::future::{Future, FutureObj, FutureExt};
use futures::task::{Context, Poll};

///
/// Job that evaluates a future
///
pub struct FutureJob {
    action: Option<FutureObj<'static, ()>>
}

impl FutureJob {
    pub fn new<TFuture: 'static+Send+Future<Output=()>>(action: TFuture) -> FutureJob {
        FutureJob { action: Some(FutureObj::new(Box::new(action))) }
    }
}

impl ScheduledJob for FutureJob {
    fn run(&mut self, context: &mut Context) -> Poll<()> {
        // Consume the action when it's run
        let action = self.action.take();

        if let Some(mut action) = action {
            match action.poll_unpin(context) {
                Poll::Ready(()) => Poll::Ready(()),
                Poll::Pending   => {
                    self.action = Some(action);
                    Poll::Pending
                }
            }
        } else {
            panic!("Cannot schedule an action twice");
        }
    }
}
