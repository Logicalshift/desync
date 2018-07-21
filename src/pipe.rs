//!
//! Desync pipes provide a way to generate and process streams via a `Desync` object
//! 

use super::desync::*;

use futures::*;
use futures::executor;
use futures::executor::Spawn;

use std::sync::*;
use std::result::Result;

lazy_static! {
    /// The shared queue where we monitor for updates to the active pipe streams
    static ref PIPE_MONITOR: PipeMonitor = PipeMonitor::new();
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
    PIPE_MONITOR.monitor(move || {
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
                    desync.sync(move |core| {
                        let mut process = process.lock().unwrap();
                        let process     = &mut *process;
                        process(core, Ok(next));
                    });
                },

                // Stream returned an error
                Err(e) => {
                    // Process the error on the stream
                    desync.sync(move |core| {
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
/// Pipes a stream into this object. Whenever an item becomes available on the stream, the
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
/// The main polling component for that implements the stream pipes
/// 
struct PipeMonitor {
}

///
/// Provides the 'Notify' interface for a polling function with a particular ID
/// 
struct PipeNotify<PollFn: Send> {
    next_poll: Arc<Desync<Option<Spawn<PollFn>>>>
}

impl PipeMonitor {
    ///
    /// Creates a new poll thread
    /// 
    pub fn new() -> PipeMonitor {
        PipeMonitor {
        }
    }

    ///
    /// Performs a polling operation on a poll
    /// 
    fn poll<PollFn>(this_poll: &mut Option<Spawn<PollFn>>, next_poll: Arc<Desync<Option<Spawn<PollFn>>>>)
    where PollFn: 'static+Send+Future<Item=(), Error=()> {
        // If the polling function exists...
        if let Some(mut poll) = this_poll.take() {
            // Create a notification
            let notify = PipeNotify {
                next_poll: next_poll
            };
            let notify = Arc::new(notify);

            // Poll the function
            let poll_result = poll.poll_future_notify(&notify, 0);

            // Keep the polling function alive if it has not finished yet
            if poll_result != Ok(Async::Ready(())) {
                // The take() call means that the polling won't continue unless we pass it forward like this
                *this_poll = Some(poll);
            }
        }
    }

    ///
    /// Adds a polling function to the current thread. It will be called using the futures
    /// notification system (ie, can call things like the stream poll function)
    /// 
    pub fn monitor<PollFn>(&self, poll_fn: PollFn)
    where PollFn: 'static+Send+FnMut() -> Poll<(), ()> {
        // Turn the polling function into a future (it will complete when monitoring is complete)
        let poll_fn     = future::poll_fn(poll_fn);

        // Spawn it with an executor
        let poll_fn     = executor::spawn(poll_fn);

        // Create a desync object for polling
        let poll_fn     = Arc::new(Desync::new(Some(poll_fn)));
        let next_poll   = Arc::clone(&poll_fn);

        // Perform the initial polling
        poll_fn.sync(move |poll_fn| Self::poll(poll_fn, next_poll));
    }
}

impl<PollFn> executor::Notify for PipeNotify<PollFn>
where PollFn: 'static+Send+Future<Item=(), Error=()> {
    fn notify(&self, _id: usize) {
        // Poll the future whenever we're notified
        let next_poll = Arc::clone(&self.next_poll);
        self.next_poll.async(move |poll_fn| PipeMonitor::poll(poll_fn, next_poll));
    }
}
