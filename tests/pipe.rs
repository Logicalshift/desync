extern crate desync;
extern crate futures;

use desync::*;
use futures::stream;
use futures::future;
use futures::executor;
use futures::sink::{SinkExt};
use futures::stream::{StreamExt};
use futures::channel::mpsc;
use futures::prelude::*;

use std::sync::*;
use std::thread;
use std::time::Duration;

#[test]
fn pipe_in_simple_stream() {
    // Create a stream
    let stream  = vec![1, 2, 3];
    let stream  = stream::iter(stream);

    // Create an object for the stream to be piped into
    let obj     = Arc::new(Desync::new(vec![]));

    // Pipe the stream into the object
    pipe_in(Arc::clone(&obj), stream, |core: &mut Vec<Result<i32, ()>>, item| { core.push(Ok(item)); Box::pin(future::ready(())) });

    // Delay to allow the messages to be processed on the stream
    thread::sleep(Duration::from_millis(10));

    // Once the stream is drained, the core should contain Ok(1), Ok(2), Ok(3)
    assert!(obj.sync(|core| core.clone()) == vec![Ok(1), Ok(2), Ok(3)])
}

#[test]
fn pipe_in_mpsc_receiver() {
    // Create a channel to send to the object
    let (mut sender, receiver) = mpsc::channel(0);

    // Create an object
    let obj = Arc::new(Desync::new(vec![]));

    // Add anything received to the vector via a pipe
    pipe_in(Arc::clone(&obj), receiver, |core, item| { core.push(item); Box::pin(future::ready(())) });

    // Initially empty
    assert!(obj.sync(|core| core.clone()) == vec![]);

    // Send some values
    let send_values = async {
        sender.send(1).await.unwrap();
        sender.send(2).await.unwrap();
    };
    executor::block_on(send_values);
    
    // Delay to allow the messages to be processed on the stream
    // TODO: fix so this isn't needed. This happens because there's a race between when 'poll'
    // is called in the pipe and the 'async' call
    thread::sleep(Duration::from_millis(20));

    // Should be available on the core
    assert!(obj.sync(|core| core.clone()) == vec![1, 2]);
}

#[test]
fn pipe_through() {
    // Create a channel we'll use to send data to the pipe
    let (mut sender, receiver) = mpsc::channel(10);

    // Create an object to pipe through
    let obj             = Arc::new(Desync::new(1));

    // Create a pipe that adds values from the stream to the value in the object
    let mut pipe_out    = pipe(Arc::clone(&obj), receiver, |core, item| future::ready(item + *core).boxed());

    // Start things running
    executor::block_on(async {
        sender.send(2).await.unwrap();
        assert!(pipe_out.next().await == Some(3));

        sender.send(42).await.unwrap();
        assert!(pipe_out.next().await == Some(43));

        // It is possible for a poll to already be pending again at this point, which may race to read the value we set later on, so we synchronise to ensure they are all processed
        obj.sync(|_| { });

        // Changing the value should change the output
        obj.desync(|core| *core = 2);

        sender.send(44).await.unwrap();
        assert!(pipe_out.next().await == Some(46));
    });
}

#[test]
fn pipe_through_1000() {
    for _ in 0..1000 {
        // Create a channel we'll use to send data to the pipe
        let (mut sender, receiver) = mpsc::channel(10);

        // Create an object to pipe through
        let obj             = Arc::new(Desync::new(1));

        // Create a pipe that adds values from the stream to the value in the object
        let mut pipe_out    = pipe(Arc::clone(&obj), receiver, |core, item| future::ready(item + *core).boxed());

        // Start things running
        executor::block_on(async {
            sender.send(2).await.unwrap();
            assert!(pipe_out.next().await == Some(3));

            sender.send(42).await.unwrap();
            assert!(pipe_out.next().await == Some(43));

            // It is possible for a poll to already be pending again at this point, which may race to read the value we set later on, so we synchronise to ensure they are all processed
            obj.sync(|_| { });

            // Changing the value should change the output
            obj.desync(|core| *core = 2);

            sender.send(44).await.unwrap();
            assert!(pipe_out.next().await == Some(46));
        });
    }
}

#[test]
fn pipe_through_stream_closes() {
    let mut pipe_out_with_closed_stream = {
        // Create a channel we'll use to send data to the pipe
        let (mut sender, receiver) = mpsc::channel(10);

        // Create an object to pipe through
        let obj = Arc::new(Desync::new(1));

        // Create a pipe that adds values from the stream to the value in the object
        let mut pipe_out = pipe(Arc::clone(&obj), receiver, |core, item: i32| future::ready(item + *core).boxed());

        // Start things running
        executor::block_on(async {
            sender.send(2).await.unwrap();
            assert!(pipe_out.next().await == Some(3))
        });

        pipe_out
    };

    executor::block_on(async {
        // The sender is now closed (the sender and receiver are dropped after the block above), so the pipe should close too
        assert!(pipe_out_with_closed_stream.next().await == None);
    });
}

#[test]
fn pipe_through_produces_backpressure() {
    // Create a channel we'll use to send data to the pipe
    let (mut sender, receiver) = mpsc::channel(0);

    // Create an object to pipe through
    let obj = Arc::new(Desync::new(1));

    // Create a pipe that adds values from the stream to the value in the object
    let mut pipe_out    = pipe(Arc::clone(&obj), receiver, |core, item: i32| future::ready(item + *core).boxed());

    // Set the backpressure depth to 3
    pipe_out.set_backpressure_depth(3);

    // Start things running. We never read from this pipe here
    executor::block_on(async {
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
    });
}
