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

// TODO: need to make it safe to drop a suspended queue (well, a suspended Desync)
// TODO: move tests into a separate file
// TODO: test suspension when in 'drain on this thread' mode for sync() calls
// TODO: this can be implemented much more simply with GCD on OS X but that's not available on all platforms
// TODO: handle panicking threads better

use super::job::*;
use super::unsafe_job::*;
use super::scheduler_thread::*;

use std::mem;
use std::fmt;
use std::sync::*;
use std::collections::vec_deque::*;

use num_cpus;
use futures::future::Future;
use futures::sync::oneshot;

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
/// The scheduler is used to schedule tasks onto a pool of threads
///
pub struct Scheduler {
    /// The queues that are active in the scheduler
    schedule: Arc<Mutex<VecDeque<Arc<JobQueue>>>>,

    /// Active threads and whether or not they're busy
    threads: Mutex<Vec<(Arc<Mutex<bool>>, SchedulerThread)>>,

    /// The maximum number of threads permitted in this scheduler
    max_threads: Mutex<usize>
}

///
/// Represents the state of a job queue
///
#[derive(PartialEq, Debug, Clone, Copy)]
enum QueueState {
    /// Queue is currently not running and not ready to run
    Idle,

    /// Queue has been queued up to run but isn't running yet
    Pending,

    /// Queue has been assigned to a thread and is currently running
    Running,

    /// Queue is running but should suspend instead of running the next step
    Suspending,

    /// Queue has been suspended and won't run futher jobs
    Suspended
}

///
/// Structure protected by the jobqueue matrix
///
struct JobQueueCore {
    /// The jobs that are scheduled on this queue
    queue: VecDeque<Box<ScheduledJob>>,

    /// The current state of this queue
    state: QueueState,
    
    /// How many times this queue has been suspended (can be negative to indicate the suspension ended before it began)
    suspension_count: i32
}

///
/// A job queue provides a list of jobs to perform in order
/// 
pub struct JobQueue {
    /// The shared data for this queue is stored within a mutex
    core: Mutex<JobQueueCore>
}

impl fmt::Debug for JobQueue {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        let core = self.core.lock().expect("JobQueue core lock");

        fmt.write_str(&format!("JobQueue: State: {:?}, Pending: {}", core.state, core.queue.len()))
    }
}

impl JobQueue {
    ///
    /// Creates a new job queue 
    ///
    fn new() -> JobQueue {
        JobQueue { 
            core: Mutex::new(JobQueueCore {
                queue:              VecDeque::new(),
                state:              QueueState::Idle,
                suspension_count:   0
            })
        }
    }

    ///
    /// If there are any jobs waiting, dequeues the next one
    ///
    fn dequeue(&self) -> Option<Box<ScheduledJob>> {
        let mut core = self.core.lock().expect("JobQueue core lock");

        if core.state == QueueState::Suspending {
            // Stop dequeuing if the queue is suspending
            None
        } else {
            // Treat queue as running in all other states
            debug_assert!(core.state == QueueState::Running);
            core.queue.pop_front()
        }
    }

    ///
    /// Runs jobs on this queue until there are none left, marking the job as inactive when done
    /// 
    fn drain(&self) {
        debug_assert!(self.core.lock().unwrap().state == QueueState::Running);
        let mut done = false;

        while !done {
            // Run jobs until the queue is drained
            while let Some(mut job) = self.dequeue() {
                debug_assert!(self.core.lock().unwrap().state == QueueState::Running);
                job.run();
            }

            // Try to move back to the 'not running' state
            {
                let mut core = self.core.lock().expect("JobQueue core lock");
                debug_assert!(core.state == QueueState::Running || core.state == QueueState::Suspending);

                // If the queue is empty at the point where we obtain the lock, we can deactivate ourselves
                if core.queue.len() == 0 {
                    core.state = match core.state {
                        QueueState::Running     => QueueState::Idle,
                        QueueState::Suspending  => QueueState::Suspended,
                        x                       => x
                    };
                    done = true;
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
            schedule:       Arc::new(Mutex::new(VecDeque::new())),
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
        { *self.max_threads.lock().expect("Max threads lock") = max_threads };

        // Schedule as many threads as we can
        while self.schedule_thread() {}
    }

    ///
    /// Despawns threads if we're running more than the maximum number
    /// 
    /// Must not be called from a scheduler thread (as it waits for the threads to despawn)
    ///
    pub fn despawn_threads_if_overloaded(&self) {
        let max_threads = { *self.max_threads.lock().expect("Max threads lock") };
        let to_despawn  = {
            // Transfer the threads from the threads vector to our _to_despawn variable
            // This is then dropped outside the mutex (so we don't block if one of the threads doesn't stop)
            let mut to_despawn  = vec![];
            let mut threads     = self.threads.lock().expect("Scheduler threads lock");

            while threads.len() > max_threads {
                to_despawn.push(threads.pop().expect("Missing threads").1.despawn());
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
    fn next_to_run(schedule: &Arc<Mutex<VecDeque<Arc<JobQueue>>>>) -> Option<Arc<JobQueue>> {
        // Search the queues...
        let mut schedule = schedule.lock().expect("Schedule lock");

        // Find a queue where the state is pending
        while let Some(q) = schedule.pop_front() {
            let mut core = q.core.lock().expect("JobQueue core lock");

            if core.state == QueueState::Pending {
                // Queue is ready to run. Mark it as running and return it
                core.state = QueueState::Running;
                return Some(q.clone());
            }
        }

        None
    }

    ///
    /// Attempts to schedule a task on a dormant thread
    ///
    fn schedule_dormant<NextJob, RunJob, JobData>(&self, next_job: NextJob, job: RunJob) -> bool
    where RunJob: 'static+Send+Fn(JobData) -> (), NextJob: 'static+Send+Fn() -> Option<JobData> {
        let threads = self.threads.lock().expect("Scheduler threads lock");

        // Find the first thread that is not marked as busy and schedule this task on it
        for &(ref busy_rc, ref thread) in threads.iter() {
            let mut busy = busy_rc.lock().expect("Thread busy lock");

            if !*busy {
                // Clone the busy mutex so we can return this thread to readiness
                let also_busy =  busy_rc.clone();

                // This thread is busy
                *busy = true;
                thread.run(Job::new(move || {
                    let mut done = false;

                    while !done {
                        // Obtain the next job. The thread is not busy once there are no longer any jobs
                        // We hold the mutex while this is going on to avoid a race condition when a thread is going dormant
                        let job_data = {
                            let mut busy = also_busy.lock().expect("Thread busy lock");
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
        let max_threads = { *self.max_threads.lock().expect("Max threads lock") };
        let mut threads = self.threads.lock().expect("Scheduler threads lock");

        if threads.len() < max_threads {
            // Create a new thread
            let is_busy     = Arc::new(Mutex::new(false));
            let new_thread  = SchedulerThread::new();
            threads.push((is_busy, new_thread));
            
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
        let schedule = self.schedule.clone();

        // Schedule work on this dormant thread
        if !self.schedule_dormant(move || Self::next_to_run(&schedule), move |work| work.drain()) {
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
    /// If a queue is idle and has pending jobs, places it in the schedule
    ///
    fn reschedule_queue(&self, queue: &Arc<JobQueue>) {
        let reschedule = {
            let mut core = queue.core.lock().expect("JobQueue core lock");

            if core.state == QueueState::Idle {
                // Schedule a thread to restart the queue if more things were queued
                if core.queue.len() > 0 {
                    // Need to schedule the queue after this event
                    core.state = QueueState::Pending;
                    true
                } else {
                    // Queue is empty and can go back to idle
                    core.state = QueueState::Idle;
                    false
                }
            } else {
                false
            }
        };

        if reschedule {
            self.schedule.lock().expect("Schedule lock").push_back(queue.clone());
            self.schedule_thread();
        }
    }

    ///
    /// Spawns a thread in this scheduler
    ///
    pub fn spawn_thread(&self) {
        let is_busy     = Arc::new(Mutex::new(false));
        let new_thread  = SchedulerThread::new();
        self.threads.lock().expect("Scheduler threads lock").push((is_busy, new_thread));
    }

    ///
    /// Creates a new job queue for this scheduler
    ///
    pub fn create_job_queue(&self) -> Arc<JobQueue> {
        let new_queue = Arc::new(JobQueue::new());
        new_queue
    }

    ///
    /// Schedules a job on this scheduler, which will run after any jobs that are already 
    /// in the specified queue and as soon as a thread is available to run it.
    ///
    pub fn async<TFn: 'static+Send+FnOnce() -> ()>(&self, queue: &Arc<JobQueue>, job: TFn) {
        let schedule_queue = {
            let job         = Job::new(job);
            let mut core    = queue.core.lock().expect("JobQueue core lock");

            // Push the job onto the queue
            core.queue.push_back(Box::new(job));

            if core.state == QueueState::Idle {
                // If the queue is idle, then move it to pending
                core.state = QueueState::Pending;
                true
            } else {
                // If the queue is in any other state, then we leave it alone
                false
            }
        };

        // If when we were queuing the jobs we found that the queue was idle, then move it to the pending list
        if schedule_queue {
            // Add the queue to the schedule
            self.schedule.lock().expect("Schedule lock").push_back(queue.clone());

            // Wake up a thread to run it if we can
            self.schedule_thread();
        }
    }

    ///
    /// Schedules a job to run and returns a future for retrieving the result
    ///
    pub fn future<TFn, Item: 'static+Send>(&self, queue: &Arc<JobQueue>, job: TFn) -> Box<Future<Item=Item, Error=oneshot::Canceled>>
    where TFn: 'static+Send+FnOnce() -> Item {
        let (send, receive) = oneshot::channel();

        self.async(queue, move || {
            let res = job();
            send.send(res).ok();
        });

        Box::new(receive)
    }

    ///
    /// Requests that a queue be suspended once it has finished all of its active jobs
    ///
    pub fn suspend(&self, queue: &Arc<JobQueue>) {
        let to_suspend = queue.clone();

        self.async(queue, move || {
            // Mark the queue as suspending
            let mut core = to_suspend.core.lock().expect("JobQueue core lock");

            debug_assert!(core.state == QueueState::Running);

            // Only actually suspend the core if it hasn't already been resumed elsewhere
            core.suspension_count += 1;
            if core.suspension_count > 0 {
                core.state = QueueState::Suspending;
            }
        });
    }

    ///
    /// Resumes a queue that was previously suspended
    ///
    pub fn resume(&self, queue: &Arc<JobQueue>) {
        // Reduce the amount of suspension used by a queue
        // TODO: this is currently fairly unsafe as we can call resume extra times or not at all
        // TODO: better might be to return a token from suspend that we can use to resume the queue (problem is: rescheduling in the right place)
        let needs_reschedule = {
            let mut core = queue.core.lock().expect("JobQueue core lock");

            // Queue becomes less suspended
            core.suspension_count -= 1;
            if core.suspension_count <= 0 {
                match core.state {
                    QueueState::Suspended => {
                        // If the queue was suspended and should no longer be, return it to the idle state
                        core.state = QueueState::Idle;
                        true
                    },
                    QueueState::Suspending => {
                        // If the queue was in the process of suspending, cancel that
                        // and resume running
                        core.state = QueueState::Running;
                        false
                    },
                    _ => false
                }
            } else {
                false
            }
        };

        if needs_reschedule {
            self.reschedule_queue(queue);
        }
    }

    ///
    /// Runs a sync job immediately on the current thread. Queue must be in Running mode for this to be valid
    ///
    fn sync_immediate<Result, TFn: FnOnce() -> Result>(&self, queue: &Arc<JobQueue>, job: TFn) -> Result {
        debug_assert!(queue.core.lock().expect("JobQueue core lock").state == QueueState::Running);

        // Call the function to get the result
        let result = job();

        // Queue is now idle
        queue.core.lock().expect("JobQueue core lock").state = QueueState::Idle;

        // Not running any more
        self.reschedule_queue(queue);

        result
    }

    ///
    /// Runs a sync job immediately by running all the jobs in the current queue 
    ///
    fn sync_drain<Result: Send, TFn: Send+FnOnce() -> Result>(&self, queue: &Arc<JobQueue>, job: TFn) -> Result {
        debug_assert!(queue.core.lock().expect("JobQueue core lock").state == QueueState::Running);

        // When the task runs on the queue, we'll put it here
        let result = Arc::new((Mutex::new(None), Condvar::new()));

        // Queue a job that'll run the requested job and then set the result
        // We'll unpark the thread in case we need to handle a suspension
        let queue_result        = result.clone();
        let result_job          = Box::new(Job::new(move || {
            let job_result = job();
            *queue_result.0.lock().expect("Sync queue result lock") = Some(job_result);
            queue_result.1.notify_one();
        }));

        // Stuff on the queue normally has a 'static lifetime. When we're running
        // sync, the task will be done by the time this method is finished, so
        // we use an unsafe job to bypass the normal lifetime checking
        let unsafe_result_job   = UnsafeJob::new(&*result_job);
        queue.core.lock().expect("JobQueue core lock").queue.push_back(Box::new(unsafe_result_job));

        // While there is no result, run a job from the queue
        while result.0.lock().expect("Sync queue result lock").is_none() {
            if let Some(mut job) = queue.dequeue() {
                // Queue is running
                debug_assert!(queue.core.lock().unwrap().state != QueueState::Suspended);
                job.run();
            } else {
                // Queue may have suspended (or gone to suspending and back to running)
                let wait_in_background = {
                    let mut core = queue.core.lock().expect("JobQueue core lock");
                    if core.state == QueueState::Suspending {
                        // Finish suspension, then wait for job to complete
                        core.state = QueueState::Suspended;
                        true
                    } else {
                        // Queue is still running
                        debug_assert!(core.state == QueueState::Running);
                        false
                    }
                };

                if wait_in_background {
                    // After we ran the thread, it suspended. It will be rescheduled in the background before it runs.
                    while result.0.lock().expect("Sync queue result lock").is_none() {
                        // Park until the result becomes available
                        let parking = &result.1;
                        let result  = result.0.lock().unwrap();
                        let _result = parking.wait(result).unwrap();
                    }
                }
            }
        }

        // Reschedule the queue if there are any events left pending
        // Note: the queue is already pending when we start running events from it here.
        // This means it'll get dequeued by a thread eventually: maybe while it's running
        // here. As we've set the queue state to running while we're busy, the thread won't
        // start the queue while it's already running.
        queue.core.lock().expect("JobQueue core lock").state = QueueState::Idle;
        self.reschedule_queue(queue);

        // Get the final result by swapping it out of the mutex
        let mut final_result    = None;
        let mut old_result      = result.0.lock().expect("Sync queue result lock");

        mem::swap(&mut *old_result, &mut final_result);

        final_result.expect("Finished sync request without result")
    }

    ///
    /// Queues a sync job and waits for the queue to finish running 
    ///
    fn sync_background<Result: Send, TFn: Send+FnOnce() -> Result>(&self, queue: &Arc<JobQueue>, job: TFn) -> Result {
        // Queue a job that unparks this thread when done
        let pair    = Arc::new((Mutex::new(None), Condvar::new()));
        let pair2   = pair.clone();

        // Safe job that signals the condvar when needed
        let job     = Box::new(Job::new(move || {
            let &(ref result, ref cvar) = &*pair2;

            // Run the job
            let actual_result = job();

            // Set the result and notify the waiting thread
            *result.lock().expect("Background job result lock") = Some(actual_result);
            cvar.notify_one();
        }));
        
        // Unsafe job with unbounded lifetime is needed because stuff on the queue normally needs a static lifetime
        let need_reschedule = {
            // Schedule the job and see if the queue went back to 'idle'. Reschedule if it is.
            let unsafe_job  = Box::new(UnsafeJob::new(&*job));
            let mut core    = queue.core.lock().expect("JobQueue core lock");

            core.queue.push_back(unsafe_job);
            core.state == QueueState::Idle
        };
        if need_reschedule { self.reschedule_queue(queue); }

        // Wait for the result to arrive (and the sweet relief of no more unsafe job)
        let &(ref lock, ref cvar) = &*pair;
        let mut result = lock.lock().expect("Background job result lock");
        
        while result.is_none() {
            result = cvar.wait(result).expect("Background job cvar wait");
        }

        // Get the final result by swapping it out of the mutex
        let mut final_result    = None;
        mem::swap(&mut *result, &mut final_result);
        final_result.expect("Finished background sync job without result")
    }

    ///
    /// Schedules a job on this scheduler, which will run after any jobs that are already
    /// in the specified queue. This function will not return until the job has completed.
    ///
    pub fn sync<Result: Send, TFn: Send+FnOnce() -> Result>(&self, queue: &Arc<JobQueue>, job: TFn) -> Result {
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
            let mut core = queue.core.lock().expect("JobQueue core lock");

            match core.state {
                QueueState::Suspended   => RunAction::WaitForBackground,
                QueueState::Suspending  => RunAction::WaitForBackground,
                QueueState::Running     => RunAction::WaitForBackground,
                QueueState::Pending     => { core.state = QueueState::Running; RunAction::DrainOnThisThread },
                QueueState::Idle        => { core.state = QueueState::Running; RunAction::Immediate }
            }
        };

        match run_action {
            RunAction::Immediate            => self.sync_immediate(queue, job),
            RunAction::DrainOnThisThread    => self.sync_drain(queue, job),
            RunAction::WaitForBackground    => self.sync_background(queue, job)
        }
    }
}

impl fmt::Debug for Scheduler {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        let threads = {
            let threads         = self.threads.lock().expect("Scheduler threads lock");
            let busyness:String = threads.iter().map(|&(ref busy, _)| { if *busy.lock().expect("Thread busy lock") { 'B' } else { 'I' } }).collect();

            busyness
        };
        let queue_size = format!("Pending queue count: {}", self.schedule.lock().expect("Schedule lock").len());

        fmt.write_str(&format!("{} {}", threads, queue_size))
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
/// Schedules a job to run and returns a future for retrieving the result
///
pub fn future<TFn, Item: 'static+Send>(queue: &Arc<JobQueue>, job: TFn) -> Box<Future<Item=Item, Error=oneshot::Canceled>>
where TFn: 'static+Send+FnOnce() -> Item {
    scheduler().future(queue, job)
}

///
/// Performs an action synchronously on the specified queue 
///
pub fn sync<Result: Send, TFn: Send+FnOnce() -> Result>(queue: &Arc<JobQueue>, job: TFn) -> Result {
    scheduler().sync(queue, job)
}

#[cfg(test)]
pub mod test {
    use super::*;
    use std::thread;
    use std::time::*;
    use std::sync::mpsc::*;

    pub fn timeout<TFn: 'static+Send+FnOnce() -> ()>(action: TFn, millis: u64) {
        enum ThreadState {
            Ok,
            Timeout,
            Panic
        };

        let (tx, rx)    = channel();
        let (tx1, tx2)  = (tx.clone(), tx.clone());

        thread::Builder::new()
            .name("test timeout thread".to_string())
            .spawn(move || {
                struct DetectPanic(Sender<ThreadState>);
                impl Drop for DetectPanic {
                    fn drop(&mut self) {
                        if thread::panicking() {
                            self.0.send(ThreadState::Panic).ok();
                        }
                    }
                }

                let _detectpanic = DetectPanic(tx1.clone());

                action();
                tx1.send(ThreadState::Ok).ok();
            })
            .expect("Create timeout run thread");

        let (timer_done, timer_done_recv) = channel();
        let timer = thread::Builder::new()
            .name("timeout thread".to_string())
            .spawn(move || {
                let done = timer_done_recv.recv_timeout(Duration::from_millis(millis));
                if done.is_err() {
                    tx2.send(ThreadState::Timeout).ok();
                }
            }).expect("Create timeout timer thread");

        match rx.recv().expect("Receive timeout status") {
            ThreadState::Ok => {
                // Stop the timer thread
                timer_done.send(()).expect("Stop timer");
                timer.join().expect("Wait for timer to stop");
            },
            ThreadState::Timeout => {
                println!("{:?}", scheduler());
                panic!("Timeout");
            },
            ThreadState::Panic => {
                println!("{:?}", scheduler());
                panic!("Timed thread panicked");
            }
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
}
