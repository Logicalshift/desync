use std::mem;

///
/// Trait implemented by things that can be scheduled as a job
/// 
pub trait ScheduledJob : Send {
    /// Runs this particular job
    fn run(&mut self);
}

///
/// Basic job is just a FnOnce
///
pub struct Job<TFn> 
where TFn: Send+FnOnce() -> () {
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
    fn run(&mut self) {
        // Consume the action when it's run
        let mut action = None;
        mem::swap(&mut action, &mut self.action);

        if let Some(action) = action {
            action();
        } else {
            panic!("Cannot schedule an action twice");
        }
    }
}
