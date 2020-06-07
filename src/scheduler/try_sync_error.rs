///
/// Possible error conditions from a `try_sync()` call on the scheduler
///
#[derive( Clone, PartialEq, Debug)]
pub enum TrySyncError {
    /// The queue is busy, so the function has not been executed
    Busy
}
