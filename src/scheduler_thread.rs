use std::thread;
use std::sync::mpsc::*;

///
/// A scheduler thread reads from the scheduler queue
///
pub struct SchedulerThread {
    /// The jobs that this thread should run
    jobs: Sender<Box<dyn FnOnce() -> ()+Send>>,

    /// The thread itself
    thread: thread::JoinHandle<()>,
}

impl SchedulerThread {
    ///
    /// Creates a new scheduler thread 
    ///
    pub fn new() -> SchedulerThread {
        // All the thread does is run jobs from its channel
        let (jobs_in, jobs_out): (Sender<Box<dyn FnOnce() -> ()+Send>>, Receiver<Box<dyn FnOnce() -> ()+Send>>) = channel();
        let thread = thread::Builder::new()
            .name("desync jobs thread".to_string())
            .spawn(move || {
                while let Ok(mut job) = jobs_out.recv() {
                    (*job)();
                }
            }).unwrap();

        SchedulerThread {
            jobs:   jobs_in,
            thread: thread
        }
    }

    ///
    /// Schedules a job to be run on this thread
    ///
    pub fn run<Job: 'static+FnOnce() -> ()+Send>(&self, job: Job) {
        self.jobs.send(Box::new(job)).unwrap();
    }

    ///
    /// De-spawns this thread and returns the join handle 
    ///
    pub fn despawn(self) -> thread::JoinHandle<()> {
        self.thread
    }
}
