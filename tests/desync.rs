extern crate desync;
extern crate futures;

use desync::Desync;

mod scheduler;
use self::scheduler::timeout::*;

use futures::prelude::*;
use futures::future;

use std::sync::*;
use std::time::*;
use std::thread::*;

#[derive(Debug)]
struct TestData {
    val: u32
}

#[test]
fn retrieve_data_synchronously() {
    let desynced = Desync::new(TestData { val: 0 });

    assert!(desynced.sync(|data| data.val) == 0);
}

#[test]
fn retrieve_data_into_local_var() {
    let desynced = Desync::new(TestData { val: 42 });
    let mut val = 0;

    desynced.sync(|data| val = data.val);

    assert!(val == 42);
}

#[test]
fn update_data_asynchronously() {
    let desynced = Desync::new(TestData { val: 0 });

    desynced.desync(|data| {
        sleep(Duration::from_millis(100));
        data.val = 42;
    });
    
    assert!(desynced.sync(|data| data.val) == 42);
}

#[test]
fn update_data_asynchronously_1000_times() {
    for _i in 0..1000 {
        timeout(|| {
            let desynced = Desync::new(TestData { val: 0 });

            desynced.desync(|data| {
                data.val = 42;
            });
            desynced.desync(|data| {
                data.val = 43;
            });
            
            assert!(desynced.sync(|data| data.val) == 43);
        }, 500);
    }
}

#[test]
fn update_data_with_future() {
    timeout(|| {
        use futures::executor;

        let desynced = Desync::new(TestData { val: 0 });

        desynced.desync(|data| {
            sleep(Duration::from_millis(100));
            data.val = 42;
        });

        executor::block_on(async {
            let future = desynced.future(|data| { Box::pin(future::ready(data.val)) });
            assert!(future.await.unwrap() == 42);
        });
    }, 500);
}

#[test]
fn update_data_with_future_1000_times() {
    // Seems to timeout fairly reliably after signalling the future
    use futures::executor;

    for _i in 0..1000 {
        timeout(|| {
            let desynced = Desync::new(TestData { val: 0 });

            desynced.desync(|data| {
                data.val = 42;
            });
            desynced.desync(|data| {
                data.val = 43;
            });

            executor::block_on(async {
                let future = desynced.future(|data| Box::pin(future::ready(data.val)));
                
                assert!(future.await.unwrap() == 43);
            });
        }, 500);
    }
}

#[test]
fn dropping_while_running_isnt_obviously_bad() {
    let desynced = Desync::new(TestData { val: 0 });

    desynced.desync(|data| {
        sleep(Duration::from_millis(100));
        data.val = 42;
    });
    desynced.desync(|data| {
        sleep(Duration::from_millis(100));
        data.val = 42;
    });
}

#[test]
fn wait_for_future() {
    // TODO: occasional test failure that happens if the future 'arrives' before the queue is empty
    // (Because we need a future that arrives when the queue is actually suspended)
    timeout(|| {
        use futures::executor;
        use futures::channel::oneshot;

        // We use a oneshot as our future, and a mpsc channel to track progress
        let desynced                = Desync::new(0);
        let (future_tx, future_rx)  = oneshot::channel();

        // First value 0 -> 1
        desynced.desync(|val| { 
            // Sleep here so the future should be waiting for us
            sleep(Duration::from_millis(100));
            assert!(*val == 0);
            *val = 1; 
        });

        // Future should go 1 -> 2, but takes whatever future_tx sends
        let future = desynced.after(future_rx, |val, future_result| {
            assert!(*val == 1);
            *val = future_result.unwrap();

            // Return '4' to anything listening for this future
            4
        });

        // Finally, 3
        desynced.desync(move |val| { assert!(*val == 2); *val = 3 });

        executor::block_on(async {
            // Send '2' to the future
            future_tx.send(2).unwrap();

            // Future should resolve to 4
            assert!(future.await == Ok(4));

            // Final value should be 3
            assert!(desynced.sync(|val| *val) == 3);
        })
    }, 500);
}

#[test]
fn future_and_sync() {
    // This test seems to produce different behaviour if it's run by itself (this sleep tends to force it to run after the other tests and thus fail)
    // So far the failure seems reliable when this test is running exclusively
    sleep(Duration::from_millis(1000));

    use std::thread;
    use futures::prelude::*;
    use futures::channel::oneshot;

    // The idea here is we perform an action with a future() and read the result back with a sync() (which is a way you can mix-and-match
    // programming models with desync)
    // 
    // The 'core' runs a request as a future, waiting for the channel result. We store the result in sync_request, and then retrieve
    // it again by calling sync - as Desync always runs things sequentially, it guarantees the ordering (something that's much harder
    // to achieve with a mutex)
    let (send, recv)    = oneshot::channel::<i32>();
    let core            = Desync::new(0);
    let sync_request    = Desync::new(None);

    // Send a request to the 'core' via the sync reqeust and store the result
    let _ = sync_request.future(move |data| {
        async move {
            let result = core.future(move |_core| {
                async move {
                    Some(recv.await.unwrap())
                }.boxed()
            }).await;

            *data = result.unwrap();
        }.boxed()
    });

    // Signal the future after a delay
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        send.send(42).ok();
    });

    // Retrieve the result once the future completes
    let result = sync_request.sync(|req| req.take());

    // Should retrieve the value generated in the future
    assert!(result == Some(42));
}

#[test]
fn double_future_and_sync() {
    use std::thread;

    // This test will queue two futures here, each of which will need to return to another desync
    // If two futures are scheduled and triggered in a row when draining a queue that both signal
    let core        = Arc::new(Desync::new(()));

    let initiator_1 = Desync::new(None);
    let initiator_2 = Desync::new(None);
    let initiator_3 = Desync::new(None);

    let core_1      = Arc::clone(&core);
    let _           = initiator_1.future(move |val| {
        async move {
            // Wait for a task on the core
            *val = core_1.future(move |_| {
                async move { thread::sleep(Duration::from_millis(400)); Some(1) }.boxed()
            }).await.unwrap();
        }.boxed()
    });

    let core_2      = Arc::clone(&core);
    let _           = initiator_2.future(move |val| {
        async move {
            // Wait for the original initiator to start its future
            thread::sleep(Duration::from_millis(100));

            // Wait for a task on the core
            *val = core_2.future(move |_| {
                async move { thread::sleep(Duration::from_millis(200)); Some(2) }.boxed()
            }).await.unwrap();
        }.boxed()
    });

    let core_3      = Arc::clone(&core);
    let _           = initiator_3.future(move |val| {
        async move {
            // Wait for the original initiator to start its future
            thread::sleep(Duration::from_millis(200));

            // Wait for a task on the core
            *val = core_3.future(move |_| {
                async move { thread::sleep(Duration::from_millis(200)); Some(3) }.boxed()
            }).await.unwrap();
        }.boxed()
    });

    // Wait for the result from the futures synchronously
    assert!(initiator_3.sync(|val| { *val }) == Some(3));
    assert!(initiator_2.sync(|val| { *val }) == Some(2));
    assert!(initiator_1.sync(|val| { *val }) == Some(1));
}
