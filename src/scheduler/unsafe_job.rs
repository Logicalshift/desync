use super::job::*;

use futures::task::{Context, Poll};
use std::mem;

use std::sync::*;

///
/// The unsafe job does not manage the lifetime of its TFn
///
pub struct UnsafeJob {
    // TODO: this can possibly become Shared<> once that API stabilises
    action: *mut dyn ScheduledJob,

    /// Optional condition variable signalled once the job has finished running 
    on_finish: Option<(Arc<Condvar>, Arc<Mutex<bool>>)>,
}

impl UnsafeJob {
    ///
    /// Creates an unsafe job. The referenced object should last as long as the job does
    ///
    pub unsafe fn new<'a>(action: &'a mut dyn ScheduledJob) -> UnsafeJob {
        let action_ptr: *mut dyn ScheduledJob = action;

        // Transmute to remove the lifetime parameter :-/
        // (We're safe provided this job is executed before the reference goes away)
        UnsafeJob { action: mem::transmute(action_ptr), on_finish: None }
    }

    ///
    /// Creates an unsafe job that notifies a condition variable and sets a boolean to true when it's finished. The referenced object should last as long as the job does
    ///
    pub unsafe fn new_with_notification<'a>(action: &'a mut dyn ScheduledJob, on_finish: Arc<Condvar>, is_finished: Arc<Mutex<bool>>) -> UnsafeJob {
        let action_ptr: *mut dyn ScheduledJob = action;

        // Transmute to remove the lifetime parameter :-/
        // (We're safe provided this job is executed before the reference goes away)
        UnsafeJob { action: mem::transmute(action_ptr), on_finish: Some((on_finish, is_finished)) }
    }
}
unsafe impl Send for UnsafeJob {}

impl Drop for UnsafeJob {
    #[inline]
    fn drop(&mut self) {
        if let Some((on_finish, is_finished)) = self.on_finish.take() {
            (*is_finished.lock().unwrap()) = true;
            on_finish.notify_all();
        }
    }
}

impl ScheduledJob for UnsafeJob {
    fn run(&mut self, context: &mut Context) -> Poll<()> {
        unsafe {
            (*self.action).run(context)
        }
    }
}
