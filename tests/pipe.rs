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

    // Should be available on the core
    assert!(obj.sync(|core| core.clone()) == vec![1, 2]);
}

#[test]
fn pipe_through() {
    // Create a channel we'll use to send data to the pipe
    let (sender, receiver) = mpsc::channel(10);

    // Create an object to pipe through
    let obj = Arc::new(Desync::new(1));

    // Create a pipe that adds values from the stream to the value in the object
    let pipe_out = pipe(Arc::clone(&obj), receiver, |core, item: Result<i32, ()>| item.map(|item| item + *core));

    // Start things running
    let mut sender      = executor::spawn(sender);
    let mut pipe_out    = executor::spawn(pipe_out);

    // Sending a value should add whatever is in the desync object
    sender.wait_send(2).unwrap();
    assert!(pipe_out.wait_stream() == Some(Ok(3)));

    sender.wait_send(42).unwrap();
    assert!(pipe_out.wait_stream() == Some(Ok(43)));

    // Changing the value should change the output
    obj.async(|core| *core = 2);

    sender.wait_send(42).unwrap();
    assert!(pipe_out.wait_stream() == Some(Ok(44)));
}

#[test]
fn pipe_through_stream_closes() {
    let mut pipe_out_with_closed_stream = {
        // Create a channel we'll use to send data to the pipe
        let (sender, receiver) = mpsc::channel(10);

        // Create an object to pipe through
        let obj = Arc::new(Desync::new(1));

        // Create a pipe that adds values from the stream to the value in the object
        let pipe_out = pipe(Arc::clone(&obj), receiver, |core, item: Result<i32, ()>| item.map(|item| item + *core));

        // Start things running
        let mut sender      = executor::spawn(sender);
        let mut pipe_out    = executor::spawn(pipe_out);

        // Sending a value should add whatever is in the desync object
        sender.wait_send(2).unwrap();
        assert!(pipe_out.wait_stream() == Some(Ok(3)));

        pipe_out
    };

    // The sender is now closed (the sender and receiver are dropped after the block above), so the pipe should close too
    assert!(pipe_out_with_closed_stream.wait_stream() == None);
}

#[test]
fn pipe_through_produces_backpressure() {
    // Create a channel we'll use to send data to the pipe
    let (mut sender, receiver) = mpsc::channel(0);

    // Create an object to pipe through
    let obj = Arc::new(Desync::new(1));

    // Create a pipe that adds values from the stream to the value in the object
    let mut pipe_out    = pipe(Arc::clone(&obj), receiver, |core, item: Result<i32, ()>| item.map(|item| item + *core));

    // Set the backpressure depth to 3
    pipe_out.set_backpressure_depth(3);

    // Start things running. We never read from this pipe here
    let _pipe_out       = executor::spawn(pipe_out);

    // Send 3 events to the pipe. Wait a bit between them to allow for processing time
    for _x in 0..3 {
        assert!(sender.try_send(1) == Ok(()));

        // The wait here allows the message to flow through to the pipe (if we call try_send again before the pipe has a chance to accept the input)
        thread::sleep(Duration::from_millis(5));
    }

    // This will stick in the channel (pipe should not be accepting more input)
    assert!(sender.try_send(2) == Ok(()));
    thread::sleep(Duration::from_millis(5));

    // Channel will push back on this one
    let channel_full = sender.try_send(3);
    assert!(channel_full.is_err());
    assert!(channel_full.unwrap_err().is_full());
}