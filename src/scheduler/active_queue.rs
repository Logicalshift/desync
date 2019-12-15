use super::job_queue::*;
use super::queue_state::*;

use std::thread;

///
/// Struct that holds the currently active queue and marks it as panicked if dropped during a panic
///
pub (super) struct ActiveQueue<'a> {
    pub (super) queue: &'a JobQueue
}

impl<'a> Drop for ActiveQueue<'a> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.queue.core.lock()
                .map(|mut core| core.state = QueueState::Panicked)
                .ok();
        }
    }
}
