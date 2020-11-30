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

use std::sync::*;
use std::pin::{Pin};
use std::collections::VecDeque;

lazy_static! {
    /// Desync for disposing of references used in pipes (if a pipe is closed with pending data, this avoids clearing it in the same context as the pipe monitor)
    static ref REFERENCE_CHUTE: Desync<()> = Desync::new(());
}

/// The maximum number of items to queue on a pipe stream before we stop accepting new input
const PIPE_BACKPRESSURE_COUNT: usize = 5;

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
        PollFn: 'static+Send+for<'a> FnMut(&'a mut Core, Context<'a>) -> BoxFuture<'a, bool> {
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
            let _ = target.future_desync(move |core| {
                async move {
                    // Create a futures context from the context reference
                    let waker   = task::waker_ref(&arc_self);
                    let context = Context::from_waker(&waker);

                    // Pass in to the poll function
                    let future_poll = {
                        let mut maybe_poll_fn   = maybe_poll_fn.lock().unwrap();
                        let future_poll     = maybe_poll_fn.as_mut().map(|poll_fn| (poll_fn)(core, context));
                        future_poll
                    };

                    if let Some(future_poll) = future_poll {
                        let keep_polling    = future_poll.await;
                        if !keep_polling {
                            // Deallocate the function when it's time to stop polling altogether
                            (*arc_self.poll_fn.lock().unwrap()) = None;
                        }
                    }
                }.boxed()
            });
        } else {
            // Stream has woken up but the desync is no longer listening
            (*arc_self.poll_fn.lock().unwrap()) = None;
        }
    }
}

impl<Core, PollFn> task::ArcWake for PipeContext<Core, PollFn>
where   Core:   'static+Send+Unpin,
        PollFn: 'static+Send+for<'a> FnMut(&'a mut Core, Context<'a>) -> BoxFuture<'a, bool> {
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

    let stream      = Arc::new(Mutex::new(stream));
    let process     = Arc::new(Mutex::new(process));

    // The context is used to trigger polling of the stream
    let context     = PipeContext::new(&desync, move |core, context| {
        let process = Arc::clone(&process);
        let stream  = Arc::clone(&stream);

        async move {
            let mut context = context;

            loop {
                // Poll the stream
                let next = stream.lock().unwrap().poll_next_unpin(&mut context);

                match next {
                    // Wait for notification when the stream goes pending
                    Poll::Pending       => return true,

                    // Stop polling when the stream stops generating new events
                    Poll::Ready(None)   => return false,

                    // Invoke the callback when there's some data on the stream
                    Poll::Ready(Some(next)) => {
                        let process_future = (&mut *process.lock().unwrap())(core, next);
                        process_future.await;
                    }
                }
            }
        }.boxed()
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
    let input_stream        = Arc::new(Mutex::new(stream));
    let process             = Arc::new(Mutex::new(process));
    let mut output_desync   = Some(Arc::clone(&desync));
    let output_stream       = PipeStream::<Output>::new(move || {
        output_desync.take();
    });

    // Get the core from the output stream
    let stream_core     = Arc::clone(&output_stream.core);
    let stream_core     = Arc::downgrade(&stream_core);

    // Create the read context
    let context             = PipeContext::new(&desync, move |core, context| {
        let stream_core     = stream_core.upgrade();
        let mut context     = context;
        let input_stream    = Arc::clone(&input_stream);
        let process         = Arc::clone(&process);

        async move {
            if let Some(stream_core) = stream_core {
                // Defer processing if the stream core is full
                let is_closed = {
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
                    stream_core.closed
                };

                // Wake up the stream and stop reading from the stream if the core is closed
                if is_closed {
                    let notify = { stream_core.lock().unwrap().notify.take() };
                    notify.map(|notify| notify.wake());

                    return false;
                }

                loop {
                    // Disable the notifier if there was one left over
                    stream_core.lock().unwrap().notify_stream_closed = None;

                    // Poll the stream
                    let next = input_stream.lock().unwrap().poll_next_unpin(&mut context);

                    match next {
                        // Wait for notification when the stream goes pending
                        Poll::Pending       => {
                            stream_core.lock().unwrap().notify_stream_closed = Some(context.waker().clone());
                            return true
                        },

                        // Stop polling when the stream stops generating new events
                        Poll::Ready(None)   => {
                            // Mark the stream as closed, and wake it up
                            let notify = {
                                let mut stream_core = stream_core.lock().unwrap();

                                stream_core.closed = true;
                                stream_core.notify.take()
                            };
                            notify.map(|notify| notify.wake());

                            return false;
                        },

                        // Invoke the callback when there's some data on the stream
                        Poll::Ready(Some(next)) => {
                            // Pipe the next item through
                            let next_item = (&mut *process.lock().unwrap())(core, next);
                            let next_item = next_item.await;

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
        }.boxed()
    });

    // Poll the context to start the stream running
    PipeContext::poll(context);

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

    /// The task to notify when the stream changes
    notify_stream_closed: Option<task::Waker>,

    /// The task to notify when we reduce the amount of pending data
    backpressure_release_notify: Option<task::Waker>
}

///
/// A stream generated by a pipe
/// 
pub struct PipeStream<Item> {
    /// The core contains the stream state
    core: Arc<Mutex<PipeStreamCore<Item>>>,

    /// Called when dropping the output stream
    on_drop: Option<Box<dyn Send+Sync+FnMut() -> ()>>
}

impl<Item> PipeStream<Item> {
    ///
    /// Creates a new, empty, pipestream
    /// 
    fn new<FnOnDrop: 'static+Send+Sync+FnMut() -> ()>(on_drop: FnOnDrop) -> PipeStream<Item> {
        PipeStream {
            core: Arc::new(Mutex::new(PipeStreamCore {
                max_pipe_depth:                 PIPE_BACKPRESSURE_COUNT,
                pending:                        VecDeque::new(),
                closed:                         false,
                notify:                         None,
                notify_stream_closed:           None,
                backpressure_release_notify:    None
            })),

            on_drop: Some(Box::new(on_drop))
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

        // Mark the core as closed to stop it from reading from the stream
        core.closed = true;

        // Wake the stream to finish closing it
        core.notify_stream_closed.take().map(|notify_stream_closed| notify_stream_closed.wake());

        // Run the drop function
        self.on_drop.take().map(|mut on_drop| {
            REFERENCE_CHUTE.desync(move |_| {
                (on_drop)()
            })
        });
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
