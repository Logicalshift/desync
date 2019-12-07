use futures::task::{Context, Poll};

///
/// Trait implemented by things that can be scheduled as a job
/// 
pub trait ScheduledJob : Send {
    /// Runs this particular job
    fn run(&mut self, context: &Context) -> Poll<()>;
}

///
/// Basic job is just a FnOnce
///
pub struct Job<TFn> {
    action: Option<TFn>
}

impl<TFn> Job<TFn> 
where TFn: Send+FnOnce() -> () {
    pub fn new(action: TFn) -> Job<TFn> {
        Job { action: Some(action) }
    }
}

impl<TFn> ScheduledJob for Job<TFn>
where TFn: Send+FnOnce() -> () {
    fn run(&mut self, _context: &Context) -> Poll<()> {
        // Consume the action when it's run
        let action = self.action.take();

        if let Some(action) = action {
            action();
            Poll::Ready(())
        } else {
            panic!("Cannot schedule an action twice");
        }
    }
}
