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
//! use futures::future;
//! use futures::channel::mpsc;
//! use futures::executor;
//! use futures::prelude::*;
//! # use ::desync::*;
//! 
//! executor::block_on(async {
//!     let desync_hashset          = Arc::new(Desync::new(HashSet::new()));
//!     let (mut sender, receiver)  = mpsc::channel(5);
//! 
//!     pipe_in(Arc::clone(&desync_hashset), receiver, |hashset, value| { hashset.insert(value); future::ready(()).boxed() });
//! 
//!     sender.send("Test".to_string()).await.unwrap();
//!     sender.send("Another value".to_string()).await.unwrap();
//! # 
//! #   assert!(desync_hashset.sync(|hashset| hashset.contains(&("Test".to_string()))))
//! });
//! ```
//! 

use super::desync::*;

use futures::*;
use futures::future::{BoxFuture};
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
struct LazyDrop<Core: 'static+Send+Unpin> {
    reference: Option<Arc<Desync<Core>>>
}

impl<Core: 'static+Send+Unpin> LazyDrop<Core> {
    pub fn new(reference: Arc<Desync<Core>>) -> LazyDrop<Core> {
        LazyDrop {
            reference: Some(reference)
        }
    }
}

impl<Core: 'static+Send+Unpin> Deref for LazyDrop<Core> {
    type Target = Desync<Core>;

    fn deref(&self) -> &Desync<Core> {
        &*(self.reference.as_ref().unwrap())
    }
}

impl<Core: 'static+Send+Unpin> Drop for LazyDrop<Core> {
    fn drop(&mut self) {
        // Drop the reference down the chute (this ensures that if the Arc<Desync<X>> is freed, it won't block the monitor pipe when the contained Desync synchronises during drop)
        let reference = self.reference.take();
        REFERENCE_CHUTE.desync(move |_| mem::drop(reference));
    }
}

///
/// Futures notifier used to wake up a pipe when a stream or future notifies
///
struct PipeContext<Core, PollFn>
where   Core: Send+Unpin {
    /// The desync target that will be woken when the stream notifies that it needs to be polled
    ///   We keep a weak reference so that if the stream/future is all that's left referencing the
    ///   desync, it's thrown away
    target: Weak<Desync<Core>>,

    /// The function that should be called to poll this stream. Returns false if we should no longer keep polling the stream
    poll_fn: Arc<Mutex<Option<PollFn>>>
}

impl<Core, PollFn> PipeContext<Core, PollFn>
where   Core:   'static+Send+Unpin,
        PollFn: 'static+Send+FnMut(&mut Core, Context) -> bool {
    ///
    /// Creates a new pipe context, ready to poll
    ///
    fn new(target: &Arc<Desync<Core>>, poll_fn: PollFn) -> Arc<PipeContext<Core, PollFn>> {
        let context = PipeContext {
            target:     Arc::downgrade(target),
            poll_fn:    Arc::new(Mutex::new(Some(poll_fn)))
        };

        Arc::new(context)
    }

    ///
    /// Triggers the poll function in the context of the target desync
    ///
    fn poll(arc_self: Arc<Self>) {
        // If the desync is stil live...
        if let Some(target) = arc_self.target.upgrade() {
            // Grab the poll function
            let maybe_poll_fn = Arc::clone(&arc_self.poll_fn);

            // Schedule a polling operation on the desync
            target.desync(move |core| {
                // Create a futures context from the context reference
                let waker   = task::waker_ref(&arc_self);
                let context = Context::from_waker(&waker);

                // Pass in to the poll function
                let mut maybe_poll_fn   = maybe_poll_fn.lock().unwrap();

                if let Some(poll_fn) = &mut *maybe_poll_fn {
                    let keep_polling    = (poll_fn)(core, context);
                    if !keep_polling {
                        // Deallocate the function when it's time to stop polling altogether
                        *maybe_poll_fn = None;
                    }
                }
            })
        } else {
            // Stream has woken up but the desync is no longer listening
            (*arc_self.poll_fn.lock().unwrap()) = None;
        }
    }
}

impl<Core, PollFn> task::ArcWake for PipeContext<Core, PollFn>
where   Core:   'static+Send+Unpin,
        PollFn: 'static+Send+FnMut(&mut Core, Context) -> bool {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        Self::poll(Arc::clone(arc_self));
    }

    fn wake(self: Arc<Self>) {
        Self::poll(self);
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
where   Core:       'static+Send+Unpin,
        S:          'static+Send+Unpin+Stream,
        S::Item:    Send,
        ProcessFn:  'static+Send+for<'a> FnMut(&'a mut Core, S::Item) -> BoxFuture<'a, ()> {

    let mut stream  = stream;
    let mut process = process;

    // The context is used to trigger polling of the stream
    let context     = PipeContext::new(&desync, move |core, context| {
        let mut context = context;

        loop {
            // Poll the stream
            let next = stream.poll_next_unpin(&mut context);

            match next {
                // Wait for notification when the stream goes pending
                Poll::Pending       => return true,

                // Stop polling when the stream stops generating new events
                Poll::Ready(None)   => return false,

                // Invoke the callback when there's some data on the stream
                Poll::Ready(Some(next)) => {
                    process(core, next);
                }
            }
        }
    });

    // Trigger the initial poll
    PipeContext::poll(context);
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
/// use futures::prelude::*;
/// use futures::future;
/// use futures::channel::mpsc;
/// use futures::executor;
/// # use ::desync::*;
/// 
/// executor::block_on(async {
///     let desync_hashset          = Arc::new(Desync::new(HashSet::new()));
///     let (mut sender, receiver)  = mpsc::channel::<String>(5);
/// 
///     let mut value_inserted      = pipe(Arc::clone(&desync_hashset), receiver, 
///         |hashset, value| { future::ready((value.clone(), hashset.insert(value))).boxed() });
/// 
///     sender.send("Test".to_string()).await.unwrap();
///     sender.send("Another value".to_string()).await.unwrap();
///     sender.send("Test".to_string()).await.unwrap();
/// 
///     assert!(value_inserted.next().await == Some(("Test".to_string(), true)));
///     assert!(value_inserted.next().await == Some(("Another value".to_string(), true)));
///     assert!(value_inserted.next().await == Some(("Test".to_string(), false)));
/// });
/// ```
/// 
pub fn pipe<Core, S, Output, ProcessFn>(desync: Arc<Desync<Core>>, stream: S, process: ProcessFn) -> PipeStream<Output>
where   Core:       'static+Send+Unpin,
        S:          'static+Send+Unpin+Stream,
        S::Item:    Send,
        Output:     'static+Send,
        ProcessFn:  'static+Send+for <'a> FnMut(&'a mut Core, S::Item) -> BoxFuture<'a, Output> {

    // Prepare the streams
    let mut input_stream    = stream;
    let mut process         = process;
    let output_stream       = PipeStream::<Output>::new();

    // Get the core from the output stream
    let stream_core     = Arc::clone(&output_stream.core);
    let stream_core     = Arc::downgrade(&stream_core);

    // Create the read context
    let context             = PipeContext::new(&desync, move |core, context| {
        let mut context = context;

        if let Some(stream_core) = stream_core.upgrade() {
            // Defer processing if the stream core is full
            {
                // Fetch the core
                let mut stream_core = stream_core.lock().unwrap();

                // If the pending queue is full, then stop processing events
                if stream_core.pending.len() >= stream_core.max_pipe_depth {
                    // Wake when the stream accepts some input
                    stream_core.backpressure_release_notify = Some(context.waker().clone());

                    // Go back to sleep without reading from the stream
                    return true;
                }

                // If the core is closed, finish up
                if stream_core.closed {
                    return false;
                }
            }

            loop {
                // Poll the stream
                let next = stream.poll_next_unpin(&mut context);

                match next {
                    // Wait for notification when the stream goes pending
                    Poll::Pending       => return true,

                    // Stop polling when the stream stops generating new events
                    Poll::Ready(None)   => return false,

                    // Invoke the callback when there's some data on the stream
                    Poll::Ready(Some(next)) => {
                        // Pipe the next item through
                        let next_item = process(core, next);

                        // Send to the pipe stream, and wake it up
                        let notify = {
                            let mut stream_core = stream_core.lock().unwrap();

                            stream_core.pending.push_back(next_item);
                            stream_core.notify.take()
                        };
                        notify.map(|notify| notify.wake());
                    }
                }
            }
        } else {
            // The stream core has been released
            return false;
        }
    });

    output_stream

    /*
    // Fetch the input stream and prepare the process function for async calling
    let mut input_stream    = Box::new(stream);
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
                let next            = (*input_stream).poll_next_unpin(context);
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
                let _ = desync.future(move |core| {
                    // Process the next item
                    let future = {
                        let mut process     = process.lock().unwrap();
                        let process         = &mut *process;
                        process(core, next_item)
                    };

                    async move {
                        // Wait for the next item
                        let next_item = future.await;

                        // Send to the pipe stream
                        let notify = {
                            let mut stream_core = stream_core.lock().unwrap();

                            stream_core.pending.push_back(next_item);
                            stream_core.notify.take()
                        };
                        notify.map(|notify| notify.wake());

                        when_finished.wake();
                    }.boxed()
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
    */
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
struct PipeNotify {
    future: Arc<Desync<Option<BoxFuture<'static, ()>>>>
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
        let poll_fn                 = future::poll_fn(poll_fn).boxed();
        let poll_fn                 = Arc::new(Desync::new(Some(poll_fn)));

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

impl PipeNotify {
    fn poll(&self, context: &mut Context) {
        // Poll for the next result
        self.future.sync(|maybe_future| {
            // Take ownership of the future
            let mut future = maybe_future.take();

            // Poll for the next result
            match future.as_mut().map(|future| future.poll_unpin(context)) {
                // Stop if the future completes (keep the polling function so it's deallocated)
                None | Some(Poll::Ready(())) => { 
                    // Drop the future down the reference chute to avoid a potential deadlock
                    REFERENCE_CHUTE.desync(move |_| mem::drop(future));
                }

                // Wait for the next event if the future does not complete
                Some(Poll::Pending) => { *maybe_future = future }
            }
        });
    }
}

impl task::ArcWake for PipeNotify {
    fn wake_by_ref(arc_self: &Arc<Self>) {
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
