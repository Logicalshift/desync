pub mod timeout;

extern crate desync;
extern crate futures;

use desync::scheduler::*;

use self::timeout::*;

use std::thread;
use std::time::*;
use std::sync::*;
use std::sync::mpsc::*;

use futures::sync::oneshot;

#[test]
fn can_schedule_async() {
    timeout(|| {
        let (tx, rx)    = channel();
        let queue       = queue();

        async(&queue, move || {
            tx.send(42).unwrap();
        });

        assert!(rx.recv().unwrap() == 42);
    }, 100);
}

#[test]
fn async_only_runs_once() {
    for _x in 0..1000 {
        timeout(|| {
            let num_runs    = Arc::new(Mutex::new(0));
            let queue       = queue();

            let num_runs2   = num_runs.clone();
            async(&queue, move || {
                let mut num_runs = num_runs2.lock().unwrap();
                *num_runs += 1
            });

            while *num_runs.lock().unwrap() == 0 {
                thread::sleep(Duration::from_millis(1));
            }

            assert!(*num_runs.lock().unwrap() == 1);
        }, 100);
    }
}

#[test]
fn async_runs_in_order_1000_iter() {
    for _x in 0..1000 {
        timeout(|| {
            let (tx, rx)    = channel();
            let queue       = queue();

            for iter in 0..10 {
                let tx = tx.clone();
                async(&queue, move || {
                    tx.send(iter).unwrap();
                });
            }

            for iter in 0..10 {
                assert!(rx.recv().unwrap() == iter);
            }

            assert!(sync(&queue, || 42) == 42);
        }, 500);
    }
}

#[test]
fn can_schedule_after_queue_released() {
    timeout(|| {
        {
            let (tx, rx)    = channel();
            let queue1      = queue();

            async(&queue1, move || {
                tx.send(42).unwrap();
            });

            assert!(rx.recv().unwrap() == 42);
        }

        {
            let (tx, rx)    = channel();
            let queue2      = queue();

            async(&queue2, move || {
                tx.send(43).unwrap();
            });

            assert!(rx.recv().unwrap() == 43);
        }
    }, 100);
}

#[test]
fn will_schedule_in_order() {
    timeout(|| {
        let (tx, rx)    = channel();
        let queue       = queue();

        let (tx1, tx2)  = (tx.clone(), tx.clone());

        async(&queue, move || {
            thread::sleep(Duration::from_millis(100));
            tx1.send(1).unwrap();
        });
        async(&queue, move || {
            tx2.send(2).unwrap();
        });

        assert!(rx.recv().unwrap() == 1);
        assert!(rx.recv().unwrap() == 2);
    }, 500);
}

#[test]
fn will_schedule_separate_queues_in_parallel() {
    timeout(|| {
        let (tx, rx)        = channel();
        let queue1          = queue();
        let queue2          = queue();
        let queue2_has_run  = Arc::new(Mutex::new(false));

        let queue1_check = queue2_has_run.clone();

        async(&queue1, move || {
            // The other task needs to start within 100ms for this to work
            thread::sleep(Duration::from_millis(100));
            tx.send(*queue1_check.lock().unwrap()).unwrap();
        });
        async(&queue2, move || {
            *queue2_has_run.lock().unwrap() = true;
        });

        assert!(rx.recv().unwrap() == true);
    }, 500);
}

#[test]
fn will_despawn_extra_threads() {
    // As we join with the threads, we'll timeout if any of the spawned threads fail to end
    timeout(|| {
        let scheduler = scheduler();

        // Maximum of 10 threads, but we'll spawn 20
        scheduler.set_max_threads(10);
        for _ in 1..20 {
            scheduler.spawn_thread();
        }

        scheduler.despawn_threads_if_overloaded();
    }, 30000);
}

#[test]
fn can_schedule_sync() {
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
fn can_reschedule_after_immediate_sync() {
    timeout(|| {
        let (tx, rx)    = channel();
        let queue       = queue();
        let queue_ref   = queue.clone();

        let new_val = sync(&queue, move || {
            async(&queue_ref, move || {
                tx.send(43).unwrap();
            });

            42
        });

        assert!(new_val == 42);
        assert!(rx.recv().unwrap() == 43);
    }, 500);
}

#[test]
fn can_schedule_sync_after_async() {
    timeout(|| {
        let val         = Arc::new(Mutex::new(0));
        let queue       = queue();

        let async_val = val.clone();
        async(&queue, move || {
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
        scheduler.async(&queue, move || {
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

#[test]
fn can_suspend_queue() {
    for _x in 0..1000 {
        timeout(|| {
            let queue           = queue();
            let scheduler       = scheduler();
            let (tx, rx)        = channel();

            let pos             = Arc::new(Mutex::new(0));

            // Send the current position when the async methods run
            let pos2 = pos.clone();
            let tx2 = tx.clone();
            async(&queue, move || { tx2.send(*pos2.lock().unwrap()).unwrap(); });

            // Suspend after the first send
            scheduler.suspend(&queue);

            // Send agin
            let pos2 = pos.clone();
            let tx2 = tx.clone();
            async(&queue, move || { tx2.send(*pos2.lock().unwrap()).unwrap(); });

            // Wait for the first queue to send
            assert!(rx.recv().unwrap() == 0);

            thread::yield_now();

            // Update the position and resume
            *pos.lock().unwrap() = 1;
            scheduler.resume(&queue);

            // The resumption will send us a value when it occurs
            assert!(rx.recv().unwrap() == 1);
        }, 500);
    }
}

#[test]
fn can_suspend_queue_with_local_drain() {
    timeout(|| {
        // Want a scheduler with 0 threads to force a 'drain on current thread' situation
        let scheduler = Arc::new(Scheduler::new());
        scheduler.set_max_threads(0);
        scheduler.despawn_threads_if_overloaded();

        let queue = scheduler.create_job_queue();
        
        // Job so there's something to drain
        scheduler.async(&queue, ||{});
        scheduler.suspend(&queue);

        // Resume after a delay
        let to_resume           = queue.clone();
        let resume_scheduler    = scheduler.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            resume_scheduler.resume(&to_resume);

            // The scheduler will need to be able to finish the task on a thread
            resume_scheduler.set_max_threads(1);
        });

        // Should be able to retrieve a value once the queue resumes
        assert!(scheduler.sync(&queue, || 42) == 42);
    }, 500);
}

#[test]
fn can_resume_before_suspend() {
    for _x in 0..1000 {
        timeout(|| {
            let (tx, rx)    = channel();
            let queue       = queue();
            let scheduler   = scheduler();

            // Increment the position, suspend the queue, increment it again
            let tx2         = tx.clone();
            async(&queue, move || { tx2.send(1).unwrap(); });
            scheduler.resume(&queue);
            scheduler.suspend(&queue);
            let tx2         = tx.clone();
            async(&queue, move || { tx2.send(2).unwrap(); });

            assert!(rx.recv_timeout(Duration::from_millis(100)) == Ok(1));
            assert!(rx.recv_timeout(Duration::from_millis(100)) == Ok(2));
        }, 500);
    }
}

#[test]
fn safe_to_drop_suspended_queue() {
    timeout(|| {
        let queue       = queue();
        let scheduler   = scheduler();

        let pos         = Arc::new(Mutex::new(0));

        // Increment the position, suspend the queue, increment it again
        let pos2        = pos.clone();
        async(&queue, move || { let mut pos2 = pos2.lock().unwrap(); *pos2 += 1 });
        scheduler.suspend(&queue);
        let pos2        = pos.clone();
        async(&queue, move || { let mut pos2 = pos2.lock().unwrap(); *pos2 += 1 });

        // Wait for long enough for these events to take place and check the queue
        while *pos.lock().unwrap() == 0 {
            thread::sleep(Duration::from_millis(100));
        }
        assert!(*pos.lock().unwrap() == 1);
    }, 500);
}
