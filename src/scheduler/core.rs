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
    /// If a queue is pending and currently in the schedule (ie, has not yet been claimed by a thread), then move its state to 'Running'
    /// and remove it from the schedule, claiming it for the current thread.
    ///
    /// Returns 'true' if the queue was claimed, false otherwise
    ///
    pub (super) fn claim_pending_queue(&self, queue: &Arc<JobQueue>) -> bool {
        // Lock the schedule first
        let mut schedule    = self.schedule.lock().expect("Schedule lock");

        // Now claim the queue
        let mut queue_core  = queue.core.lock().expect("Queue lock");

        // The queue must be idle or pending to be claimable
        match queue_core.state {
            QueueState::Pending |
            QueueState::Idle    => {
                // Move the queue to the running state
                queue_core.state = QueueState::Running;

                // Remove from the schedule
                schedule.retain(|scheduled_queue| !Arc::ptr_eq(scheduled_queue, queue));

                true
            }

            _ => {
                // Any other state does not cause the queue to start running
                false
            }
        }
    }

    ///
    /// If a queue is idle and has pending jobs, places it in the schedule
    ///
    pub (super) fn reschedule_queue(&self, queue: &Arc<JobQueue>, core: Arc<SchedulerCore>) {
        let reschedule = {
            let mut core = queue.core.lock().expect("JobQueue core lock");

            // Signal any waiting condition variables
            core.wake_blocked.iter_mut()
                .for_each(|cond_var| {
                    if let Some(cond_var) = cond_var.upgrade() {
                        cond_var.notify_one();
                    }
                });
            core.wake_blocked.retain(|cond_var| cond_var.strong_count() > 0);

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
    /// If any of the scheduler threads have finished (which generally means they panicked), despawn them
    ///
    pub (super) fn remove_finished_threads(&self) {
        let mut dead_threads = vec![];

        // Collate the dead threads into a vec
        {
            // Find busy threads
            let mut threads = self.threads.lock().expect("Scheduler threads lock");

            // TODO: drain_filter in nightly would be better than this (but it's in nightly)
            let mut thread_num = 0;
            while thread_num < threads.len() {
                let (_, thread) = &threads[thread_num];

                if thread.is_finished() {
                    // Remove the thread and queue it for later despawning
                    let (is_busy, dead_thread) = threads.remove(thread_num);
                    dead_threads.push((is_busy, dead_thread));
                } else {
                    // Thread still running
                    thread_num += 1;
                }
            }
        }

        // Despawn the dead threads (which might panic a bit)
        for (is_busy, dead_thread) in dead_threads {
            // Join with the thread in case it's mid-panic
            let maybe_panic = dead_thread.despawn().join();

            if let Ok(()) = maybe_panic {
                // The busy flag should always be unset after a thread is despawned
                let busy = is_busy.lock().unwrap();
                if *busy {
                    panic!("Thread despawned while busy");
                }
            } else {
                // Panics are relayed via the desync that failed
            }
        }
    }

    ///
    /// Attempts to schedule a task on a dormant thread
    ///
    pub (super) fn schedule_dormant<NextJob, RunJob, JobData>(&self, next_job: NextJob, job: RunJob) -> bool
    where
        RunJob:     'static + Send + Fn(JobData) -> (), 
        NextJob:    'static + Send + Fn() -> Option<JobData>,
    {
        // Try to despawn any threads that have finished since the last time we were called
        self.remove_finished_threads();

        // Find busy threads
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
