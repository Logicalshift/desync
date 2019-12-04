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
//! # use std::collections::HashSet;
//! # use std::sync::*;
//! # 
//! use futures::sync::mpsc;
//! use futures::executor;
//! # use ::desync::*;
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
//! # assert!(desync_hashset.sync(|hashset| hashset.contains(&("Test".to_string()))))
//! ```
//! 

use super::desync::*;

use futures::*;
use futures::future::{Future};
use futures::stream::{Stream};
use futures::task;
use futures::task::{Poll, Context};

use std::mem;
use std::sync::*;
use std::pin::{Pin};
use std::ops::Deref;
use std::collections::VecDeque;

lazy_static! {
    /// The shared queue where we monitor for updates to the active pipe streams
    static ref PIPE_MONITOR: PipeMonitor = PipeMonitor::new();

    /// Desync for disposing of references used in pipes (if a pipe is closed with pending data, this avoids clearing it in the same context as the pipe monitor)
    static ref REFERENCE_CHUTE: Desync<()> = Desync::new(());
}

/// The maximum number of items to queue on a pipe stream before we stop accepting new input
const PIPE_BACKPRESSURE_COUNT: usize = 5;

/// Wraps an Arc<> that is dropped on a separate queue
struct LazyDrop<Core: 'static+Send> {
    reference: Option<Arc<Desync<Core>>>
}

impl<Core: 'static+Send> LazyDrop<Core> {
    pub fn new(reference: Arc<Desync<Core>>) -> LazyDrop<Core> {
        LazyDrop {
            reference: Some(reference)
        }
    }
}

impl<Core: 'static+Send> Deref for LazyDrop<Core> {
    type Target = Desync<Core>;

    fn deref(&self) -> &Desync<Core> {
        &*(self.reference.as_ref().unwrap())
    }
}

impl<Core: 'static+Send> Drop for LazyDrop<Core> {
    fn drop(&mut self) {
        // Drop the reference down the chute (this ensures that if the Arc<Desync<X>> is freed, it won't block the monitor pipe when the contained Desync synchronises during drop)
        let reference = self.reference.take();
        REFERENCE_CHUTE.desync(move |_| mem::drop(reference));
    }
}

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
        ProcessFn:  'static+Send+FnMut(&mut Core, S::Item) -> () {

    // Need a mutable version of the stream
    let mut stream = stream;

    // We stop processing once the desync object is no longer used anywhere else
    let desync = Arc::downgrade(&desync);

    // Wrap the process fn up so we can call it asynchronously
    // (it doesn't really need to be in a mutex as it's only called by our object but we need to make it pass Rust's checks and we don't have a way to specify this at the moment)
    let process = Arc::new(Mutex::new(process));

    // Monitor the stream
    PIPE_MONITOR.monitor(move |context| {
        loop {
            let desync = desync.upgrade();

            if let Some(desync) = desync {
                let desync      = LazyDrop::new(desync);

                // Read the current status of the stream
                let process     = Arc::clone(&process);
                let next        = stream.poll_next(context);

                match next {
                    // Just wait if the stream is not ready
                    Poll::Pending => { return Poll::Pending; },

                    // Stop processing when the stream is finished
                    Poll::Ready(None) => { return Poll::Ready(()); }

                    // Stream returned a value
                    Poll::Ready(Some(next)) => {
                        let when_ready = context.waker().clone();

                        // Process the value on the stream
                        desync.desync(move |core| {
                            {
                                let mut process = process.lock().unwrap();
                                let process     = &mut *process;
                                process(core, next);
                            }

                            when_ready.wake();
                        });

                        // Wake again when the processing finishes
                        return Poll::Pending;
                    },
                }
            } else {
                // The desync target is no longer available - indicate that we've completed monitoring
                return Poll::Ready(());
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
/// # use std::collections::HashSet;
/// # use std::sync::*;
/// # 
/// use futures::sync::mpsc;
/// use futures::executor;
/// # use ::desync::*;
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
pub fn pipe<Core, S, Output, ProcessFn>(desync: Arc<Desync<Core>>, stream: S, process: ProcessFn) -> PipeStream<Output>
where   Core:       'static+Send,
        S:          'static+Send+Stream,
        S::Item:    Send,
        Output:     'static+Send,
        ProcessFn:  'static+Send+FnMut(&mut Core, S::Item) -> Output {
    
    // Fetch the input stream and prepare the process function for async calling
    let mut input_stream    = stream;
    let process             = Arc::new(Mutex::new(process));

    // Create the output stream
    let output_stream   = PipeStream::new();
    let stream_core     = Arc::clone(&output_stream.core);
    let stream_core     = Arc::downgrade(&stream_core);

    // Monitor the input stream and pass data to the output stream
    PIPE_MONITOR.monitor(move |context| {
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
                        stream_core.backpressure_release_notify = Some(context.waker().clone());

                        // Go back to sleep without reading from the stream
                        return Poll::Pending;
                    }

                    // If the core is closed, finish up
                    if stream_core.closed {
                        return Poll::Ready(());
                    }
                }

                // Read the current status of the stream
                let process         = Arc::clone(&process);
                let next            = input_stream.poll(context);
                let next_item;

                // Work out what the next item to pass to the process function should be
                match next {
                    // Just wait if the stream is not ready
                    Poll::Pending => { return Poll::Pending; },

                    // Stop processing when the input stream is finished
                    Poll::Ready(None) => { 
                        let when_closed = context.waker().clone();

                        desync.desync(move |_core| {
                            // Mark the target stream as closed
                            let notify = {
                                let mut stream_core = stream_core.lock().unwrap();
                                stream_core.closed = true;
                                stream_core.notify.take()
                            };
                            notify.map(|notify| notify.wake());

                            when_closed.wake();
                        });

                        // Pipe has finished. We return not ready here and finish up once the closed event fires
                        return Poll::Pending;
                    }

                    // Stream returned a value
                    Poll::Ready(Some(next)) => next_item = next
                }

                // Send the next item to be processed
                let when_finished = context.waker().clone();
                desync.desync(move |core| {
                    // Process the next item
                    let mut process     = process.lock().unwrap();
                    let process         = &mut *process;
                    let next_item       = process(core, next_item);

                    // Send to the pipe stream
                    let notify = {
                        let mut stream_core = stream_core.lock().unwrap();

                        stream_core.pending.push_back(next_item);
                        stream_core.notify.take()
                    };
                    notify.map(|notify| notify.wake());

                    when_finished.wake();
                });

                // Poll again when the task is complete
                return Poll::Pending;

            } else {
                // We stop processing once nothing is reading from the target stream
                return Poll::Ready(());
            }
        }
    });

    // The pipe stream is the result
    output_stream
}

///
/// The shared data for a pipe stream
/// 
struct PipeStreamCore<Item>  {
    /// The maximum number of items we allow to be queued in this stream before producing backpressure
    max_pipe_depth: usize,

    /// The pending data for this stream
    pending: VecDeque<Item>,

    /// True if the input stream has closed (the stream is closed once this is true and there are no more pending items)
    closed: bool,

    /// The task to notify when the stream changes
    notify: Option<task::Waker>,

    /// The task to notify when we reduce the amount of pending data
    backpressure_release_notify: Option<task::Waker>
}

///
/// A stream generated by a pipe
/// 
pub struct PipeStream<Item> {
    core: Arc<Mutex<PipeStreamCore<Item>>>
}

impl<Item> PipeStream<Item> {
    ///
    /// Creates a new, empty, pipestream
    /// 
    fn new() -> PipeStream<Item> {
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

impl<Item> Drop for PipeStream<Item> {
    fn drop(&mut self) {
        let mut core = self.core.lock().unwrap();

        // Flush the pending queue
        core.pending = VecDeque::new();

        // TODO: wake the monitor and stop listening to the source stream
        // (Right now this will happen next time the source stream produces data)
    }
}

impl<Item> Stream for PipeStream<Item> {
    type Item   = Item;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context) -> Poll<Option<Item>> {
        let (result, notify) = {
            // Fetch the state from the core (locking the state)
            let mut core = self.core.lock().unwrap();

            if let Some(item) = core.pending.pop_front() {
                // Value waiting at the start of the stream
                let notify_backpressure = core.backpressure_release_notify.take();

                (Poll::Ready(Some(item)), notify_backpressure)
            } else if core.closed {
                // No more data will be returned from this stream
                (Poll::Ready(None), None)
            } else {
                // Stream not ready
                let notify_backpressure = core.backpressure_release_notify.take();
                core.notify = Some(context.waker().clone());

                (Poll::Pending, notify_backpressure)
            }
        };

        // If anything needs notifying, do so outside of the lock
        notify.map(|notify| notify.wake());
        result
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
struct PipeNotify<MonitorFuture: Send> {
    future: Arc<Desync<Option<MonitorFuture>>>
}

impl PipeMonitor {
    ///
    /// Creates a new pipe monitor
    /// 
    pub fn new() -> PipeMonitor {
        PipeMonitor {
        }
    }

    ///
    /// Adds a polling function to the current thread. It will be called using the futures
    /// notification system (ie, can call things like the stream poll function)
    /// 
    pub fn monitor<PollFn>(&self, poll_fn: PollFn)
    where PollFn: 'static+Send+FnMut(&mut Context) -> Poll<()> {
        // Turn the polling function into a future (it will complete when monitoring is complete)
        let poll_fn     = future::poll_fn(poll_fn);
        let poll_fn     = Arc::new(Desync::new(Some(poll_fn)));

        // Create a notifier that will act as the context for this polling operation
        let notifier    = PipeNotify {
            future: Arc::clone(&poll_fn)
        };

        // Perform the initial polling
        let notifier    = Arc::new(notifier);
        let waker       = task::waker(Arc::clone(&notifier));
        let mut context = Context::from_waker(&waker);

        notifier.poll(&mut context);
    }
}

impl<MonitorFuture> PipeNotify<MonitorFuture>
where MonitorFuture: 'static+Send+Future<Output=()> {
    fn poll(&self, context: &mut Context) {
        // Poll for the next result
        self.future.sync(|maybe_future| {
            // Take ownership of the future
            let future = maybe_future.take();

            // Poll for the next result
            match future.map(|future| future.poll(context)) {
                // Stop if the future completes (keep the polling function so it's deallocated)
                None | Some(Poll::Ready(())) => { }

                // Wait for the next event if the future does not complete
                Some(Poll::Pending) => { *maybe_future = future }
            }
        });
    }
}

impl<MonitorFuture> task::ArcWake for PipeNotify<MonitorFuture>
where MonitorFuture: 'static+Send+Future<Output=()> {
    fn wake_by_ref(arc_self: &Arc<PipeNotify<MonitorFuture>>) {
        let waker       = task::waker_ref(arc_self);
        let mut context = Context::from_waker(&waker);

        arc_self.poll(&mut context)
    }

    fn wake(self: Arc<Self>) {
        let waker       = task::waker_ref(&self);
        let mut context = Context::from_waker(&waker);

        self.poll(&mut context)
    }
}
