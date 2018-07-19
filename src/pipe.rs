//!
//! Desync pipes provide a way to generate and process streams via a `Desync` object
//! 

use super::desync::*;

use futures::*;
use futures::executor;
use futures::executor::Spawn;

use std::mem;
use std::sync::*;
use std::thread;
use std::thread::JoinHandle;
use std::result::Result;
use std::collections::{HashMap, HashSet};

lazy_static! {
    /// The shared poll thread that's used to schedule events from pipe streams
    static ref POLL_THREAD: PollThread = PollThread::new();
}

///
/// Pipes a stream into a desync object. Whenever an item becomes available on the stream, the
/// processing function is called asynchronously with the item that was received.
/// 
/// Piping a stream to a `Desync` like this will cause it to start executing: ie, this is
/// similar to calling `executor::spawn(stream)`, except that the stream will immediately
/// start draining into the `Desync` object.
/// 
pub fn pipe_in<Core, S, ProcessFn>(desync: Arc<Desync<Core>>, stream: S, process: ProcessFn)
where   Core:       'static+Send,
        S:          'static+Send+Stream,
        S::Item:    Send,
        S::Error:   Send,
        ProcessFn:  'static+Send+FnMut(&mut Core, Result<S::Item, S::Error>) -> () {

    // Need a mutable version of the stream
    let mut stream = stream;

    // Wrap the process fn up so we can call it asynchronously
    // (it doesn't really need to be in a mutex as it's only called by our object but we need to make it pass Rust's checks and we don't have a way to specify this at the moment)
    let process = Arc::new(Mutex::new(process));

    // Poll the stream on the poll thread
    POLL_THREAD.monitor(move || {
        loop {
            // Read the current status of the stream
            let process     = Arc::clone(&process);
            let next        = stream.poll();

            match next {
                // Just wait if the stream is not ready
                Ok(Async::NotReady) => { return Ok(Async::NotReady); },

                // Stream returned a value
                Ok(Async::Ready(Some(next))) => { 
                    // Process the value on the stream
                    desync.async(move |core| {
                        let mut process = process.lock().unwrap();
                        let process     = &mut *process;
                        process(core, Ok(next));
                    });
                },

                // Stream returned an error
                Err(e) => {
                    // Process the error on the stream
                    desync.async(move |core| {
                        let mut process = process.lock().unwrap();
                        let process     = &mut *process;
                        process(core, Err(e));
                    });
                },

                // Stream finished
                Ok(Async::Ready(None)) => { return Ok(Async::Ready(())); }
            }
        }
    });
}

/*
///
/// Pipes a stream into this object. Whenever an item becomes available on the strema, the
/// processing function is called asynchronously with the item that was received. The
/// return value is placed onto the output stream.
/// 
pub fn pipe<Core, S, Output, OutputErr, ProcessFn>(desync: Arc<Desync<Core>>, stream: S, process: ProcessFn) -> Box<dyn Stream<Item=Output, Error=OutputErr>> 
where   Core:       'static+Send,
        S:          'static+Send+Stream,
        S::Item:    Send,
        S::Error:   Send,
        Output:     Send,
        OutputErr:  Send,
        ProcessFn:  'static+Send+FnMut(&mut Core, Result<S::Item, S::Error>) -> Result<Output, OutputErr> {
    unimplemented!()
}
*/

///
/// In order to implement the polling functions, we need a thread that runs the executor
/// for any streams that we're currently piping (calling pipe_in or pipe will effectively
/// need to spawn the relevant stream).
/// 
/// This represents that thread. There's a bit of a limitation in that the `poll` methods 
/// for the various streams will block the thread so this may start to bottleneck at times of
/// high load or with streams with poll methods that take significant time to execute.
/// 
struct PollThread {
    /// The poll functions that are being monitored by this thread
    notifications: Arc<Mutex<PollNotifications>>,

    /// The function that should be called for every notification ID
    poll_functions: Arc<Mutex<HashMap<u32, Spawn<Box<Future<Item=(), Error=()>+Send>>>>>,

    /// The joinhandle of the running thread
    thread: Arc<JoinHandle<()>>
}

///
/// Stores things being monitored
/// 
struct PollNotifications {
    /// Next available ID for a polling function
    next_id: u32,

    /// Poll functions where the notify handle has been set
    notified_ids: HashSet<u32>
}

///
/// Provides the 'Notify' interface for a polling function with a particular ID
/// 
struct PollNotify {
    /// The ID that should be marked as notified when the callback is made
    id: u32,

    /// The structure where the notifications are stored
    notifications: Arc<Mutex<PollNotifications>>,

    /// The thread that should be notified when this notification occurs
    thread: thread::Thread
}

impl PollThread {
    ///
    /// Creates a new poll thread
    /// 
    pub fn new() -> PollThread {
        // Create the monitors for the new thread
        let notifications = PollNotifications {
            next_id:        0,
            notified_ids:   HashSet::new()  
        };
        let notifications = Arc::new(Mutex::new(notifications));

        // Create the set of polling functions
        let poll_functions = Arc::new(Mutex::new(HashMap::new()));

        // Run the thread with the monitors
        let thread = Self::run(Arc::clone(&notifications), Arc::clone(&poll_functions));
        let thread = Arc::new(thread);

        // Generate the thread object
        let thread = PollThread { 
            notifications:  notifications,
            poll_functions: poll_functions,
            thread:         thread
        };

        thread
    }

    ///
    /// Starts the poll thread running (poll threads cannot currently be stopped)
    /// 
    fn run(notifications: Arc<Mutex<PollNotifications>>, functions: Arc<Mutex<HashMap<u32, Spawn<Box<Future<Item=(), Error=()>+Send>>>>>) -> JoinHandle<()> {
        thread::spawn(move || {
            loop {
                // Park the thread until there is something to do
                thread::park();

                // Fetch the list of notified functions we should call
                let to_notify = {
                    let mut notifications   = notifications.lock().unwrap();
                    let mut to_notify       = HashSet::new();

                    mem::swap(&mut to_notify, &mut notifications.notified_ids);

                    to_notify
                };

                // Notify each function in turn
                let mut functions   = functions.lock().unwrap();
                let thread          = thread::current();

                for function_id in to_notify {
                    // Fetch the function. If it returns false, we need to remove it from the list
                    let finished_polling = functions.get_mut(&function_id)
                        .map(|poll_function| {
                            // Create the notification structure
                            let notify = PollNotify {
                                id:             function_id,
                                notifications:  Arc::clone(&notifications),
                                thread:         thread.clone()
                            };

                            // Call the polling function
                            poll_function.poll_future_notify(&Arc::new(notify), 0)
                        })
                        .unwrap_or(Ok(Async::Ready(())));
                    
                    // If the polling function completes, then remove the function
                    if finished_polling == Ok(Async::Ready(())) {
                        functions.remove(&function_id);
                    }
                }
            }
        })
    }

    ///
    /// Adds a polling function to the current thread. It will be called using the futures
    /// notification system (ie, can call things like the stream poll function)
    /// 
    pub fn monitor<PollFn>(&self, poll_fn: PollFn)
    where PollFn: 'static+Send+FnMut() -> Poll<(), ()> {
        let mut functions       = self.poll_functions.lock().unwrap();
        let mut notifications   = self.notifications.lock().unwrap();

        // Get an ID for this monitor
        let id = notifications.next_id;
        notifications.next_id += 1;

        // Turn the polling function into a future
        let poll_fn: Box<dyn Future<Item=(), Error=()>+Send> = Box::new(future::poll_fn(poll_fn));
        let poll_fn = executor::spawn(poll_fn);

        functions.insert(id, poll_fn);

        // Mark it as notified
        notifications.notified_ids.insert(id);

        // Wake the thread
        self.thread.thread().unpark();
    }
}

impl executor::Notify for PollNotify {
    fn notify(&self, _id: usize) {
        // Add our ID to the notification list
        let mut notifications = self.notifications.lock().unwrap();
        notifications.notified_ids.insert(self.id);

        // Wake the thread
        self.thread.unpark();
    }
}
