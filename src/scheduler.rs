//!
//! # Scheduler
//! 
//! The scheduler provides a new synchronisation mechanism: the `JobQueue`. You can get one
//! by calling `scheduler::queue()`:
//! 
//! ```
//! use desync::scheduler;
//! 
//! let queue = scheduler::queue();
//! ```
//! 
//! A `JobQueue` allows jobs to be scheduled in the background. Jobs are scheduled in the order
//! that they arrive, so anything on a queue is run synchronously with respect to the queue
//! itself. The `async` call can be used to schedule stuff:
//! 
//! ```
//! # use desync::scheduler;
//! # 
//! # let queue = scheduler::queue();
//! # 
//! scheduler::async(&queue, || println!("First job"));
//! scheduler::async(&queue, || println!("Second job"));
//! scheduler::async(&queue, || println!("Third job"));
//! ```
//! 
//! These will be scheduled onto background threads created by the scheduler. There is also a
//! `sync` method. Unlike `async`, this can return a value from the job function it takes
//! as a parameter and doesn't return until its job has completed:
//! 
//! ```
//! # use desync::scheduler;
//! # 
//! # let queue = scheduler::queue();
//! #
//! scheduler::async(&queue, || println!("In the background"));
//! let someval = scheduler::sync(&queue, || { println!("In the foreground"); 42 });
//! # assert!(someval == 42);
//! ```
//! 
//! As queues are synchronous with themselves, it's possible to access data without needing
//! extra synchronisation primitives: `async` is perfect for updating data in the background
//! and `sync` can be used to perform operations where data is returned to the calling thread.
//!

use std::mem;
use std::sync::*;
use std::thread::*;
use std::sync::mpsc::*;
use std::collections::vec_deque::*;

use num_cpus;

const MIN_THREADS: usize = 8;

lazy_static! {
    static ref SCHEDULER: Arc<Scheduler> = Arc::new(Scheduler::new());
}

///
/// The default maximum number of threads in a scheduler 
///
fn initial_max_threads() -> usize {
    MIN_THREADS.max(num_cpus::get()*2)
}

///
/// Trait implemented by things that can be scheduled as a job
/// 
trait ScheduledJob : Send {
    /// Runs this particular job
    fn run(&mut self);
}

///
/// Basic job is just a FnOnce
///
struct Job<TFn> 
where TFn: Send+FnOnce() -> () {
    action: Option<TFn>
}

impl<TFn> Job<TFn> 
where TFn: Send+FnOnce() -> () {
    fn new(action: TFn) -> Job<TFn> {
        Job { action: Some(action) }
    }
}

impl<TFn> ScheduledJob for Job<TFn>
where TFn: Send+FnOnce() -> () {
    fn run(&mut self) {
        // Consume the action when it's run
        let mut action = None;
        mem::swap(&mut action, &mut self.action);

        if let Some(action) = action {
            action();
        } else {
            panic!("Cannot schedule an action twice");
        }
    }
}

///
/// The scheduler is used to schedule tasks onto a pool of threads
///
pub struct Scheduler {
    /// The queues that are active in the scheduler
    queues: Arc<Mutex<Vec<Weak<JobQueue>>>>,

    /// Active threads
    threads: Mutex<Vec<SchedulerThread>>,

    /// The maximum number of threads permitted in this scheduler
    max_threads: Mutex<usize>
}

///
/// A job queue provides a list of jobs to perform in order
/// 
pub struct JobQueue {
    /// Jobs scheduled on this queue
    queue: Mutex<VecDeque<Box<ScheduledJob>>>,

    /// True while this queue is running
    running: Mutex<bool>
}

///
/// A scheduler thread reads from the scheduler queue
///
struct SchedulerThread {
    /// The jobs that this thread should run
    jobs: Sender<Box<ScheduledJob>>,

    /// The thread itself
    thread: JoinHandle<()>,

    /// Flag that indicates that this thread is busy
    busy: Arc<Mutex<bool>>
}

impl SchedulerThread {
    ///
    /// Creates a new scheduler thread 
    ///
    fn new() -> SchedulerThread {
        // All the thread does is run jobs from its channel
        let (jobs_in, jobs_out): (Sender<Box<ScheduledJob>>, Receiver<Box<ScheduledJob>>) = channel();
        let thread = spawn(move || {
            while let Ok(mut job) = jobs_out.recv() {
                job.run();
            }
        });

        SchedulerThread {
            jobs:   jobs_in,
            thread: thread,
            busy:   Arc::new(Mutex::new(false))
        }
    }

    ///
    /// Schedules a job to be run on this thread
    ///
    fn run<Job: 'static+ScheduledJob>(&self, job: Job) {
        self.jobs.send(Box::new(job)).unwrap();
    }
}

impl JobQueue {
    ///
    /// Creates a new job queue 
    ///
    fn new() -> JobQueue {
        JobQueue { 
            queue:      Mutex::new(VecDeque::new()),
            running:    Mutex::new(false)
        }
    }

    ///
    /// Adds a new job to this queue, returns true if the queue is dormant and needs to be started
    ///
    fn queue<TJob: 'static+ScheduledJob>(&self, job: TJob) -> bool {
        let mut queue   = self.queue.lock().unwrap();
        let running     = self.running.lock().unwrap();

        queue.push_back(Box::new(job));

        !*running
    }

    ///
    /// If there are any jobs waiting, dequeues the next one
    ///
    fn dequeue(&self) -> Option<Box<ScheduledJob>> {
        let mut queue = self.queue.lock().unwrap();

        queue.pop_front()
    }

    ///
    /// Runs jobs on this queue until there are none left, marking the job as inactive when done
    /// 
    fn drain(&self) {
        let mut done = false;

        while !done {
            // Run jobs until the queue is drained
            while let Some(mut job) = self.dequeue() {
                job.run();
            }

            // Mark as not running any more
            {
                let queue       = self.queue.lock().unwrap();
                let mut running = self.running.lock().unwrap();

                // If the queue is empty at the point where we obtain the 'running' lock, we can deactivate ourselves
                if queue.len() == 0 {
                    *running    = false;
                    done        = true;
                }
            }
        }
    }
}

impl Scheduler {
    ///
    /// Creates a new scheduler
    /// 
    /// (There's only actually one scheduler)
    /// 
    fn new() -> Scheduler {
        let result = Scheduler { 
            queues:         Arc::new(Mutex::new(vec![])),
            threads:        Mutex::new(vec![]),
            max_threads:    Mutex::new(initial_max_threads())
        };

        result
    }

    ///
    /// Changes the maximum number of threads this scheduler can spawn (existing threads
    /// are not despawned by this method)
    ///
    pub fn set_max_threads(&self, max_threads: usize) {
        // Update the maximum number of threads we can spawn
        { *self.max_threads.lock().unwrap() = max_threads };

        // Try to schedule a thread if we can
        self.schedule_thread();
    }

    ///
    /// Despawns threads if we're running more than the maximum number
    /// 
    /// Must not be called from a scheduler thread (as it waits for the threads to despawn)
    ///
    pub fn despawn_threads_if_overloaded(&self) {
        let max_threads = { *self.max_threads.lock().unwrap() };
        let to_despawn  = {
            // Transfer the threads from the threads vector to our _to_despawn variable
            // This is then dropped outside the mutex (so we don't block if one of the threads doesn't stop)
            let mut to_despawn  = vec![];
            let mut threads     = self.threads.lock().unwrap();

            while threads.len() > max_threads {
                to_despawn.push(threads.pop().unwrap().thread);
            }

            to_despawn
        };

        // Wait for the threads to despawn
        to_despawn.into_iter().for_each(|join_handle| { join_handle.join().ok(); });
    }

    ///
    /// Finds the next queue that should be run. If this returns successfully, the queue will 
    /// be marked as running.
    /// 
    fn next_to_run(queues: &Arc<Mutex<Vec<Weak<JobQueue>>>>) -> Option<Arc<JobQueue>> {
        // Search the queues...
        let queues = queues.lock().unwrap();

        // Find a queue where is_running is false
        for q in queues.iter() {
            // Queue is a weak ref, check that it's still in use
            if let Some(q) = q.upgrade() {
                // If the queue is not running, then mark it as running and return it
                let mut is_running = q.running.lock().unwrap();
                if !*is_running {
                    // Clone here is necessary because the is_running update is borrowing q
                    *is_running = true;
                    return Some(q.clone());
                }
            }
        }

        None
    }

    ///
    /// Attempts to schedule a task on a dormant thread
    ///
    fn schedule_dormant<NextJob, RunJob, JobData>(&self, next_job: NextJob, job: RunJob) -> bool
    where RunJob: 'static+Send+Fn(JobData) -> (), NextJob: 'static+Send+Fn() -> Option<JobData> {
        let threads = self.threads.lock().unwrap();

        // Find the first thread that is not marked as busy and schedule this task on it
        for thread in threads.iter() {
            let mut busy = thread.busy.lock().unwrap();

            if !*busy {
                // Clone the busy mutex so we can return this thread to readiness
                let also_busy = thread.busy.clone();

                // This thread is busy
                *busy = true;
                thread.run(Job::new(move || {
                    let mut done = false;

                    while !done {
                        // Obtain the next job. The thread is not busy once there are no longer any jobs
                        // We hold the mutex while this is going on to avoid a race condition when a thread is going dormant
                        let job_data = {
                            let mut busy = also_busy.lock().unwrap();
                            let job_data = next_job();

                            // If there's no next job, then this thread is no longer busy
                            if job_data.is_none() {
                                *busy = false;
                            }

                            job_data
                        };

                        // Run the job if there is one, stop the thread if there is not
                        if let Some(job_data) = job_data {
                            job(job_data);
                        } else {
                            done = true;
                        }
                    }
                }));

                return true;
            }
        }

        // No dormant threads were found
        false
    }

    ///
    /// If we're running fewer than the maximum number of threads, try to spawn a new one
    ///
    fn spawn_thread_if_less_than_maximum(&self) -> bool {
        let max_threads = { *self.max_threads.lock().unwrap() };
        let mut threads = self.threads.lock().unwrap();

        if threads.len() < max_threads {
            // Create a new thread
            let new_thread = SchedulerThread::new();
            threads.push(new_thread);
            
            true
        } else {
            // Can't spawn a new thread
            false
        }
    }

    ///
    /// Wakes a thread to run a dormant queue. Returns true if a thread was woken up
    ///
    fn schedule_thread(&self) -> bool {
        // Find a dormant thread and activate it
        let queues = self.queues.clone();

        // Schedule work on this dormant thread
        if !self.schedule_dormant(move || Self::next_to_run(&queues), move |work| work.drain()) {
            // Try to create a new thread
            if self.spawn_thread_if_less_than_maximum() {
                // Try harder to schedule this task if a thread was created
                self.schedule_thread()
            } else {
                // Couldn't schedule on an existing thread or create a new one
                false
            }
        } else {
            // Successfully scheduled
            true
        }
    }

    ///
    /// Spawns a thread in this scheduler
    ///
    pub fn spawn_thread(&self) {
        let new_thread = SchedulerThread::new();
        self.threads.lock().unwrap().push(new_thread);
    }

    ///
    /// Creates a new job queue for this scheduler
    ///
    pub fn create_job_queue(&self) -> Arc<JobQueue> {
        // Create the  new queue
        let new_queue = Arc::new(JobQueue::new());

        let mut queues = self.queues.lock().unwrap();

        // Replace an existing weak queue if possible
        let weak_pos = queues.iter().position(|q| q.upgrade().is_none());

        if let Some(weak_pos) = weak_pos {
            // Replace a queue that was present but is no longer in use
            queues[weak_pos] = Arc::downgrade(&new_queue);
        } else {
            // Add to the queues managed by this object (as a weak reference)
            queues.push(Arc::downgrade(&new_queue));
        }

        new_queue
    }

    ///
    /// Schedules a job on this scheduler, which will run after any jobs that are already 
    /// in the specified queue and as soon as a thread is available to run it.
    ///
    pub fn async<TFn: 'static+Send+FnOnce() -> ()>(&self, queue: &Arc<JobQueue>, job: TFn) {
        if queue.queue(Job::new(job)) {
            // A true result indicates that the job was scheduled but the queue is not running. Try to schedule a thread if this occurs.
            self.schedule_thread();
        }
    }

    ///
    /// Schedules a job on this scheduler, which will run after any jobs that are already
    /// in the specified queue. This function will not return until the job has completed.
    ///
    pub fn sync<Result: 'static+Send, TFn: 'static+Send+FnOnce() -> Result>(&self, queue: &Arc<JobQueue>, job: TFn) -> Result {
        enum RunAction {
            /// The queue is empty: call the function directly and don't bother with storing a result
            Immediate,

            /// The queue is not empty but not running: drain on this thread so we get to the sync op
            DrainOnThisThread,

            /// The queue is running in the background
            WaitForBackground
        }

        // If the queue is idle when this is called, we need to schedule this task on this thread rather than one owned by the background process
        let run_action = {
            let queue_data      = queue.queue.lock().unwrap();
            let mut is_running  = queue.running.lock().unwrap();

            if !*is_running {
                // The queue is idle: we're going to run it until this task is done
                *is_running = true;

                if queue_data.len() == 0 {
                    // The queue is idle and has nothing pending: just call the function without scheduling it
                    RunAction::Immediate
                } else {
                    // The queue is idle and has stuff pending: use this thread to actually drain it as the other threads are busy
                    RunAction::DrainOnThisThread
                }
            } else {
                // Queue is running elsewhere, so we need to park this thread instead
                RunAction::WaitForBackground
            }
        };

        match run_action {
            RunAction::Immediate => {
                // Call the function to get the result
                let result = job();

                // Not running any more
                let reschedule = {
                    let queue_data      = queue.queue.lock().unwrap();
                    let mut is_running  = queue.running.lock().unwrap();

                    *is_running = false;

                    // Schedule a thread to restart the queue if more things were queued
                    queue_data.len() > 0
                };

                if reschedule {
                    self.schedule_thread();
                }

                result
            },

            RunAction::DrainOnThisThread => {
                // When the task runs on the queue, we'll put it here
                let result = Arc::new(Mutex::new(None));

                // Queue a job that'll run the requested job and then set the result
                let queue_result = result.clone();
                queue.queue(Job::new(move || {
                    let job_result = job();
                    *queue_result.lock().unwrap() = Some(job_result);
                }));

                // While there is no result, run a job from the queue
                while result.lock().unwrap().is_none() {
                    if let Some(mut job) = queue.dequeue() {
                        job.run();
                    } else {
                        panic!("Queue drained before synchronous job could execute");
                    }
                }

                // Stop running the queue as soon as we have the result
                *queue.running.lock().unwrap() = false;

                // Schedule a thread so the queue can start running again if there are any extra jobs remaining
                self.schedule_thread();

                // Get the final result by swapping it out of the mutex
                let mut final_result    = None;
                let mut old_result      = result.lock().unwrap();

                mem::swap(&mut *old_result, &mut final_result);

                final_result.unwrap()
            },
        
            RunAction::WaitForBackground => {
                // Queue a job that unparks this thread when done
                let pair    = Arc::new((Mutex::new(None), Condvar::new()));
                let pair2   = pair.clone();

                // Signal the condvar when the result is ready
                self.async(queue, move || {
                    let &(ref result, ref cvar) = &*pair2;

                    // Run the job
                    let actual_result = job();

                    // Set the result and notify the waiting thread
                    *result.lock().unwrap() = Some(actual_result);
                    cvar.notify_one();
                });

                // Wait for the result to arrive
                let &(ref lock, ref cvar) = &*pair;
                let mut result = lock.lock().unwrap();
                
                while result.is_none() {
                    result = cvar.wait(result).unwrap();
                }

                // Get the final result by swapping it out of the mutex
                let mut final_result    = None;
                mem::swap(&mut *result, &mut final_result);
                final_result.unwrap()
            }
        }
    }
}

///
/// Retrieves the global scheduler
///
pub fn scheduler<'a>() -> &'a Scheduler {
    &SCHEDULER
}

///
/// Creates a scheduler queue
///
pub fn queue() -> Arc<JobQueue> {
    scheduler().create_job_queue()
}

///
/// Performs an action asynchronously on the specified queue
///
pub fn async<TFn: 'static+Send+FnOnce() -> ()>(queue: &Arc<JobQueue>, job: TFn) {
    scheduler().async(queue, job)
}

///
/// Performs an action synchronously on the specified queue 
///
pub fn sync<Result: 'static+Send, TFn: 'static+Send+FnOnce() -> Result>(queue: &Arc<JobQueue>, job: TFn) -> Result {
    scheduler().sync(queue, job)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::time::*;

    fn timeout<TFn: 'static+Send+FnOnce() -> ()>(action: TFn, millis: u64) {
        let (tx, rx)    = channel();
        let (tx1, tx2)  = (tx.clone(), tx.clone());

        spawn(move || {
            action();
            tx1.send(true).ok();
        });

        spawn(move || {
            sleep(Duration::from_millis(millis));
            tx2.send(false).ok();
        });

        if rx.recv().unwrap() == false {
            panic!("Timeout");
        }
    }

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
                sleep(Duration::from_millis(100));
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
                sleep(Duration::from_millis(100));
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
        }, 500);
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
                sleep(Duration::from_millis(100));
                *async_val.lock().unwrap() = 42;
            });

            // Make sure a thread wakes up and claims the queue before we do
            sleep(Duration::from_millis(10));

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
                sleep(Duration::from_millis(100));
                *async_val.lock().unwrap() = 42;
            });

            let new_val = scheduler.sync(&queue, move || { 
                let v = val.lock().unwrap();
                *v
            });

            assert!(new_val == 42);
        }, 500);
    }
}
