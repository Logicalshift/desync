//!
//! The main `Desync` struct
//! 

use super::scheduler::*;

use std::pin::{Pin};
use std::sync::{Arc};
use std::marker::{Unpin};
use futures::{FutureExt};
use futures::channel::oneshot;
use futures::future::{Future, BoxFuture};

use std::mem;
use std::result::{Result};

///
/// A data storage structure used to govern synchronous and asynchronous access to an underlying object.
///
pub struct Desync<T: Send+Unpin> {
    /// Queue used for scheduling runtime for this object
    queue:  Arc<JobQueue>,

    /// Data for this object. Boxed so the pointer remains the same through the lifetime of the object.
    /// Will be 'None' only briefly when the data has been taken to be dropped
    data:   Option<Pin<Box<T>>>
}

// Rust actually derives this anyway at the moment
unsafe impl<T: Send+Unpin> Send for Desync<T> {}

// True iff queue: Sync
unsafe impl<T: Send+Unpin> Sync for Desync<T> {}

///
/// Used for passing the data pointer through to the queue
/// 
/// 'Safe' because the queue is synchronised during drop, so we can never use the pointer
/// if the object does not exist.
/// 
struct DataRef<T: Send>(*const T);
unsafe impl<T: Send> Send for DataRef<T> {}

// TODO: we can change DataRef to Shared (https://doc.rust-lang.org/std/ptr/struct.Shared.html in the future)

// TODO: T does not need to be static as we know that its lifetime is at least the lifetime of Desync<T> and hence the queue
impl<T: 'static+Send+Unpin> Desync<T> {
    ///
    /// Creates a new Desync object
    ///
    pub fn new(data: T) -> Desync<T> {
        let queue = queue();

        Desync {
            queue:  queue,
            data:   Some(Pin::new(Box::new(data)))
        }
    }

    ///
    /// Performs an operation asynchronously on this item. This function will return
    /// immediately and the job will happen on a separate thread at some time in the
    /// future (generally fairly soon).
    /// 
    /// Jobs are always performed in the order that they are queued and are always
    /// performed synchronously with respect to this object.
    ///
    #[inline]
    #[deprecated(since="0.3.0", note="please use `desync` instead")]
    pub fn r#async<TFn>(&self, job: TFn)
    where TFn: 'static+Send+FnOnce(&mut T) -> () {
        self.desync(job)
    }

    ///
    /// Performs an operation asynchronously on this item. This function will return
    /// immediately and the job will happen on a separate thread at some time in the
    /// future (generally fairly soon).
    /// 
    /// Jobs are always performed in the order that they are queued and are always
    /// performed synchronously with respect to this object.
    ///
    pub fn desync<TFn>(&self, job: TFn)
    where TFn: 'static+Send+FnOnce(&mut T) -> () {
        // As drop() is the last thing called, we know that this object will still exist at the point where the queue makes the asynchronous callback
        let data = DataRef::<T>(&**self.data.as_ref().unwrap());

        desync(&self.queue, move || {
            let data = data.0 as *mut T;
            job(unsafe { &mut *data });
        })
    }

    ///
    /// Performs an operation synchronously on this item. This will be queued with any other
    /// jobs that this item may be performing, and this function will not return until the
    /// job is complete and the result is available. 
    ///
    pub fn sync<TFn, Result>(&self, job: TFn) -> Result
    where TFn: Send+FnOnce(&mut T) -> Result, Result: Send {
        let result = {
            // As drop() is the last thing called, we know that this object will still exist at the point where the callback occurs
            // Exclusivity is guaranteed because the queue executes only one task at a time
            let data = DataRef::<T>(&**self.data.as_ref().unwrap());

            sync(&self.queue, move || {
                let data = data.0 as *mut T;
                job(unsafe { &mut *data })
            })
        };

        result
    }

    ///
    /// Performs an operation synchronously on this item, 
    ///
    pub fn try_sync<TFn, FnResult>(&self, job: TFn) -> Result<FnResult, TrySyncError>
    where TFn: Send+FnOnce(&mut T) -> FnResult, FnResult: Send {
        let result = {
            // As drop() is the last thing called, we know that this object will still exist at the point where the callback occurs
            let data = DataRef::<T>(&**self.data.as_ref().unwrap());

            try_sync(&self.queue, move || {
                let data = data.0 as *mut T;
                job(unsafe { &mut *data })
            })
        };

        result
    }

    ///
    /// Deprecated name for `future_desync`
    ///
    #[inline]
    #[deprecated(since="0.7.0", note="please use either `future_desync` or `future_sync` to schedule futures")]
    pub fn future<TFn, TOutput>(&self, job: TFn) -> impl Future<Output=Result<TOutput, oneshot::Canceled>>+Send
    where   TFn:        'static+Send+for<'a> FnOnce(&'a mut T) -> BoxFuture<'a, TOutput>,
            TOutput:    'static+Send {
        self.future_desync(job)
    }

    ///
    /// Performs an operation asynchronously on the contents of this item, returning the 
    /// result via a future.
    ///
    /// The future will be scheduled in the background, so it will make progress even if the current scheduler is
    /// blocked for any reason. Additionally, it's not necessary to await the returned future, which can be discarded
    /// if necessary.
    /// 
    /// The future returned is a `BoxFuture`, which you can create using `.boxed()` or `Box::pin()` on a future. This is 
    /// solely to work around a limitation in Rust's type system (it's not presently possible to introduce the lifetime 
    /// from for<'a> into the return type of a function)
    ///
    pub fn future_desync<'a, TFn, TFuture>(&self, job: TFn) -> SchedulerFuture<TFuture::Output>
    where
        TFn:                'static + Send + FnOnce(&'a mut T) -> TFuture,
        TFuture:            'static + 'a + Send + Future,
        TFuture::Output:    'static + Send,
    {
        // The future will have a lifetime shorter than the lifetime of this structure, and exclusivity is guaranteed
        // because queues only execute one task at a time
        let data = DataRef::<T>(&**self.data.as_ref().unwrap());

        scheduler().future_desync(&self.queue, move || {
            let data        = data.0 as *mut T;
            let job         = job(unsafe { &mut *data });

            async {
                job.await
            }
        })
    }

    ///
    /// Performs an operation asynchronously on the contents of this item, returning a future that must be awaited
    /// before it is dropped.
    ///
    /// The future will be scheduled in the current execution context, so it will only make progress if the current
    /// scheduler is running. The task will be cancelled and will not complete execution if the future is dropped before
    /// completion, so it's usually necessary to await the result of this function for the task to behave correctly.
    /// 
    /// The future returned is a `BoxFuture`, which you can create using `.boxed()` or `Box::pin()` on a future. This is 
    /// solely to work around a limitation in Rust's type system (it's not presently possible to introduce the lifetime 
    /// from for<'a> into the return type of a function)
    ///
    pub fn future_sync<'a, 'b, TFn, TFuture>(&'a self, job: TFn) -> impl 'a + Future<Output=Result<TFuture::Output, oneshot::Canceled>> + Send
    where
        TFn:                'a + Send + FnOnce(&'b mut T) -> TFuture,
        TFuture:            'a + 'b + Send + Future,
        TFuture::Output:    'a + Send,
    {
        // The future will have a lifetime shorter than the lifetime of this structure
        let data = DataRef::<T>(&**self.data.as_ref().unwrap());

        scheduler().future_sync(&self.queue, move || {
            let data        = data.0 as *mut T;
            let job         = job(unsafe { &mut *data });

            async {
                job.await
            }
        })
    }

    ///
    /// After the pending operations for this item are performed, waits for the
    /// supplied future to complete and then calls the specified function
    ///
    pub fn after<'a, TFn, Res, Fut>(&self, after: Fut, job: TFn) -> impl 'static + Future<Output=Result<Res, oneshot::Canceled>> + Send 
    where 
        Res: 'static + Send,
        Fut: 'static + Future + Send,
        TFn: 'static + Send + FnOnce(&mut T, Fut::Output) -> Res 
    {
        self.future_desync(move |data| {
            async move {
                let future_result = after.await;
                job(data, future_result)
            }.boxed()
        })
    }
}

impl<T: Send+Unpin> Drop for Desync<T> {
    fn drop(&mut self) {
        use std::thread;

        // Take the data we're about to drop from the object
        let data = self.data.take();

        // Ensure that everything on the queue has committed by queueing a last synchronous event
        // (Not synchronising the queue would make this unsafe as we would hold on to a pointer to
        // the internal data structure)
        if thread::panicking() {
            // If the thread is already panicking when we're dropped, do not panic again
            scheduler().sync_no_panic(&self.queue, move || {
                mem::drop(data);
            });
        } else {
            // Thread is not panicking
            sync(&self.queue, move || {
                mem::drop(data);
            });
        }
    }
}
