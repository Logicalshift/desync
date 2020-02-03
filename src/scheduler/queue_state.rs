///
/// Represents the state of a job queue
///
#[derive(PartialEq, Debug, Clone, Copy)]
pub (super) enum QueueState {
    /// Queue is currently not running and not ready to run
    Idle,

    /// Queue has been queued up to run but isn't running yet
    Pending,

    /// Queue has been assigned to a thread and is currently running
    Running,

    /// A job on the queue has indicated that it's waiting to be re-awakened (by the scheduler)
    WaitingForWake,

    /// The queue is running synchronously on a thread and is waiting to be unparked
    WaitingForUnpark,

    /// We've returned from a polling operation and are waiting to be resumed
    WaitingForPoll,

    /// A wake-up call was made while the queue was in the running state
    AwokenWhileRunning,

    /// Queue received a panic and is no longer able to be scheduled
    Panicked
}

impl QueueState {
    ///
    /// Indicates if this queue is in the running state
    ///
    pub (crate) fn is_running(&self) -> bool {
        match self {
            QueueState::Running             | 
            QueueState::AwokenWhileRunning  | 
            QueueState::WaitingForUnpark    => true,
            _other                          => false
        }
    }
}
