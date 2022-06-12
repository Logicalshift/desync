use std::thread;
use std::sync::mpsc::*;

///
/// Creates a FnMut that runs a FnOnce once (or panics)
///
/// The scheduler only runs jobs once so it accepts them as FnOnce values, but it's currently not possible to
/// box FnOnce values such that they can be run, so we wrap them in FnMut values that panic if called more
/// than once.
/// 
/// (Nightly rust has FnBox to get around this)
///
fn wrap_fnonce<TFn: FnOnce() -> ()>(job: TFn) -> impl FnMut() -> () {
    let mut job = Some(job);

    move || {
        let job = job.take();

        if let Some(job) = job {
            job()
        } else {
            panic!("Cannot evaluate a job more than once")
        }
    }
}

///
/// A scheduler thread reads from the scheduler queue
///
pub struct SchedulerThread {
    /// The jobs that this thread should run
    jobs: Sender<Box<dyn FnMut() -> ()+Send>>,

    /// The thread itself
    thread: thread::JoinHandle<()>,
}

impl SchedulerThread {
    ///
    /// Creates a new scheduler thread 
    ///
    pub fn new() -> SchedulerThread {
        // All the thread does is run jobs from its channel
        let (jobs_in, jobs_out): (Sender<Box<dyn FnMut() -> ()+Send>>, Receiver<Box<dyn FnMut() -> ()+Send>>) = channel();
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
        self.jobs.send(Box::new(wrap_fnonce(job))).unwrap();
    }

    ///
    /// Returns true if this scheduler thread has finished (eg: due to a panic)
    ///
    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    ///
    /// De-spawns this thread and returns the join handle 
    ///
    pub fn despawn(self) -> thread::JoinHandle<()> {
        self.thread
    }
}
