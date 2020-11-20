use desync::scheduler::*;

use super::timeout::*;

use std::thread;
use std::time::*;
use std::sync::*;
use std::sync::mpsc::*;

use futures::prelude::*;
use futures::task;
use futures::task::{ArcWake, Poll};
use futures::channel::oneshot;

#[test]
fn schedule_future() {
    timeout(|| {
        use futures::executor;

        let queue       = queue();
        let future      = future_desync(&queue, move || async {
            thread::sleep(Duration::from_millis(100));
            42
        });

        executor::block_on(async {
            assert!(future.await.unwrap() == 42);
        });
    }, 500);
}

#[test]
fn schedule_future_with_no_scheduler_threads() {
    timeout(|| {
        use futures::executor;

        let scheduler   = Scheduler::new();

        // Even with 0 threads, futures should still run (by draining on the current thread as for sync actions)
        scheduler.set_max_threads(0);
        scheduler.despawn_threads_if_overloaded();

        let queue       = queue();
        let future      = scheduler.future_desync(&queue, move || async {
            thread::sleep(Duration::from_millis(100));
            42
        });

        executor::block_on(async {
            assert!(future.await.unwrap() == 42);
        });
    }, 500);
}

#[test]
fn wake_future_with_no_scheduler_threads() {
    timeout(|| {
        use futures::executor;

        let (tx, rx)    = oneshot::channel::<i32>();
        let scheduler   = Scheduler::new();

        // Even with 0 threads, futures should still run (by draining on the current thread as for sync actions)
        scheduler.set_max_threads(0);
        scheduler.despawn_threads_if_overloaded();

        // Schedule a future that will block until we send a value
        let queue       = queue();
        let future      = scheduler.future_desync(&queue, move || async {
            rx.await.expect("Receive result")
        });

        // Schedule a thread that will send a value
        thread::spawn(move || {
            // Wait for a bit before sending the result so the future should block
            thread::sleep(Duration::from_millis(100));

            tx.send(42).expect("Send")
        });

        executor::block_on(async {
            assert!(future.await.expect("result") == 42);
        });
    }, 500);
}

#[test]
fn wait_for_future() {
    timeout(|| {
        use futures::executor;

        // We use a oneshot as our future, and a mpsc channel to track progress
        let (tx, rx)                = channel();
        let (future_tx, future_rx)  = oneshot::channel();
        
        let scheduler   = scheduler();
        let queue       = queue();

        // Start by sending '1' from an async
        let tx2 = tx.clone();
        desync(&queue, move || { tx2.send(1).unwrap(); });

        // Then send the value sent via our oneshot using a future
        let tx2 = tx.clone();
        let future = scheduler.after(&queue, future_rx, 
            move |val| val.map(move |val| { tx2.send(val).unwrap(); 4 }));

        // Then send '3'
        let tx2 = tx.clone();
        desync(&queue, move || { tx2.send(3).unwrap(); });

        // '1' should be available, but we should otherwise be blocked on the future
        assert!(rx.recv().unwrap() == 1);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());

        // Send '2' to the future
        future_tx.send(2).unwrap();

        executor::block_on(async {
            // Future should resolve to 4
            assert!(future.await.unwrap() == Ok(4));

            // Should receive the '2' from the future, then 3
            assert!(rx.recv_timeout(Duration::from_millis(100)).unwrap() == 2);
            assert!(rx.recv().unwrap() == 3);
        });
    }, 500);
}

#[test]
fn future_waits_for_us() {
    timeout(|| {
        use futures::executor;

        // We use a oneshot as our future, and a mpsc channel to track progress
        let (tx, rx)                = channel();
        let (future_tx, future_rx)  = oneshot::channel();
        
        let scheduler   = scheduler();
        let queue       = queue();

        // Start by sending '1' from an async
        let tx2 = tx.clone();
        desync(&queue, move || { thread::sleep(Duration::from_millis(100)); tx2.send(1).unwrap(); });

        // Then send the value sent via our oneshot using a future
        let tx2 = tx.clone();
        let future = scheduler.after(&queue, future_rx, 
            move |val| val.map(move |val| { tx2.send(val).unwrap(); 4 }));

        // Then send '3'
        let tx2 = tx.clone();
        desync(&queue, move || { tx2.send(3).unwrap(); });

        // Send '2' to the future
        future_tx.send(2).unwrap();

        executor::block_on(async {
            // Future should resolve to 4
            assert!(future.await.unwrap() == Ok(4));

            // '1' should be available first
            assert!(rx.recv().unwrap() == 1);

            // Should receive the '2' from the future, then 3
            assert!(rx.recv_timeout(Duration::from_millis(100)).unwrap() == 2);
            assert!(rx.recv().unwrap() == 3);
        });
    }, 500);
}

///
/// Used for polling futures to see if they've notified us yet
///
struct TestWaker {
    pub awake: Mutex<bool>
}

impl ArcWake for TestWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        (*arc_self.awake.lock().unwrap()) = true;
    }
}

#[test]
fn poll_two_futures_on_one_queue() {
    // 0 threads so we force the future to act in 'drain' mode
    let scheduler   = Scheduler::new();

    // Even with 0 threads, futures should still run (by draining on the current thread as for sync actions)
    scheduler.set_max_threads(0);
    scheduler.despawn_threads_if_overloaded();

    // If a single queue has a future on 
    let queue           = queue();
    let (done1, recv1)  = oneshot::channel::<()>();
    let (done2, recv2)  = oneshot::channel::<()>();

    let wake1           = Arc::new(TestWaker { awake: Mutex::new(false) });
    let wake2           = Arc::new(TestWaker { awake: Mutex::new(false) });

    // Wait for done1 then done2 to signal
    let mut future_1    = scheduler.future_desync(&queue, move || {
        async move { recv1.await.ok(); }
    });

    let mut future_2    = scheduler.future_desync(&queue, move || {
        async move { recv2.await.ok(); }
    });

    // Poll future 1 then future 2 (as there are no threads to run, we'll use the 'drain on current thread' style, which will return pending as recv is pending)
    let waker_ref           = task::waker_ref(&wake1);
    let mut ctxt            = task::Context::from_waker(&waker_ref);

    assert!(future_1.poll_unpin(&mut ctxt) == Poll::Pending);

    // Only future_1 should be 'pollable' at this point (ie, is in the WaitForPoll state from the previous call)
    let waker_ref           = task::waker_ref(&wake2);
    let mut ctxt            = task::Context::from_waker(&waker_ref);

    assert!(future_2.poll_unpin(&mut ctxt) == Poll::Pending);

    // Finish both futures
    done1.send(()).unwrap();

    let waker_ref           = task::waker_ref(&wake2);
    let mut ctxt            = task::Context::from_waker(&waker_ref);

    assert!(future_2.poll_unpin(&mut ctxt) == Poll::Pending);

    done2.send(()).unwrap();

    // future_1 should be signalled for polling, future_2 should not
    assert!((*wake2.awake.lock().unwrap()) == false);
    assert!((*wake1.awake.lock().unwrap()) == true);

    // Retrieve the result for future_1
    let waker_ref           = task::waker_ref(&wake2);
    let mut ctxt            = task::Context::from_waker(&waker_ref);

    assert!(future_1.poll_unpin(&mut ctxt) == Poll::Ready(Ok(())));
    
    // Both future 1 and future 2 should have signalled now
    
    // TODO: this is a possible bug with 0 threads - the thread won't reschedule after future_1 completes, so wake2 will not yet be set
    //          (This is quite a complicated problem: if the drain continued processing jobs until it became pending instead of scheduling
    //          in the background, this would work but the return from poll could be delayed indefinitely)
    // assert!((*wake2.awake.lock().unwrap()) == true);
   
    // Give the scheduler a chance to run the other future (it will be queued in the background, so this is required for the notification to occur)
    scheduler.set_max_threads(1);
    for _ in 0..100 {
        if *wake2.awake.lock().unwrap() { break; }
        thread::sleep(Duration::from_millis(1));
    }
    assert!((*wake2.awake.lock().unwrap()) == true);

    let waker_ref           = task::waker_ref(&wake2);
    let mut ctxt            = task::Context::from_waker(&waker_ref);

    // Should be able to retrieve the result of future_2
    assert!(future_2.poll_unpin(&mut ctxt) == Poll::Ready(Ok(())));

    // TODO: not actually sure if this is bad behaviour or not but if future_2 is polled first, future_1 won't be available until future_2
    //      completes. This is another 0 thread only issue as future_1 will be able to send its notification when the thread pool is available.
}
