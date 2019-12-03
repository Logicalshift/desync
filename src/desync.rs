//!
//! The main `Desync` struct
//! 

use super::scheduler::*;

use std::sync::Arc;
use futures::channel::oneshot;
use futures::future::Future;

///
/// A data storage structure used to govern synchronous and asynchronous access to an underlying object.
///
pub struct Desync<T: Send> {
    /// Queue used for scheduling runtime for this object
    queue:  Arc<JobQueue>,

    /// Data for this object. Boxed so the pointer remains the same through the lifetime of the object.
    data:   Box<T>
}

// Rust actually derives this anyway at the moment
unsafe impl<T: Send> Send for Desync<T> {}

// True iff queue: Sync
unsafe impl<T: Send> Sync for Desync<T> {}

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
impl<T: 'static+Send> Desync<T> {
    ///
    /// Creates a new Desync object
    ///
    pub fn new(data: T) -> Desync<T> {
        let queue = queue();

        Desync {
            queue:  queue,
            data:   Box::new(data)
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
        unsafe {
            // As drop() is the last thing called, we know that this object will still exist at the point where the queue makes the asynchronous callback
            let data = DataRef(&*self.data);

            desync(&self.queue, move || {
                let data = data.0 as *mut T;
                job(&mut *data);
            })
        }
    }

    ///
    /// Performs an operation synchronously on this item. This will be queued with any other
    /// jobs that this item may be performing, and this function will not return until the
    /// job is complete and the result is available. 
    ///
    pub fn sync<TFn, Result>(&self, job: TFn) -> Result
    where TFn: Send+FnOnce(&mut T) -> Result, Result: Send {
        let result = unsafe {
            // As drop() is the last thing called, we know that this object will still exist at the point where the callback occurs
            let data = DataRef(&*self.data);

            sync(&self.queue, move || {
                let data = data.0 as *mut T;
                job(&mut *data)
            })
        };

        result
    }

    ///
    /// Performs an operation asynchronously on the contents of this item, returning the 
    /// result via a future.
    ///
    pub fn future<TFn, Item: 'static+Send>(&self, job: TFn) -> impl Future<Item=Item, Error=oneshot::Canceled>+Send
    where TFn: 'static+Send+FnOnce(&mut T) -> Item {
        let (send, receive) = oneshot::channel();

        self.desync(|data| {
            let result = job(data);

            if let Err(_result) = send.send(result) {
                // The listening side disconnected: we'll throw away the result and act like a normal desync instead
            }
        });

        receive
    }

    ///
    /// After the pending operations for this item are performed, waits for the
    /// supplied future to complete and then calls the specified function
    ///
    pub fn after<'a, TFn, Item: 'static+Send, Res: 'static+Send, Fut: 'a+Future<Output=Item>+Send>(&self, after: Fut, job: TFn) -> impl 'a+Future<Output=Res>+Send 
    where TFn: 'static+Send+FnOnce(&mut T, Item) -> Res {
        unsafe {
            // As drop() is the last thing called, we know that this object will still exist at the point where
            // Also, we'll have exclusive access to this object when the callback occurs
            let data = DataRef(&*self.data);

            scheduler().after(&self.queue, after, move |future_result| {
                let data = data.0 as *mut T;
                job(&mut *data, future_result)
            })
        }
    }
}

impl<T: Send> Drop for Desync<T> {
    fn drop(&mut self) {
        // Ensure that everything on the queue has committed by queueing a last synchronous event
        // (Not synchronising the queue would make this unsafe as we would hold on to a pointer to
        // the internal data structure)
        sync(&self.queue, || {});
    }
}
