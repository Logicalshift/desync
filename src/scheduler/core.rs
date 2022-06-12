use super::scheduler_thread::*;
use super::job_queue::*;
use super::queue_state::*;
use super::wake_queue::*;

use std::sync::*;
use std::collections::vec_deque::*;

use futures::task;
use futures::task::{Context};

///
/// The scheduler core contains the internal data used by the scheduler
///
pub (super) struct SchedulerCore {
    /// The queues that are active in the scheduler
    pub (super) schedule: Arc<Mutex<VecDeque<Arc<JobQueue>>>>,

    /// Active threads and whether or not they're busy
    pub (super) threads: Mutex<Vec<(Arc<Mutex<bool>>, SchedulerThread)>>,

    /// The maximum number of threads permitted in this scheduler
    pub (super) max_threads: Mutex<usize>
}

impl SchedulerCore {
    ///
    /// Wakes a thread to run a dormant queue. Returns true if a thread was woken up
    ///
    pub (super) fn schedule_thread(&self, core: Arc<SchedulerCore>) -> bool {
        // Find a dormant thread and activate it
        let schedule = self.schedule.clone();

        // Schedule work on this dormant thread
        let work_core   = Arc::clone(&core);
        let do_work     = move |work: Arc<JobQueue>| {
            let waker       = Arc::new(WakeQueue(Arc::clone(&work), Arc::clone(&work_core)));
            let waker       = task::waker_ref(&waker);
            let mut context = Context::from_waker(&waker);

            work.drain(&mut context)
        };

        if !self.schedule_dormant(move || Self::next_to_run(&schedule), do_work) {
            // Try to create a new thread
            if self.spawn_thread_if_less_than_maximum() {
                // Try harder to schedule this task if a thread was created
                self.schedule_thread(core)
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
    pub (super) fn reschedule_queue(&self, queue: &Arc<JobQueue>, core: Arc<SchedulerCore>) {
        let reschedule = {
            let mut core = queue.core.lock().expect("JobQueue core lock");

            match core.state {
                QueueState::Idle => {
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
                },

                QueueState::WaitingForPoll(_) => {
                    // If the target thread gets stuck and stops draining the queue, race it to reschedule it on one of our threads if we can
                    true
                },

                _ => {
                    // Not scheduled
                    false
                }
            }
        };

        if reschedule {
            self.schedule.lock().expect("Schedule lock").push_back(queue.clone());
            self.schedule_thread(core);
        }
    }

    ///
    /// Finds the next queue that should be run. If this returns successfully, the queue will 
    /// be marked as running.
    /// 
    pub (super) fn next_to_run(schedule: &Arc<Mutex<VecDeque<Arc<JobQueue>>>>) -> Option<Arc<JobQueue>> {
        // Search the queues...
        let mut schedule = schedule.lock().expect("Schedule lock");

        // Find a queue where the state is pending
        while let Some(q) = schedule.pop_front() {
            let mut core = q.core.lock().expect("JobQueue core lock");

            match core.state {
                QueueState::Pending |
                QueueState::WaitingForPoll(_) => {
                    // Queue is ready to run. Mark it as running and return it
                    core.state = QueueState::Running;
                    return Some(q.clone());
                }

                _ => { 
                    // Move to the next queue in the schedule
                }
            }
        }

        None
    }

    ///
    /// Attempts to schedule a task on a dormant thread
    ///
    pub (super) fn schedule_dormant<NextJob, RunJob, JobData>(&self, next_job: NextJob, job: RunJob) -> bool
    where
        RunJob:     'static + Send + Fn(JobData) -> (), 
        NextJob:    'static + Send + Fn() -> Option<JobData>,
    {
        let threads = self.threads.lock().expect("Scheduler threads lock");

        // Find the first thread that is not marked as busy and schedule this task on it
        for &(ref busy_rc, ref thread) in threads.iter() {
            if let Ok(mut busy) = busy_rc.try_lock() {
                // If the busy lock is held, then we consider the thread to be busy
                if !*busy {
                    // Clone the busy mutex so we can return this thread to readiness
                    let also_busy =  busy_rc.clone();

                    // This thread is busy
                    *busy = true;
                    thread.run(move || {
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
                    });

                    return true;
                }
            }
        }

        // No dormant threads were found
        false
    }

    ///
    /// If we're running fewer than the maximum number of threads, try to spawn a new one
    ///
    pub (super) fn spawn_thread_if_less_than_maximum(&self) -> bool {
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
}
