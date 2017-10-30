//!
//! # Scheduler
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
    MIN_THREADS.min(num_cpus::get()*2)
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

        result.spawn_thread();
        result.spawn_thread();
        result.spawn_thread();
        result.spawn_thread();

        result
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

        for thread in threads.iter() {
            let mut busy = thread.busy.lock().unwrap();

            if !*busy {
                let also_busy = thread.busy.clone();

                *busy = true;
                thread.run(Job::new(move || {
                    let mut done = false;

                    while !done {
                        // Obtain the next job
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
    /// Wakes a thread to run a dormant queue. Returns true if a thread was woken up
    ///
    fn schedule_thread(&self) -> bool {
        // Find a dormant thread and activate it
        let queues = self.queues.clone();

        // Schedule work on this dormant thread
        self.schedule_dormant(move || Self::next_to_run(&queues), move |work| work.drain())
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
    /// Schedules a job on this scheduler
    ///
    pub fn async<TFn: 'static+Send+FnOnce() -> ()>(&self, queue: &Arc<JobQueue>, job: TFn) {
        if queue.queue(Job::new(job)) {
            // A true result indicates that the job was scheduled but the queue is not running. Try to schedule a thread if this occurs.
            self.schedule_thread();
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
/// Creates a scheduler queue
///
pub fn async<TFn: 'static+Send+FnOnce() -> ()>(queue: &Arc<JobQueue>, job: TFn) {
    scheduler().async(queue, job)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::time::*;

    #[test]
    fn can_schedule_async() {
        let (tx, rx)    = channel();
        let queue       = queue();

        async(&queue, move || {
            tx.send(42).unwrap();
        });

        assert!(rx.recv().unwrap() == 42);
    }

    #[test]
    fn can_schedule_after_queue_released() {
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
    }

    #[test]
    fn will_schedule_in_order() {
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
    }

    #[test]
    fn will_schedule_separate_queues_in_parallel() {
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
    }
}
