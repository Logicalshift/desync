use futures::prelude::*;
use futures::future::{BoxFuture};

///
/// Represents an object that can create a boxed future that lasts as long as a particular borrow operation
///
/// This is implemented on `FnOnce(&'borrow mut T) -> impl 'borrow + Future`
///
pub trait DesyncBorrowFuture<'borrow, TBorrow, TOutput> {
    ///
    /// Creates a future that can accept a borrowed value
    ///
    fn make_future_box(self, val: &'borrow mut TBorrow) -> BoxFuture<'borrow, TOutput>;
}

impl<'borrow, TFn, TBorrow, TFuture> DesyncBorrowFuture<'borrow, TBorrow, TFuture::Output> for TFn
where
    TFn:        FnOnce(&'borrow mut TBorrow) -> TFuture,
    TFuture:    'borrow + Send + Future,
    TBorrow:    'borrow,
{
    #[inline]
    fn make_future_box(self, val: &'borrow mut TBorrow) -> BoxFuture<'borrow, TFuture::Output> {
        (self)(val).boxed()
    }
}

///
/// Represents an object that can create a boxed future that lasts as long as a particular borrow operation
///
/// This is implemented on `FnOnce(&'borrow mut T) -> impl 'borrow + Future`
///
pub trait DesyncBorrowStaticFuture<'borrow, TBorrow, TOutput> {
    ///
    /// Creates a future that can accept a borrowed value
    ///
    fn make_future_box(self, val: &'borrow mut TBorrow) -> BoxFuture<'borrow, TOutput>;
}

impl<'borrow, TFn, TBorrow, TOutput> DesyncBorrowStaticFuture<'borrow, TBorrow, TOutput> for TFn
where
    TFn:        'static + FnOnce(&'borrow mut TBorrow) -> BoxFuture<'borrow, TOutput>,
    TOutput:    'static + Send,
    TBorrow:    'static,
{
    #[inline]
    fn make_future_box(self, val: &'borrow mut TBorrow) -> BoxFuture<'borrow, TOutput> {
        (self)(val)
    }
}
