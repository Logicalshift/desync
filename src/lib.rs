//!
//! # Desync
//! 

#[macro_use]
extern crate lazy_static;
extern crate num_cpus;
extern crate futures;

pub mod scheduler;
pub mod desync;

pub use desync::*;
