use desync::scheduler::*;

use super::timeout::*;

use std::thread;
use std::time::*;
use std::sync::mpsc::*;

use futures::channel::oneshot;

#[test]
fn schedule_future() {
    timeout(|| {
        use futures::executor;

        let queue       = queue();
        let future      = future(&queue, move || async {
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
        let future      = scheduler.future(&queue, move || async {
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
        let future      = scheduler.future(&queue, move || async {
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
