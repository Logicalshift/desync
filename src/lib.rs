//!
//! # Desync
//! 
//! Desync provides a new synchronisation type, `Desync<T>`, which works by ordering operations on
//! its enclosed data type instead of the traditional method of using mutexes to protect critical
//! sections. This allows concurrency to be built around two basic operations:
//! 
//!  * `desync_thing.sync(|thing| /* ... */)` for synchronous access to the data
//!  * `desync_thing.desync(|thing| /* ... */)` for asynchronous access to the data - running the supplied task in the background.
//! 
//! If only the `sync()` operation is used, this is roughly equivalent to a standard `Mutex`, except
//! with much stronger guarantees about which thread gets the data first. The other operation,
//! `desync()` effectively replaces the need to spawn threads and move data around in order to 
//! add concurrency to a program.
//! 
//! Desync also provides equivalent methods for async code: `future_sync()` will perform an operation
//! in the current async context and `future_desync()` will schedule an operation in the background.
//! These can be freely mixed with the `sync()` and `desync()` operations so it becomes fairly easy to
//! mix code using traditional threading and code using async futures. As Desync uses order-of-operations
//! to guarantee exclusive access to the data, these operations can borrow the contained data across any
//! `await`s that might be needed, unlike locks created using the `Mutex` type, which can't be sent
//! between threads.
//! 
//! Desync provides fairly strong ordering guarantees: in particular, when any of the methods return,
//! the ordering of the operation is guaranteed relative to any following operation. This property makes
//! desync code quite easy to follow and less prone to race conditions than traditional threading. The
//! ability to easily schedule updates asynchronously provides a way around common scenarios where the
//! need to lock multiple mutexes can create deadlocks.
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
//! The `future_sync()` action returns a boxed Future that can be used with other libraries that use them. It's 
//! conceptually the same as `sync`, except that it doesn't wait for the operation to complete:
//! 
//! ```
//! # extern crate futures;
//! # extern crate desync;
//! # fn main() {
//! # use desync::Desync;
//! # use std::thread;
//! # use std::time::*;
//! # use futures::{FutureExt};
//! # use futures::executor;
//! # use futures::future;
//! # let number = Desync::new(0);
//! # number.desync(|val| {
//! #     // Long update here
//! #     thread::sleep(Duration::from_millis(100));
//! #     *val = 42;
//! # });
//! let future_number = number.future_sync(|val| future::ready(*val).boxed());
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

pub use self::scheduler::{TrySyncError};
pub use self::desync::*;
pub use self::pipe::*;
