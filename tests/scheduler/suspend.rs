use desync::scheduler::*;

use super::timeout::*;

use std::thread;
use std::time::*;
use std::sync::*;
use std::sync::mpsc::*;

#[test]
fn suspend_queue() {
    for _x in 0..1000 {
        timeout(|| {
            let queue           = queue();
            let scheduler       = scheduler();
            let (tx, rx)        = channel();

            let pos             = Arc::new(Mutex::new(0));

            // Send the current position when the async methods run
            let pos2 = pos.clone();
            let tx2 = tx.clone();
            desync(&queue, move || { tx2.send(*pos2.lock().unwrap()).unwrap(); });

            // Suspend after the first send
            scheduler.suspend(&queue);

            // Send agin
            let pos2 = pos.clone();
            let tx2 = tx.clone();
            desync(&queue, move || { tx2.send(*pos2.lock().unwrap()).unwrap(); });

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
fn suspend_queue_with_local_drain() {
    timeout(|| {
        // Want a scheduler with 0 threads to force a 'drain on current thread' situation
        let scheduler = Arc::new(Scheduler::new());
        scheduler.set_max_threads(0);
        scheduler.despawn_threads_if_overloaded();

        let queue = scheduler.create_job_queue();
        
        // Job so there's something to drain
        scheduler.desync(&queue, ||{});
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
fn resume_before_suspend() {
    for _x in 0..1000 {
        timeout(|| {
            let (tx, rx)    = channel();
            let queue       = queue();
            let scheduler   = scheduler();

            // Increment the position, suspend the queue, increment it again
            let tx2         = tx.clone();
            desync(&queue, move || { tx2.send(1).unwrap(); });
            scheduler.resume(&queue);
            scheduler.suspend(&queue);
            let tx2         = tx.clone();
            desync(&queue, move || { tx2.send(2).unwrap(); });

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
        desync(&queue, move || { let mut pos2 = pos2.lock().unwrap(); *pos2 += 1 });
        scheduler.suspend(&queue);
        let pos2        = pos.clone();
        desync(&queue, move || { let mut pos2 = pos2.lock().unwrap(); *pos2 += 1 });

        // Wait for long enough for these events to take place and check the queue
        while *pos.lock().unwrap() == 0 {
            thread::sleep(Duration::from_millis(100));
        }
        assert!(*pos.lock().unwrap() == 1);
    }, 500);
}
