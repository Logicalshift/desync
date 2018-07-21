extern crate desync;
extern crate futures;

use desync::*;
use futures::stream;
use futures::executor;
use futures::sync::mpsc;

use std::sync::*;
use std::thread;
use std::time::Duration;

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

#[test]
fn pipe_in_mpsc_receiver() {
    // Create a channel to send to the object
    let (sender, receiver) = mpsc::channel(10);

    // Create an object
    let obj = Arc::new(Desync::new(vec![]));

    // Add anything received to the vector via a pipe
    pipe_in(Arc::clone(&obj), receiver, |core, item| core.push(item.unwrap()));

    // Initially empty
    assert!(obj.sync(|core| core.clone()) == vec![]);

    // Send some values
    let mut sender = executor::spawn(sender);
    sender.wait_send(1).unwrap();
    sender.wait_send(2).unwrap();

    // Should arrive after a short delay
    thread::sleep(Duration::from_millis(10));
    assert!(obj.sync(|core| core.clone()) == vec![1, 2]);
}
