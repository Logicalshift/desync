use futures::prelude::*;
use futures::task;
use futures::task::{Poll};
use futures::channel::oneshot;

use std::pin::*;

///
/// Represents a future that runs synchronously with a queue
///
/// The future is only run in an exclusive time slot within the queue - this property is used to guarantee the
/// safety of `Desync::future_sync`, which provides exclusive access to the data store while the future is running.
/// The queue is allowed to continue once the returned future has completed.
///
pub struct SyncFuture<TFn, TFuture>
where   TFn:                Send+FnOnce() -> TFuture,
        TFuture:            Send+Future,
        TFuture::Output:    Send {
    /// Callback function that starts the synchronous future running (or None if it's already started)
    create_future: Option<TFn>
}

impl<TFn, TFuture> SyncFuture<TFn, TFuture>
where   TFn:                Send+FnOnce() -> TFuture,
        TFuture:            Send+Future,
        TFuture::Output:    Send {
    ///
    /// Creates a new SyncFuture
    ///
    pub fn new(create_future: TFn) -> SyncFuture<TFn, TFuture> {
        SyncFuture {
            create_future: Some(create_future)
        }
    }
}

impl<TFn, TFuture> Future for SyncFuture<TFn, TFuture>
where   TFn:                Send+FnOnce() -> TFuture,
        TFuture:            Send+Future,
        TFuture::Output:    Send {
    type Output = Result<TFuture::Output, oneshot::Canceled>;

    fn poll(self: Pin<&mut Self>, context: &mut task::Context) -> Poll<Self::Output> {
        unimplemented!()
    }
}
