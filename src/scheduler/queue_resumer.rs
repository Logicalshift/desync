use futures::channel::oneshot;

///
/// The queue resumer is used to resume a queue that was suspended using the `suspend()` function in the scheduler
///
pub struct QueueResumer {
    pub (super) resume: oneshot::Sender<()>
}

impl QueueResumer {
    ///
    /// Resumes a suspended queue
    ///
    pub fn resume(self) {
        // Send to the channel
        self.resume.send(()).ok();
    }
}
