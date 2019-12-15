
use super::job::*;
use super::active_queue::*;
use super::queue_state::*;
use super::wake_thread::*;

use std::fmt;
use std::sync::*;
use std::thread;
use std::collections::vec_deque::*;

use futures::task;
use futures::task::{Context, Poll};

///
/// A job queue provides a list of jobs to perform in order
/// 
pub struct JobQueue {
    /// The shared data for this queue is stored within a mutex
    pub (super) core: Mutex<JobQueueCore>
}

///
/// The result of running a job
///
pub (super) enum JobStatus {
    /// No jobs were waiting on the queue
    NoJobsWaiting,

    /// Job was run successfully
    Finished,

    /// The job caused the queue to suspend: it will be rescheduled in the background
    WaitInBackground
}

///
/// Structure protected by the jobqueue matrix
///
pub (super) struct JobQueueCore {
    /// The jobs that are scheduled on this queue
    pub (super) queue: VecDeque<Box<dyn ScheduledJob>>,

    /// The current state of this queue
    pub (super) state: QueueState,
    
    /// How many times this queue has been suspended (can be negative to indicate the suspension ended before it began)
    pub (super) suspension_count: i32
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
    pub (super) fn new() -> JobQueue {
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
    pub (super) fn dequeue(&self) -> Option<Box<dyn ScheduledJob>> {
        let mut core = self.core.lock().expect("JobQueue core lock");

        match core.state {
            QueueState::Suspending      => None,
            QueueState::WaitingForWake  => None,
            other                       => {
                debug_assert!(other == QueueState::Running);
                core.queue.pop_front()
            }
        }
    }

    ///
    /// Adds a job to the front of the queue (so it's the next one to run)
    ///
    pub (super) fn requeue(&self, job: Box<dyn ScheduledJob>) {
        let mut core = self.core.lock().expect("JobQueue core lock");

        core.queue.push_front(job);
    }

    ///
    /// Runs jobs on this queue until there are none left, marking the job as inactive when done
    /// 
    pub (super) fn drain(&self, context: &mut Context) {
        let _active = ActiveQueue { queue: self };

        debug_assert!(self.core.lock().unwrap().state == QueueState::Running);
        let mut done = false;

        while !done {
            // Run jobs until the queue is drained or blocks
            while let Some(mut job) = self.dequeue() {
                debug_assert!(self.core.lock().unwrap().state == QueueState::Running);

                let poll_result = job.run(context);

                match poll_result {
                    Poll::Ready(()) => { },
                    Poll::Pending   => { 
                        // Job needs requeing
                        self.requeue(job);

                        // Queue should move from the 'running' state to the 'waiting for wake' state
                        let mut core = self.core.lock().expect("JobQueue core lock");

                        core.state = match core.state {
                            QueueState::Running             => QueueState::WaitingForWake,
                            QueueState::AwokenWhileRunning  => QueueState::Running,
                            other                           => other
                        };

                        if core.state == QueueState::WaitingForWake {
                            return;
                        }
                    }
                }
            }

            // Try to move back to the 'not running' state
            {
                let mut core = self.core.lock().expect("JobQueue core lock");
                debug_assert!(core.state == QueueState::Running || core.state == QueueState::Suspending);

                // If the queue is empty at the point where we obtain the lock, we can deactivate ourselves
                if core.queue.len() == 0 {
                    core.state = match core.state {
                        QueueState::Running         => QueueState::Idle,
                        QueueState::Suspending      => QueueState::Suspended,
                        x                           => x
                    };
                    done = true;
                } else if core.state == QueueState::Suspending {
                    // Stop draining as we're suspending
                    core.state = QueueState::Suspended;
                    done = true;
                } else if core.state == QueueState::Pending {
                    // Will restart when we get re-scheduled
                    done = true;
                }
            }
        }
    }

    ///
    /// With the queue already in the running state, dequeues a single job and runs it synchronously on the current thread
    ///
    pub (super) fn run_one_job_now(queue: &Arc<JobQueue>) -> JobStatus {
        if let Some(mut job) = queue.dequeue() {
            // Queue is running
            debug_assert!(queue.core.lock().unwrap().state == QueueState::Running);

            let waker       = Arc::new(WakeThread(Arc::clone(queue), thread::current()));
            let waker       = task::waker_ref(&waker);
            let mut context = Context::from_waker(&waker);

            loop {
                let poll_result = job.run(&mut context);

                match poll_result {
                    // A ready result ends the loop
                    Poll::Ready(()) => break,
                    Poll::Pending   => {
                        // Try to move to the parking state
                        let should_park = {
                            let mut core = queue.core.lock().unwrap();

                            core.state = match core.state {
                                QueueState::AwokenWhileRunning  => QueueState::Running,
                                QueueState::Running             => QueueState::WaitingForWake,
                                other                           => panic!("Queue was in unexpected state {:?}", other)
                            };

                            core.state == QueueState::WaitingForWake
                        };

                        // Park until the queue state returns changes
                        if should_park {
                            // If should_park is set to false, the queue was awoken very quickly
                            loop {
                                let current_state = { queue.core.lock().unwrap().state };
                                match current_state {
                                    QueueState::Idle            => break,
                                    QueueState::WaitingForWake  => (),
                                    other                       => panic!("Queue was in unexpected state {:?}", other)
                                }

                                // Park until we're awoken from the other thread (once awoken, we re-check the state)
                                thread::park();
                            }
                        }
                    }
                }
            }

            JobStatus::Finished
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
                JobStatus::WaitInBackground
            } else {
                JobStatus::NoJobsWaiting
            }
        }
    }
}
