use super::job::*;

use futures::task::{Context, Poll};
use std::mem;

///
/// The unsafe job does not manage the lifetime of its TFn
///
pub struct UnsafeJob {
    // TODO: this can possibly become Shared<> once that API stabilises
    action: *mut dyn ScheduledJob
}

impl UnsafeJob {
    ///
    /// Creates an unsafe job. The referenced object should last as long as the job does
    ///
    pub unsafe fn new<'a>(action: &'a mut dyn ScheduledJob) -> UnsafeJob {
        let action_ptr: *mut dyn ScheduledJob = action;

        // Transmute to remove the lifetime parameter :-/
        // (We're safe provided this job is executed before the reference goes away)
        UnsafeJob { action: mem::transmute(action_ptr) }
    }
}
unsafe impl Send for UnsafeJob {}

impl ScheduledJob for UnsafeJob {
    fn run(&mut self, context: &mut Context) -> Poll<()> {
        unsafe {
            (*self.action).run(context)
        }
    }
}
