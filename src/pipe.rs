//!
//! Desync pipes provide a way to generate and process streams via a `Desync` object
//! 

use super::desync::*;

use futures::*;

use std::result::Result;

impl<Core: 'static+Send> Desync<Core> {
    ///
    /// Pipes a stream into this object. Whenever an item becomes available on the stream, the
    /// processing function is called asynchronously with the item that was received.
    /// 
    pub fn pipe_in<S, ProcessFn>(&self, stream: S, process: ProcessFn)
    where   S:          Stream,
            ProcessFn:  Send+FnMut(&mut Core, Result<S::Item, S::Error>) -> () {
        unimplemented!()
    }

    ///
    /// Pipes a stream into this object. Whenever an item becomes available on the strema, the
    /// processing function is called asynchronously with the item that was received. The
    /// return value is placed onto the output stream.
    /// 
    pub fn pipe<S, Output, OutputErr, ProcessFn>(&self, stream: S, process: ProcessFn) -> Box<dyn Stream<Item=Output, Error=OutputErr>> 
    where   S:          Stream,
            Output:     Send,
            OutputErr:  Send,
            ProcessFn:  Send+FnMut(&mut Core, Result<S::Item, S::Error>) -> Result<Output, OutputErr> {
        unimplemented!()
    }
}
