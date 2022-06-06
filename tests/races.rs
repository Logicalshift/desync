use futures::channel::mpsc;

use std::sync::{Arc};
use futures::prelude::*;
use futures::executor;
use ::desync::*;

use std::thread;

#[test]
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
