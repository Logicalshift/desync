use super::scheduler::*;

use std::sync::Arc;
use futures::sync::oneshot;
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
    pub fn async<TFn>(&self, job: TFn)
    where TFn: 'static+Send+FnOnce(&mut T) -> () {
        unsafe {
            // As drop() is the last thing called, we know that this object will still exist at the point where
            let data = DataRef(&*self.data);

            async(&self.queue, move || {
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
            // As drop() is the last thing called, we know that this object will still exist at the point where
            let data = DataRef(&*self.data);

            sync(&self.queue, move || {
                let data = data.0 as *mut T;
                job(&mut *data)
            })
        };

        result
    }

    ///
    /// Performs an operation asynchronously on this thread, returning the result via a future.
    ///
    pub fn future<TFn, Item: 'static+Send>(&self, job: TFn) -> Box<Future<Item=Item, Error=oneshot::Canceled>>
    where TFn: 'static+Send+FnOnce(&mut T) -> Item {
        let (send, receive) = oneshot::channel();

        self.async(|data| {
            let result = job(data);
            send.send(result).ok();
        });

        Box::new(receive)
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

#[cfg(test)]
mod test {
    use super::*;
    use std::thread::*;
    use std::time::*;
    use super::super::scheduler::test::timeout;

    struct TestData {
        val: u32
    }

    #[test]
    fn can_retrieve_data_synchronously() {
        let desynced = Desync::new(TestData { val: 0 });

        assert!(desynced.sync(|data| data.val) == 0);
    }

    #[test]
    fn can_update_data_asynchronously() {
        let desynced = Desync::new(TestData { val: 0 });

        desynced.async(|data| {
            sleep(Duration::from_millis(100));
            data.val = 42;
        });
        
        assert!(desynced.sync(|data| data.val) == 42);
    }

    #[test]
    fn can_update_data_asynchronously_1000_times() {
        for _i in 0..1000 {
            timeout(|| {
                let desynced = Desync::new(TestData { val: 0 });

                desynced.async(|data| {
                    sleep(Duration::from_millis(1));
                    data.val = 42;
                });
                
                assert!(desynced.sync(|data| data.val) == 42);
            }, 500);
        }
    }

    #[test]
    fn can_update_data_with_future() {
        timeout(|| {
            use futures::executor;

            let desynced = Desync::new(TestData { val: 0 });

            desynced.async(|data| {
                sleep(Duration::from_millis(100));
                data.val = 42;
            });

            let mut future = executor::spawn(desynced.future(|data| {
                data.val
            }));
            
            assert!(future.wait_future().unwrap() == 42);
        }, 500);
    }

    #[test]
    fn can_update_data_with_future_1000_times() {
        // Seems to timeout fairly reliably after signalling the future
        use futures::executor;

        for _i in 0..1000 {
            timeout(|| {
                let desynced = Desync::new(TestData { val: 0 });

                desynced.async(|data| {
                    sleep(Duration::from_millis(1));
                    data.val = 42;
                });

                let mut future = executor::spawn(desynced.future(|data| data.val));
                
                assert!(future.wait_future().unwrap() == 42);
            }, 500);
        }
    }

    #[test]
    fn dropping_while_running_isnt_obviously_bad() {
        let desynced = Desync::new(TestData { val: 0 });

        desynced.async(|data| {
            sleep(Duration::from_millis(100));
            data.val = 42;
        });
        desynced.async(|data| {
            sleep(Duration::from_millis(100));
            data.val = 42;
        });
    }
}
