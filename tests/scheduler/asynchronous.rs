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

        desync(&queue, move || {
            tx.send(42).unwrap();
        });

        assert!(rx.recv().unwrap() == 42);
    }, 100);
}

#[test]
#[should_panic]
fn panicking_panics_with_future_queues() {
    timeout(|| {
        let queue       = queue();

        desync(&queue, move || {
            panic!("Oh dear");
        });

        thread::sleep(Duration::from_millis(10));

        desync(&queue, move || {
            println!("Should never get here");
        });
    }, 100);
}

#[test]
#[cfg(not(miri))]   // slow!
fn async_only_runs_once() {
    for _x in 0..1000 {
        timeout(|| {
            let num_runs    = Arc::new(Mutex::new(0));
            let queue       = queue();

            let num_runs2   = num_runs.clone();
            desync(&queue, move || {
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
#[cfg(not(miri))]   // slow!
fn async_runs_in_order_1000_iter() {
    for _x in 0..1000 {
        timeout(|| {
            let (tx, rx)    = channel();
            let queue       = queue();

            for iter in 0..10 {
                let tx = tx.clone();
                desync(&queue, move || {
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

            desync(&queue1, move || {
                tx.send(42).unwrap();
            });

            assert!(rx.recv().unwrap() == 42);
        }

        {
            let (tx, rx)    = channel();
            let queue2      = queue();

            desync(&queue2, move || {
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

        desync(&queue, move || {
            thread::sleep(Duration::from_millis(100));
            tx1.send(1).unwrap();
        });
        desync(&queue, move || {
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

        desync(&queue1, move || {
            // The other task needs to start within 100ms for this to work
            thread::sleep(Duration::from_millis(100));
            tx.send(*queue1_check.lock().unwrap()).unwrap();
        });
        desync(&queue2, move || {
            *queue2_has_run.lock().unwrap() = true;
        });

        assert!(rx.recv().unwrap() == true);
    }, 500);
}
