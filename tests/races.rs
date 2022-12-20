use futures::channel::mpsc;

use std::sync::{Arc};
use futures::prelude::*;
use futures::executor;
use ::desync::*;

use std::thread;

#[test]
#[cfg(not(miri))]   // slow!
fn pipe_blockage() {
    // See https://github.com/Logicalshift/desync/issues/4 - this test fails by blocking and timing out

    let (mut sender0, mut stream0) = mpsc::channel(1000);
    executor::block_on(async { sender0.send(0).await.unwrap() });
    let (mut sender, stream) = mpsc::channel(1000);

    thread::spawn(move || {
        executor::block_on(async move {
            while let Some(counter) = stream0.next().await {
                sender.send(counter).await.unwrap();
            }
        })
    });

    let mut stream2 = pipe(Arc::new(Desync::new(0)), stream, move |_, item| {
        let result = item + 1;
        futures::future::ready(result).boxed()
    });

    let mut next_expected = 1i64;

    executor::block_on(async move {
        println!("Running...");
        loop {
            let next = stream2.next().await.unwrap();
            sender0.send(next).await.unwrap();
            assert_eq!(next_expected, next);
            if (next_expected % 10_000) == 0 {
                println!("{} iterations", next_expected);
            }
            next_expected += 1;

            if next_expected > 100_000 {
                break;
            }
        }
    });
}

#[test]
#[cfg(not(miri))]   // slow!
fn pipe_through() {
    // Race: the 'desync' to update the value to 2 should schedule ahead of the 'send(42)'
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
            let val = pipe_out.next().await;
            println!("{:?}", val);
            assert!(val == Some(46));
        });
    }
}
