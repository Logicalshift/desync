use desync::scheduler::*;

use super::timeout::*;

use std::thread;
use std::time::*;
use std::sync::*;
use std::sync::mpsc::*;

#[test]
fn schedule_sync() {
    timeout(|| {
        let queue   = queue();

        let new_val = sync(&queue, move || 42);

        assert!(new_val == 42);
    }, 100);
}

#[test]
fn sync_has_synchronous_lifetime() {
    timeout(|| {
        let queue           = queue();
        let mut some_val    = 0;

        {
            let some_val_ref = &mut some_val;
            sync(&queue, move || *some_val_ref = 42);
        }

        assert!(some_val == 42);
    }, 100);
}

#[test]
fn reschedule_after_immediate_sync() {
    timeout(|| {
        let (tx, rx)    = channel();
        let queue       = queue();
        let queue_ref   = queue.clone();

        let new_val = sync(&queue, move || {
            desync(&queue_ref, move || {
                tx.send(43).unwrap();
            });

            42
        });

        assert!(new_val == 42);
        assert!(rx.recv().unwrap() == 43);
    }, 500);
}

#[test]
fn schedule_sync_after_async() {
    timeout(|| {
        let val         = Arc::new(Mutex::new(0));
        let queue       = queue();

        let async_val = val.clone();
        desync(&queue, move || {
            thread::sleep(Duration::from_millis(100));
            *async_val.lock().unwrap() = 42;
        });

        // Make sure a thread wakes up and claims the queue before we do
        thread::sleep(Duration::from_millis(10));

        let new_val = sync(&queue, move || { 
            let v = val.lock().unwrap();
            *v
        });

        assert!(new_val == 42);
    }, 500);
}

#[test]
fn sync_drains_with_no_threads() {
    timeout(|| {
        let scheduler   = Scheduler::new();
        let val         = Arc::new(Mutex::new(0));
        let queue       = queue();

        // Even with 0 threads, sync actions should still run (by draining on the current thread)
        scheduler.set_max_threads(0);
        scheduler.despawn_threads_if_overloaded();

        let async_val = val.clone();
        scheduler.desync(&queue, move || {
            thread::sleep(Duration::from_millis(100));
            *async_val.lock().unwrap() = 42;
        });

        let new_val = scheduler.sync(&queue, move || { 
            let v = val.lock().unwrap();
            *v
        });

        assert!(new_val == 42);
    }, 500);
}
