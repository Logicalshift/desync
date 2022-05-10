use super::job::*;

use futures::task::{Context, Poll};
use std::mem;

///
/// The unsafe job does not manage the lifetime of its TFn
///
pub struct UnsafeJob {
    // TODO: this can become Shared<> once that API stabilises
    action: *const dyn ScheduledJob
}

impl UnsafeJob {
    ///
    /// Creates an unsafe job. The referenced object should last as long as the job does
    ///
    pub fn new<'a>(action: &'a dyn ScheduledJob) -> UnsafeJob {
        let action_ptr: *const dyn ScheduledJob = action;

        // Transmute to remove the lifetime parameter :-/
        // (We're safe provided this job is executed before the reference goes away)
        unsafe { UnsafeJob { action: mem::transmute(action_ptr) } }
    }
}
unsafe impl Send for UnsafeJob {}

impl ScheduledJob for UnsafeJob {
    fn run(&mut self, context: &mut Context) -> Poll<()> {
        let action = self.action as *mut dyn ScheduledJob;
        unsafe {
            (*action).run(context)
        }
    }
}
