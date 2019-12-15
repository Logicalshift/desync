//!
//! # Desync
//! 
//! 
//! This is a library for Rust that provides a model of concurrency based around the idea of 
//! scheduling operations on data. This is in contrast to the traditional model where operations
//! are scheduled on threads with ownership of the data being passed between them.
//! 
//! This approach has several advantages over the traditional method:
//! 
//!  * It's simpler: almost the  entire set of thread methods and synchronisation primitives can 
//!    be replaced with the two fundamental scheduling functions, `sync()` and `desync()`. 
//!  * It's easier to reason about: scheduled operations are always performed in the order they're 
//!    queued so race conditions and similar issues due to out-of-order execution are both much rarer 
//!    and easier to debug.
//!  * It makes it easier to write highly concurrent code: desync makes moving between performing
//!    operations synchronously and asynchronously trivial, with no need to deal with adding code to
//!    start threads or communicate between them.
//! 
//! In addition to the two fundamental methods, desync provides methods for generating futures and
//! processing streams.
//! 
//! # Quick start
//! 
//! There is a single new synchronisation object: `Desync`. You create one like this:
//! 
//! ```
//! use desync::Desync;
//! let number = Desync::new(0);
//! ```
//! 
//! It supports two main operations. `desync` will schedule a new job for the object that will run
//! in a background thread. It's useful for deferring long-running operations and moving updates
//! so they can run in parallel.
//! 
//! ```
//! # use desync::Desync;
//! # use std::thread;
//! # use std::time::*;
//! let number = Desync::new(0);
//! number.desync(|val| {
//!     // Long update here
//!     thread::sleep(Duration::from_millis(100));
//!     *val = 42;
//! });
//! 
//! // We can carry on what we're doing with the update now running in the background
//! ```
//! 
//! The other operation is `sync`, which schedules a job to run synchronously on the data structure.
//! This is useful for retrieving values from a `Desync`.
//! 
//! ```
//! # use desync::Desync;
//! # use std::thread;
//! # use std::time::*;
//! # let number = Desync::new(0);
//! # number.desync(|val| {
//! #     // Long update here
//! #     thread::sleep(Duration::from_millis(100));
//! #     *val = 42;
//! # });
//! let new_number = number.sync(|val| *val);           // = 42
//! # assert!(new_number == 42);
//! ```
//! 
//! `Desync` objects always run operations in the order that is provided, so all operations are
//! serialized from the point of view of the data that they contain. When combined with the ability
//! to perform operations asynchronously, this provides a useful way to immediately parallelize
//! long-running operations.
//! 
//! The `future()` action returns a boxed Future that can be used with other libraries that use them. It's 
//! conceptually the same as `sync`, except that it doesn't wait for the operation to complete:
//! 
//! ```
//! # extern crate futures;
//! # extern crate desync;
//! # fn main() {
//! # use desync::Desync;
//! # use std::thread;
//! # use std::time::*;
//! # use futures::executor;
//! # use futures::future;
//! # let number = Desync::new(0);
//! # number.desync(|val| {
//! #     // Long update here
//! #     thread::sleep(Duration::from_millis(100));
//! #     *val = 42;
//! # });
//! let future_number = number.future(|val| future::ready(*val));
//! assert!(executor::block_on(async { future_number.await.unwrap() }) == 42 );
//! # }
//! ```
//! 
//! Note that this is the equivalent of just `number.sync(|val| *val)`, so this is mainly useful for
//! interacting with other code that's already using futures. The `after()` function is also provided
//! for using the results of futures to update the contents of `Desync` data: these all preserve the
//! strict order-of-operations semantics, so operations scheduled after an `after` won't start until
//! that operation has completed.
//! 
//! # Pipes and streams
//! 
//! As well as support for futures, Desync provides supports for streams. The `pipe_in()` and `pipe()`
//! functions provide a way to process stream data in a desync object as it arrives. `pipe_in()` just
//! processes stream data as it arrives, and `pipe()` provides an output stream of data.
//! 
//! `pipe()` is quite useful as a way to provide asynchronous access to synchronous code: it can be used
//! to create a channel to send requests to an asynchronous target and retrieve results back via its
//! output. (Unlike this 'traditional' method, the actual scheduling and channel maintenance does not 
//! need to be explicitly implemented)
//! 

#![warn(bare_trait_objects)]

#[macro_use]
extern crate lazy_static;
extern crate futures;

#[cfg(not(target_arch = "wasm32"))]
extern crate num_cpus;

pub mod scheduler;
pub mod desync;
pub mod pipe;

mod job;
mod future_job;
mod unsafe_job;
mod scheduler_thread;

pub use self::desync::*;
pub use self::pipe::*;
