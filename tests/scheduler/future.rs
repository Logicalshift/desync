use desync::scheduler::*;

use super::timeout::*;

use std::thread;
use std::time::*;
use std::sync::mpsc::*;

use futures::sync::oneshot;

#[test]
fn can_schedule_future() {
    timeout(|| {
        use futures::executor;

        let queue       = queue();
        let mut future  = executor::spawn(future(&queue, move || {
            thread::sleep(Duration::from_millis(100));
            42
        }));

        assert!(future.wait_future().unwrap() == 42);
    }, 500);
}

#[test]
fn can_wait_for_future() {
    timeout(|| {
        use futures::executor;

        // We use a oneshot as our future, and a mpsc channel to track progress
        let (tx, rx)                = channel();
        let (future_tx, future_rx)  = oneshot::channel();
        
        let scheduler   = scheduler();
        let queue       = queue();

        // Start by sending '1' from an async
        let tx2 = tx.clone();
        async(&queue, move || { tx2.send(1).unwrap(); });

        // Then send the value sent via our oneshot using a future
        let tx2 = tx.clone();
        let future = scheduler.after(&queue, future_rx, 
            move |val| val.map(move |val| { tx2.send(val).unwrap(); 4 }));

        // Then send '3'
        let tx2 = tx.clone();
        async(&queue, move || { tx2.send(3).unwrap(); });

        // '1' should be available, but we should otherwise be blocked on the future
        assert!(rx.recv().unwrap() == 1);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());

        // Send '2' to the future
        future_tx.send(2).unwrap();
        let mut future  = executor::spawn(future);

        // Future should resolve to 4
        assert!(future.wait_future().unwrap() == 4);

        // Should receive the '2' from the future, then 3
        assert!(rx.recv_timeout(Duration::from_millis(100)).unwrap() == 2);
        assert!(rx.recv().unwrap() == 3);
    }, 500);
}
