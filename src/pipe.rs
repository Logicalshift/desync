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
struct PollThread {
}

///
/// Provides the 'Notify' interface for a polling function with a particular ID
/// 
struct PollNotify {
}

impl PollThread {
    ///
    /// Creates a new poll thread
    /// 
    pub fn new() -> PollThread {
        PollThread {

        }
    }

    ///
    /// Adds a polling function to the current thread. It will be called using the futures
    /// notification system (ie, can call things like the stream poll function)
    /// 
    pub fn monitor<PollFn>(&self, poll_fn: PollFn)
    where PollFn: 'static+Send+FnMut() -> Poll<(), ()> {

    }
}

impl executor::Notify for PollNotify {
    fn notify(&self, _id: usize) {
    }
}
