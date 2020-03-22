use std::sync::atomic::{AtomicU64, Ordering};

lazy_static! {
    static ref NEXT_FUTURE_ID: AtomicU64 = AtomicU64::new(0);
}

///
/// ID of a future used in a state
///
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub (super) struct FutureId(pub u64);

impl FutureId {
    ///
    /// Creates a new unique future ID
    ///
    pub fn new() -> FutureId {
        let next_id = NEXT_FUTURE_ID.fetch_add(1, Ordering::Relaxed);

        FutureId(next_id)
    }
}

///
/// Represents the state of a job queue
///
#[derive(PartialEq, Debug, Clone, Copy)]
pub (super) enum QueueState {
    /// Queue is currently not running and not ready to run
    /// 
    /// The queue has this state when it has no jobs in it.
    Idle,

    /// Queue has been queued up to run but isn't running yet
    /// 
    /// In general, this means the queue is waiting for a background thread to run on.
    Pending,

    /// Queue has been assigned to a thread and is currently running
    /// 
    /// The queue must not be scheduled anywhere else while this is going on.
    Running,

    /// A job on the queue has indicated that it's waiting to be re-awakened (by the scheduler)
    /// 
    /// This state is reached when a future for this queue generates a result and the queue moves
    /// into the pending state. It generally means it's scheduled to resume on a background
    /// thread (similar to Pending)
    WaitingForWake,

    /// The queue is running synchronously on a thread and is waiting to be unparked
    /// 
    /// This occurs when the queue has been running on a thread and needs to wait for an event to
    /// occur on some other thread, usually the completion of a future. Queues in this state must
    /// only be resumed by the appropriate unparking method (the queue is effectively 'running'
    /// on the thread where it is parked)
    /// 
    /// As the queue is only allowed to run on one thread at any one time, only one thread can be
    /// waiting to be unparked.
    WaitingForUnpark,

    /// We've returned from a polling operation and are waiting to be resumed
    /// 
    /// This happens when a queue has scheduled itself onto a thread as a result of calling the
    /// poll() method on a future and has returned pending. This queue should be resumed when the
    /// future is next awoken by calling 'poll' (it's effectively 'running' in the context of the
    /// call too poll)
    /// 
    /// Only the future that moved the queue into this state can re-awaken the queue (ie, any other
    /// futures must wait for this one to complete first). As the queue can only run on a single
    /// thread at any one time, only a single future is allowed to put the queue into this state.
    WaitingForPoll(FutureId),

    /// A wake-up call was made while the queue was in the running state
    /// 
    /// When attempting to suspend a queue as the result of a future indicating that it is pending,
    /// it is possible for the future to wake the queue before it has reached the suspended state.
    /// We capture this event by setting the queue's status to 'AwokenWhileRunning'.
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
            QueueState::WaitingForPoll(_)   |
            QueueState::WaitingForUnpark    => true,
            _other                          => false
        }
    }
}
