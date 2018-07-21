extern crate desync;
extern crate futures;

use desync::*;
use futures::stream;

use std::sync::*;

#[test]
fn pipe_in_simple_stream() {
    // Create a stream
    let stream  = vec![1, 2, 3];
    let stream  = stream::iter_ok(stream);

    // Create an object for the stream to be piped into
    let obj     = Arc::new(Desync::new(vec![]));

    // Pipe the stream into the object
    pipe_in(Arc::clone(&obj), stream, |core: &mut Vec<Result<i32, ()>>, item| core.push(item));

    // Once the stream is drained, the core should contain Ok(1), Ok(2), Ok(3)
    assert!(obj.sync(|core| core.clone()) == vec![Ok(1), Ok(2), Ok(3)])
}
