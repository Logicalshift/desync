extern crate desync;
extern crate futures;

use desync::Desync;

mod scheduler;
use self::scheduler::timeout::*;

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
            let future = desynced.future_desync(|data| { future::ready(data.val) });
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
                let future = desynced.future_desync(|data| Box::pin(future::ready(data.val)));
                
                assert!(future.await.unwrap() == 43);
            });
        }, 500);
    }
}

#[test]
fn update_data_with_future_sync() {
    timeout(|| {
        use futures::executor;

        let desynced = Desync::new(TestData { val: 0 });

        desynced.desync(|data| {
            sleep(Duration::from_millis(100));
            data.val = 42;
        });

        executor::block_on(async {
            let future = desynced.future_sync(|data| { future::ready(data.val) });
            assert!(future.await.unwrap() == 42);
        });
    }, 500);
}

#[test]
fn update_data_with_future_sync_1000_times() {
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
                let future = desynced.future_sync(|data| future::ready(data.val));
                
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
    let _ = sync_request.future_desync(move |data| {
        async move {
            let result = core.future_desync(move |_core| {
                async move {
                    Some(recv.await.unwrap())
                }
            }).await;

            *data = result.unwrap();
        }
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

    // TODO: signal with channels instead of using thread::sleep

    // This test will queue two futures here, each of which will need to return to another desync
    // If two futures are scheduled and triggered in a row when draining a queue that both signal
    let core        = Arc::new(Desync::new(()));

    let initiator_1 = Desync::new(None);
    let initiator_2 = Desync::new(None);
    let initiator_3 = Desync::new(None);

    let core_1      = Arc::clone(&core);
    initiator_1.future_desync(move |val| {
        async move {
            // Wait for a task on the core
            *val = core_1.future_desync(move |_| {
                async move { thread::sleep(Duration::from_millis(400)); Some(1) }
            }).await.unwrap();
        }
    }).detach();

    let core_2      = Arc::clone(&core);
    initiator_2.future_desync(move |val| {
        async move {
            // Wait for the original initiator to start its future
            thread::sleep(Duration::from_millis(100));

            // Wait for a task on the core
            *val = core_2.future_desync(move |_| {
                async move { thread::sleep(Duration::from_millis(200)); Some(2) }
            }).await.unwrap();
        }
    }).detach();

    let core_3      = Arc::clone(&core);
    initiator_3.future_desync(move |val| {
        async move {
            // Wait for the original initiator to start its future
            thread::sleep(Duration::from_millis(200));

            // Wait for a task on the core
            *val = core_3.future_desync(move |_| {
                async move { thread::sleep(Duration::from_millis(200)); Some(3) }
            }).await.unwrap();
        }
    }).detach();

    // Wait for the result from the futures synchronously
    assert!(initiator_3.sync(|val| { *val }) == Some(3));
    assert!(initiator_2.sync(|val| { *val }) == Some(2));
    assert!(initiator_1.sync(|val| { *val }) == Some(1));
}

#[test]
fn try_sync_succeeds_on_idle_queue() {
    timeout(|| {
        let core        = Desync::new(0);

        // Queue is doing nothing, so try_sync should succeed
        let sync_result = core.try_sync(|val| {
            *val = 42;
            1
        });

        // Queue is idle, so we should receive a result
        assert!(sync_result == Ok(1));

        // Double-check that the value was updated
        assert!(core.sync(|val| *val) == 42);
    }, 500);
}

#[test]
fn try_sync_succeeds_on_idle_queue_after_async_job() {
    timeout(|| {
        use std::thread;
        let core        = Desync::new(0);

        // Schedule something asynchronously and wait for it to complete
        core.desync(|_val| thread::sleep(Duration::from_millis(50)));
        core.sync(|_val| { });

        // Queue is doing nothing, so try_sync should succeed
        let sync_result = core.try_sync(|val| {
            *val= 42;
            1
        });

        // Queue is idle, so we should receive a result
        assert!(sync_result == Ok(1));

        // Double-check that the value was updated
        assert!(core.sync(|val| *val) == 42);
    }, 500);
}

#[test]
fn try_sync_fails_on_busy_queue() {
    timeout(|| {
        use std::sync::mpsc::*;
        
        let core        = Desync::new(0);

        // Schedule on the queue and block it
        let (tx, rx)    = channel();

        core.desync(move |_val| { rx.recv().ok(); });

        // Queue is busy, so try_sync should fail
        let sync_result = core.try_sync(|val| {
            *val = 42;
            1
        });

        // Queue is idle, so we should receive a result
        assert!(sync_result.is_err());

        // Unblock the queue
        tx.send(1).ok();

        // Double-check that the value was not updated
        assert!((core.sync(|val| *val)) == 0);
    }, 500);
}

#[test]
fn fill_thread_pool_with_wait_in_background() {
    timeout(|| {
        use std::thread;
        use std::sync::mpsc;
        use std::time::{Duration};

        // This is potentially an issue with how we do thread stealing: the idea here is to create
        // 'WaitInBackground' status syncs across the whole thread pool, so when a 'foreground' sync
        // finishes there are no threads available to carry it on.
        //
        // A 'green' threads library would be ideal here as we could re-use the blocked threads: this
        // test mostly looks at the case where this isn't available. 
        let (waiting_send, waiting_recv)    = mpsc::channel();
        let (release_send, release_recv)    = mpsc::channel();
        let wait_on                         = Arc::new(Desync::new(()));

        // In a background thread, block 'wait_on' until something is sent to the 'release' channel
        let background_wait                 = Arc::clone(&wait_on);
        thread::spawn(move || {
            background_wait.sync(move |_| {
                // Tell the main thread that we've reached this point (desync will steal the thread we created, which is how we exhaust the thread pool later on)
                waiting_send.send(()).unwrap();

                println!("Blocking Desync waiting...");

                // Wait for the release message
                release_recv.recv().unwrap();

                println!("Blocking Desync finished (unblocking)");
            });
        });

        // Wait for the spawned thread to signal that it has started running
        waiting_recv.recv().unwrap();

        // Create a bunch of desyncs and tell them to wait on the same sync (this will eventually fill the thread pool with threads that are waiting for a background desync)
        let background_desyncs = (0..100).into_iter().map(|_| Arc::new(Desync::new(()))).collect::<Vec<_>>();

        background_desyncs.iter()
            .for_each(|background| {
                let background_wait = Arc::clone(&wait_on);

                background.desync(move |_| {
                    // Doesn't really matter what the task is...
                    println!("Waiting for blocking Desync...");
                    background_wait.sync(|_| { });
                    println!("Finished background task");
                })
            });

        // Sleep to allow the thread pool to saturate (tests all run in parallel so this will be most reliable when this test is run by itself)
        // (Another problem as this blocks the scheduler when it fails is it could cause other tests to time out, so definitely run this by itself
        // to check if there are issues)
        thread::sleep(Duration::from_millis(100));

        // Send the release so our background thread finishes, and now the background desyncs can run
        println!("Releasing blocking desync...");
        release_send.send(()).unwrap();

        // Should be able to sync with all the background tasks
        println!("Synchronising with background tasks...");
        background_desyncs.into_iter()
            .for_each(|background| background.sync(|_| { }));

        // Should also be able to sync with the 'wait_on' desync (after all the background things)
        println!("Synchronising with blocking desync to finish");
        wait_on.sync(|_| { });
    }, 20000);
}
