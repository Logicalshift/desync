//!
//! # Desync
//! 
//! This library provides an alternative synchronisation mechanism to the usual threads/mutexes
//! system. Instead of managing the threads, this focuses on managing the data, which largely
//! does away with the need for many synchronisation primitives. Support for futures is provided
//! to help interoperate with other Rust libraries.
//! 
//! There is a single new synchronisation object: `Desync`. You create one like this:
//! 
//! ```
//! use desync::Desync;
//! let number = Desync::new(0);
//! ```
//! 
//! It supports two main operations. `async` will schedule a new job for the object that will run
//! in a background thread. It's useful for deferring long-running operations and moving updates
//! so they can run in parallel.
//! 
//! ```
//! # use desync::Desync;
//! # use std::thread;
//! # use std::time::*;
//! let number = Desync::new(0);
//! number.async(|val| {
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
//! # number.async(|val| {
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
//! There's one final operation to be aware of and that's `future`. This returns a boxed Future that
//! can be used with other libraries that use them. It's conceptually the same as `sync`, except that
//! it doesn't wait for the operation to complete:
//! 
//! ```
//! # extern crate futures;
//! # extern crate desync;
//! # fn main() {
//! # use desync::Desync;
//! # use std::thread;
//! # use std::time::*;
//! # use futures::executor;
//! # let number = Desync::new(0);
//! # number.async(|val| {
//! #     // Long update here
//! #     thread::sleep(Duration::from_millis(100));
//! #     *val = 42;
//! # });
//! let future_number = number.future(|val| *val);
//! assert!(executor::spawn(future_number).wait_future().unwrap() == 42);
//! # }
//! ```
//! 

#[macro_use]
extern crate lazy_static;
extern crate num_cpus;
extern crate futures;

pub mod scheduler;
pub mod desync;

mod job;
mod unsafe_job;
mod scheduler_thread;

pub use desync::*;
