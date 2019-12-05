extern crate desync;
extern crate futures;

use desync::Desync;

mod scheduler;
use self::scheduler::timeout::*;

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
            let future = desynced.future(|data| { data.val });
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
                let future = desynced.future(|data| data.val);
                
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
        let desynced = Desync::new(0);
        let (mut future_tx, future_rx)  = oneshot::channel();

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
            assert!(future.await == 4);

            // Final value should be 3
            assert!(desynced.sync(|val| *val) == 3);
        })
    }, 500);
}
