//!
//! Desync pipes provide a way to generate and process streams via a `Desync` object
//! 
//! Pipes are an excellent way to interface `Desync` objects and the futures library. Piping
//! a stream into a `Desync` object is equivalent to spawning it with an executor, except
//! without the need to dedicate a thread to running it.
//! 
//! There are two kinds of pipe. The `pipe_in` function creates a pipe that processes each
//! value made available from a stream on a desync object as they arrive, producing no
//! results. This is useful for cases where a `Desync` object is being used as the endpoint
//! for some data processing (for example, to insert the results of an operation into an
//! asynchronous database object).
//! 
//! The `pipe` function pipes data through an object. For every input value, it produces
//! an output value. This is good for creating streams that perform some kind of asynchronous
//! processing operation or that need to access data from inside a `Desync` object.
//! 
//! Here's an example of using `pipe_in` to store data in a `HashSet`:
//! 
//! ```
//! # extern crate futures;
//! # extern crate desync;
//! # use std::thread;
//! # use std::time::Duration;
//! # use std::collections::HashSet;
//! # use std::sync::*;
//! # 
//! use futures::sync::mpsc;
//! use futures::executor;
//! use desync::*;
//! 
//! let desync_hashset      = Arc::new(Desync::new(HashSet::new()));
//! let (sender, receiver)  = mpsc::channel(5);
//! 
//! pipe_in(Arc::clone(&desync_hashset), receiver, |hashset, value| { value.map(|value| hashset.insert(value)); });
//! 
//! let mut sender = executor::spawn(sender);
//! sender.wait_send("Test".to_string());
//! sender.wait_send("Another value".to_string());
//! # 
//! # thread::sleep(Duration::from_millis(5));
//! # assert!(desync_hashset.sync(|hashset| hashset.contains(&("Test".to_string()))))
//! ```
//! 

use super::desync::*;

use futures::*;
use futures::executor;
use futures::executor::Spawn;

use std::sync::*;
use std::result::Result;
use std::collections::VecDeque;

lazy_static! {
    /// The shared queue where we monitor for updates to the active pipe streams
    static ref PIPE_MONITOR: PipeMonitor = PipeMonitor::new();
}

/// The maximum number of items to queue on a pipe stream before we stop accepting new input
const PIPE_BACKPRESSURE_COUNT: usize = 5;

///
/// Pipes a stream into a desync object. Whenever an item becomes available on the stream, the
/// processing function is called asynchronously with the item that was received.
/// 
/// This takes a weak reference to the passed in `Desync` object, so the pipe will stop if it's
/// the only thing referencing this object.
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

    // We stop processing once the desync object is no longer used anywhere else
    let desync = Arc::downgrade(&desync);

    // Wrap the process fn up so we can call it asynchronously
    // (it doesn't really need to be in a mutex as it's only called by our object but we need to make it pass Rust's checks and we don't have a way to specify this at the moment)
    let process = Arc::new(Mutex::new(process));

    // Monitor the stream
    PIPE_MONITOR.monitor(move || {
        loop {
            let desync      = desync.upgrade();

            if let Some(desync) = desync {
                // Read the current status of the stream
                let process     = Arc::clone(&process);
                let next        = stream.poll();

                match next {
                    // Just wait if the stream is not ready
                    Ok(Async::NotReady) => { return Ok(Async::NotReady); },

                    // Stop processing when the stream is finished
                    Ok(Async::Ready(None)) => { return Ok(Async::Ready(())); }

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
                }
            } else {
                // The desync target is no longer available - indicate that we've completed monitoring
                return Ok(Async::Ready(()));
            }
        }
    });
}

///
/// Pipes a stream into this object. Whenever an item becomes available on the stream, the
/// processing function is called asynchronously with the item that was received. The
/// return value is placed onto the output stream.
/// 
/// Unlike `pipe_in`, this keeps a strong reference to the `Desync` object so the processing
/// will continue so long as the input stream has data and the output stream is not dropped.
/// 
/// The input stream will start executing and reading values immediately when this is called.
/// Dropping the output stream will cause the pipe to be closed (the input stream will be
/// dropped and no further processing will occur).
/// 
/// This example demonstrates how to create a simple demonstration pipe that takes hashset values
/// and returns a stream indicating whether or not they were already included:
/// 
/// ```
/// # extern crate futures;
/// # extern crate desync;
/// # use std::thread;
/// # use std::time::Duration;
/// # use std::collections::HashSet;
/// # use std::sync::*;
/// # 
/// use futures::sync::mpsc;
/// use futures::executor;
/// use desync::*;
/// 
/// let desync_hashset      = Arc::new(Desync::new(HashSet::new()));
/// let (sender, receiver)  = mpsc::channel::<String>(5);
/// 
/// let value_inserted = pipe(Arc::clone(&desync_hashset), receiver, 
///     |hashset, value| { value.map(|value| (value.clone(), hashset.insert(value))) });
/// 
/// let mut sender = executor::spawn(sender);
/// sender.wait_send("Test".to_string());
/// sender.wait_send("Another value".to_string());
/// sender.wait_send("Test".to_string());
/// 
/// let mut value_inserted = executor::spawn(value_inserted);
/// assert!(value_inserted.wait_stream() == Some(Ok(("Test".to_string(), true))));
/// assert!(value_inserted.wait_stream() == Some(Ok(("Another value".to_string(), true))));
/// assert!(value_inserted.wait_stream() == Some(Ok(("Test".to_string(), false))));
/// ```
/// 
pub fn pipe<Core, S, Output, OutputErr, ProcessFn>(desync: Arc<Desync<Core>>, stream: S, process: ProcessFn) -> PipeStream<Output, OutputErr>
where   Core:       'static+Send,
        S:          'static+Send+Stream,
        S::Item:    Send,
        S::Error:   Send,
        Output:     'static+Send,
        OutputErr:  'static+Send,
        ProcessFn:  'static+Send+FnMut(&mut Core, Result<S::Item, S::Error>) -> Result<Output, OutputErr> {
    
    // Fetch the input stream and prepare the process function for async calling
    let mut input_stream    = stream;
    let process             = Arc::new(Mutex::new(process));

    // Create the output stream
    let output_stream   = PipeStream::new();
    let stream_core     = Arc::clone(&output_stream.core);
    let stream_core     = Arc::downgrade(&stream_core);

    // Monitor the input stream and pass data to the output stream
    PIPE_MONITOR.monitor(move || {
        loop {
            let stream_core = stream_core.upgrade();

            if let Some(stream_core) = stream_core {
                // Defer processing if the stream core is full
                {
                    // Fetch the core
                    let mut stream_core = stream_core.lock().unwrap();

                    // If the pending queue is full, then stop processing events
                    if stream_core.pending.len() >= stream_core.max_pipe_depth {
                        // Wake when the stream accepts some input
                        stream_core.backpressure_release_notify = Some(task::current());

                        // Go back to sleep without reading from the stream
                        return Ok(Async::NotReady);
                    }
                }

                // Read the current status of the stream
                let process         = Arc::clone(&process);
                let next            = input_stream.poll();
                let mut next_item;

                // Work out what the next item to pass to the process function should be
                match next {
                    // Just wait if the stream is not ready
                    Ok(Async::NotReady) => { return Ok(Async::NotReady); },

                    // Stop processing when the input stream is finished
                    Ok(Async::Ready(None)) => { 
                        // Mark the target stream as closed
                        let mut stream_core = stream_core.lock().unwrap();
                        stream_core.closed = true;
                        stream_core.notify.take().map(|notify| notify.notify());

                        // Pipe has finished
                        return Ok(Async::Ready(()));
                    }

                    // Stream returned a value
                    Ok(Async::Ready(Some(next))) => next_item = Ok(next),

                    // Stream returned an error
                    Err(e) => next_item = Err(e),
                }

                // Send the next item to be processed
                desync.sync(move |core| {
                    // Process the next item
                    let mut process     = process.lock().unwrap();
                    let process         = &mut *process;
                    let next_item       = process(core, next_item);

                    // Send to the pipe stream
                    let mut stream_core = stream_core.lock().unwrap();

                    stream_core.pending.push_back(next_item);
                    stream_core.notify.take().map(|notify| notify.notify());
                });

            } else {
                // We stop processing once nothing is reading from the target stream
                return Ok(Async::Ready(()));
            }
        }
    });

    // The pipe stream is the result
    output_stream
}

///
/// The shared data for a pipe stream
/// 
struct PipeStreamCore<Item, Error>  {
    /// The maximum number of items we allow to be queued in this stream before producing backpressure
    max_pipe_depth: usize,

    /// The pending data for this stream
    pending: VecDeque<Result<Item, Error>>,

    /// True if the input stream has closed (the stream is closed once this is true and there are no more pending items)
    closed: bool,

    /// The task to notify when the stream changes
    notify: Option<task::Task>,

    /// The task to notify when we reduce the amount of pending data
    backpressure_release_notify: Option<task::Task>
}

///
/// A stream generated by a pipe
/// 
pub struct PipeStream<Item, Error> {
    core: Arc<Mutex<PipeStreamCore<Item, Error>>>
}

impl<Item, Error> PipeStream<Item, Error> {
    ///
    /// Creates a new, empty, pipestream
    /// 
    fn new() -> PipeStream<Item, Error> {
        PipeStream {
            core: Arc::new(Mutex::new(PipeStreamCore {
                max_pipe_depth:                 PIPE_BACKPRESSURE_COUNT,
                pending:                        VecDeque::new(),
                closed:                         false,
                notify:                         None,
                backpressure_release_notify:    None
            }))
        }
    }

    ///
    /// Sets the number of items that this pipe stream will buffer before producing backpressure
    /// 
    /// If this call is not made, this will be set to 5.
    /// 
    pub fn set_backpressure_depth(&mut self, max_depth: usize) {
        self.core.lock().unwrap().max_pipe_depth = max_depth;
    }
}

impl<Item, Error> Drop for PipeStream<Item, Error> {
    fn drop(&mut self) {
        let mut core = self.core.lock().unwrap();

        // Flush the pending queue
        core.pending = VecDeque::new();

        // TODO: wake the monitor and stop listening to the source stream
        // (Right now this will happen next time the source stream produces data)
    }
}

impl<Item, Error> Stream for PipeStream<Item, Error> {
    type Item   = Item;
    type Error  = Error;

    fn poll(&mut self) -> Poll<Option<Item>, Error> {
        // Fetch the core
        let mut core = self.core.lock().unwrap();

        if let Some(item) = core.pending.pop_front() {
            // Value waiting at the start of the stream
            core.backpressure_release_notify.take().map(|notify| notify.notify());

            match item {
                Ok(item)    => Ok(Async::Ready(Some(item))),
                Err(erm)    => Err(erm)
            }
        } else if core.closed {
            // No more data will be returned from this stream
            Ok(Async::Ready(None))
        } else {
            // Stream not ready
            core.notify = Some(task::current());

            Ok(Async::NotReady)
        }
    }
}

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
