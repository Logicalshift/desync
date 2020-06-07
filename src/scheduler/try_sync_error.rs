use std::io::{ErrorKind};

///
/// Possible error conditions from a `try_sync()` call on the scheduler
///
#[derive(Clone, PartialEq, Debug)]
pub enum TrySyncError {
    /// The queue is busy, so the function has not been executed
    Busy
}

impl Into<ErrorKind> for TrySyncError {
    fn into(self) -> ErrorKind {
        match self {
            TrySyncError::Busy => ErrorKind::WouldBlock
        }
    }
}
