use desync::scheduler::*;

use super::timeout::*;

use std::thread;
use std::time::*;
use std::sync::*;
use std::sync::mpsc::*;

#[test]
fn schedule_async() {
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
fn schedule_after_queue_released() {
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
