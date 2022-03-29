use super::job_queue::*;
use super::queue_state::*;
use super::core::*;
use super::active_queue::*;
use super::wake_queue::*;
use super::desync_scheduler::*;

use futures::prelude::*;
use futures::channel::oneshot;
use futures::task;

use std::mem;
use std::sync::*;
use std::pin::{Pin};

///
/// The possible states of a future result
///
enum FutureResultState<T> {
    None,
    Some(T),
    ReturnedViaFuture
}

///
/// The possible states of a waker that will wake up a queue that's being drained as part of polling a future
///
enum DrainWakerState {
    /// The drain waker has never been woken before
    NotWoken,

    /// This has been woken but the waker is not set
    Woken,

    /// This has been woken and the waker is set
    WillWakeWithWaker(task::Waker)
}

///
/// Waker that will wake up a queue being drained. We set the waker with a delay so it's possible to switch to
/// a waker that will wake a future on a background queue if necessary.
///
struct DrainWaker {
    state: Mutex<DrainWakerState>
}

impl DrainWaker {
    ///
    /// Creates a new drain waker
    ///
    fn new() -> DrainWaker {
        DrainWaker {
            state: Mutex::new(DrainWakerState::NotWoken)
        }
    }

    ///
    /// Sets the waker to be called when this drain waker is woken
    ///
    fn wake_with(&self, new_waker: task::Waker) {
        use self::DrainWakerState::*;

        // Update the state and determine if we need to invoke the waker immediately (if it's been woken before the waker was set)
        let to_wake = {
            // Fetch the current state
            let mut new_state   = self.state.lock().expect("Drain waker state");
            let mut state       = Woken;
            mem::swap(&mut *new_state, &mut state);

            // Update the state based on this action
            match state {
                Woken                           => { *new_state = Woken; Some(new_waker) },
                NotWoken                        => { *new_state = WillWakeWithWaker(new_waker); None },
                WillWakeWithWaker(_old_waker)   => { *new_state = WillWakeWithWaker(new_waker); None }
            }
        };

        // Wake up the waker, if we need to call it immediately
        to_wake.map(|to_wake| to_wake.wake());
    }
}

impl task::ArcWake for DrainWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        use self::DrainWakerState::*;

        // If the current state contains a waker, we'll call it once we've unlocked the mutex
        let to_wake = {
            // Fetch the current state
            let mut new_state   = arc_self.state.lock().expect("Drain waker state");
            let mut state       = Woken;
            mem::swap(&mut *new_state, &mut state);

            // Update the state based on this action
            match state {
                NotWoken                    => { *new_state = Woken; None },
                Woken                       => { *new_state = Woken; None },
                WillWakeWithWaker(waker)    => { *new_state = Woken; Some(waker) }
            }
        };

        // Wake up the waker
        to_wake.map(|to_wake| to_wake.wake());
    }
}

///
/// Waker that wakes up two different wakers at once
///
struct DoubleWaker(Mutex<Option<(task::Waker, task::Waker)>>);

impl task::ArcWake for DoubleWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        let to_wake = arc_self.0.lock().unwrap().take();
        if let Some((waker1, waker2)) = to_wake {
            waker1.wake();
            waker2.wake();
        }
    }
}

///
/// Signalling structure used to return the result of a scheduler future
///
struct SchedulerFutureResult<T> {
    /// The result of the future, or None if it has not been generated yet
    result: FutureResultState<Result<T, oneshot::Canceled>>,

    /// The waker to be called when the future is available
    waker: Option<task::Waker>
}

///
/// Wrapper that cancels the future if it is dropped before it is signalled
///
pub (super) struct SchedulerFutureSignaller<T>(Arc<Mutex<SchedulerFutureResult<T>>>);

///
/// Future representing a task pending on a scheduler
/// 
/// If polled when no threads are available, this future will run synchronously on the current thread
/// (stealing its execution time rather than blocking)
///
pub struct SchedulerFuture<T: Send> {
    /// The unique ID of this future
    id: FutureId,

    /// The queue which will eventually evaluate the result of this future
    queue: Arc<JobQueue>,

    /// The scheduler core that this future belongs to
    scheduler: Scheduler,

    /// Set to true if this future has claimed ownership of the queue to drain it
    draining: bool,

    /// A container for the result of this scheduler future
    result: Arc<Mutex<SchedulerFutureResult<T>>>
}

impl<T> FutureResultState<T> {
    ///
    /// Returns true if no result has ever been generated for this future
    ///
    fn is_none(&self) -> bool {
        match self {
            FutureResultState::None                 => true,
            FutureResultState::Some(_)              => false,
            FutureResultState::ReturnedViaFuture    => false
        }
    }

    ///
    /// If a result has been generated by this future and has not previously been taken, returns it. Panics if the result has already
    /// been returned and is no longer available.
    ///
    fn take(&mut self) -> Option<T> {
        // Move the value out of this object
        let mut new_value = FutureResultState::ReturnedViaFuture;
        mem::swap(self, &mut new_value);

        // Return it if it contains a value
        match new_value {
            FutureResultState::None                 => { *self = FutureResultState::None; None },
            FutureResultState::ReturnedViaFuture    => { *self = FutureResultState::ReturnedViaFuture; panic!("Future result has already been returned") },
            FutureResultState::Some(value)          => { Some(value) }
        }
    }
}

impl<T> Drop for SchedulerFutureSignaller<T> {
    fn drop(&mut self) {
        let waker = {
            let mut future_result = self.0.lock().expect("Scheduler future result");

            // If no result has been generated, then mark the future as canceled
            if future_result.result.is_none() {
                // Mark the future as canceled
                future_result.result = FutureResultState::Some(Err(oneshot::Canceled));

                // Wake up anything that was polling it
                let waker = future_result.waker.take();
                waker
            } else {
                // Result is already set, so don't wake anything up
                None
            }
        };

        // If we need to wake the future, then do so here (note that we're outside of the lock when we do this)
        waker.map(|waker| waker.wake());
    }
}

impl<T> SchedulerFutureSignaller<T> {
    ///
    /// Signals that the result of the calculation is available
    ///
    pub (super) fn signal(self, result: T) {
        let waker = {
            let mut future_result = self.0.lock().expect("Scheduler future result");

            // Set the result
            future_result.result = FutureResultState::Some(Ok(result));

            // Retrieve the waker
            future_result.waker.take()
        };

        // If we retrieved a waker from the result, wake it up
        waker.map(|waker| waker.wake());
    }
}

///
/// Possible actions we can take based on the state of the signal for the future
///
enum SchedulerAction<T> {
    /// Queue is running elsewhere: we should wait for it to reach the point where the future's result is available
    WaitForCompletion,

    /// Future has already completed: we should return the value to sender
    ReturnValue(T),

    /// We've claimed the 'running' state of the queue and should drain it
    DrainQueue,

    /// The queue has panicked
    Panic
}

impl<T: Send> SchedulerFuture<T> {
    ///
    /// Creates a new scheduler future and the result needed to signal it
    ///
    pub (super) fn new(queue: &Arc<JobQueue>, core: Arc<SchedulerCore>) -> (SchedulerFuture<T>, SchedulerFutureSignaller<T>) {
        // Create an unfinished result
        let result = SchedulerFutureResult {
            result: FutureResultState::None,
            waker:  None
        };
        let result = Arc::new(Mutex::new(result));

        // Insert into a future
        let future = SchedulerFuture {
            id:         FutureId::new(),
            queue:      Arc::clone(queue),
            scheduler:  Scheduler { core },
            draining:   false,
            result:     Arc::clone(&result)
        };

        (future, SchedulerFutureSignaller(result))
    }

    ///
    /// Detaches from this future, leaving it to run in the background on the main desync scheduler
    ///
    #[inline]
    pub fn detach(self) {
        // Nothing to do, this just drops the future
    }

    ///
    /// Synchronously waits for this future to be completed by the scheduler, then returns the result
    ///
    pub fn sync(self) -> Result<T, oneshot::Canceled> {
        // See if the result has arrived yet
        let result = self.result.lock().expect("Scheduler future result").result.take();
        if let Some(result) = result {
            return result;
        }

        // Synchronise reading the result with the queue
        // TODO: if future tasks have been queued, this will wait for those as well
        let result = self.scheduler.sync(&self.queue, || { self.result.lock().expect("Scheduler future result").result.take() });

        // The result should now be available
        if let Some(result) = result {
            return result;
        } else {
            // The future never completed (or the result has been stolen somewhere else somehow)
            debug_assert!(false, "Future never completed");
            return Err(oneshot::Canceled);
        }
    }

    ///
    /// We moved the queue into the running state and need to drain it until we've got a result
    ///
    fn drain_queue(&mut self, context: &mut task::Context) -> task::Poll<Result<T, oneshot::Canceled>> {
        debug_assert!(self.queue.core.lock().expect("JobQueue core lock").state.is_running());

        // Set the queue as active
        let _active     = ActiveQueue { queue: &*self.queue };
        let mut result;

        self.draining = true;

        // While there is no result, run a job from the queue
        loop {
            // See if the result has arrived yet
            result = self.result.lock().expect("Scheduler future result").result.take();
            if !result.is_none() { break; }

            // Run the next job in the queue
            if let Some(mut job) = self.queue.dequeue() {
                // Queue is running
                debug_assert!(self.queue.core.lock().expect("Job queue core").state.is_running());

                // Create a context to poll in (we may need to reschedule in the background)
                let waker               = Arc::new(DrainWaker::new());
                let waker_ref           = task::waker_ref(&waker);
                let mut drain_context   = task::Context::from_waker(&waker_ref);

                // Poll the queue
                let poll_result = job.run(&mut drain_context);

                match poll_result {
                    task::Poll::Ready(())   => {
                        // Keep running jobs and checking the results if ready
                    },

                    task::Poll::Pending     => {
                        // Requeue the job
                        self.queue.requeue(job);

                        // If the result was supplied, break out of the loop and reschedule the queue
                        result = self.result.lock().expect("Scheduler future result").result.take();
                        if result.is_some() {
                            // Wake the queue in the background if needed (the result has arrived)
                            self.queue.core.lock().expect("JobQueue core lock").state = QueueState::WaitingForWake;

                            let queue_waker = WakeQueue(Arc::clone(&self.queue), Arc::clone(&self.scheduler.core));
                            let queue_waker = Arc::new(queue_waker);
                            let queue_waker = task::waker(queue_waker);

                            waker.wake_with(queue_waker);

                            // The future is ready (job will be rescheduled in the background)
                            self.draining = false;
                            return task::Poll::Ready(result.unwrap());
                        } else {
                            // Wait for the next poll
                            self.queue.core.lock().expect("JobQueue core lock").state = QueueState::WaitingForPoll(self.id);

                            // Wake both the queue and the context
                            let context_waker   = context.waker().clone();
                            let queue_waker     = WakeQueue(Arc::clone(&self.queue), Arc::clone(&self.scheduler.core));
                            let queue_waker     = Arc::new(queue_waker);
                            let queue_waker     = task::waker(queue_waker);

                            let wake_both       = DoubleWaker(Mutex::new(Some((queue_waker, context_waker))));
                            let wake_both       = task::waker(Arc::new(wake_both));

                            waker.wake_with(wake_both);

                            // Result is pending
                            self.draining = true;
                            return task::Poll::Pending;
                        }
                    }
                }
            } else {
                // Queue is empty and our result hasn't arrived yet?!
                
                // Assume the future will resolve eventually: move the queue in to the background
                self.result.lock().expect("Scheduler future result").waker = Some(context.waker().clone());
                
                // Reschedule the queue
                self.queue.core.lock().expect("JobQueue core lock").state = QueueState::Idle;
                self.scheduler.core.reschedule_queue(&self.queue, Arc::clone(&self.scheduler.core));

                self.draining = false;
                return task::Poll::Pending;
            }
        }

        // Reschedule the queue if there are any events left pending
        // Note: the queue is already pending when we start running events from it here.
        // This means it'll get dequeued by a thread eventually: maybe while it's running
        // here. As we've set the queue state to running while we're busy, the thread won't
        // start the queue while it's already running.
        self.queue.core.lock().expect("JobQueue core lock").state = QueueState::Idle;
        self.scheduler.core.reschedule_queue(&self.queue, Arc::clone(&self.scheduler.core));

        // Result must be available by this point
        self.draining = false;
        task::Poll::Ready(result.unwrap())
    }
}

impl<T: Send> Drop for SchedulerFuture<T> {
    fn drop(&mut self) {
        /* -- no need to reschedule manually any more, the queue will wake itself up
        // Reschedule the queue in the background if we're draining the queue
        if self.draining {
            {
                // The core should be in the 'waiting for poll' state
                let mut core = self.queue.core.lock().expect("JobQueue core lock");
                debug_assert!(match core.state { QueueState::WaitingForPoll(_) => true, _ => false });

                // Core is now idle
                core.state = QueueState::Idle;
            }

            // Reschedule the queue
            self.scheduler.core.reschedule_queue(&self.queue, Arc::clone(&self.scheduler.core));
        }
        */
    }
}

impl<T: Send> Future for SchedulerFuture<T> {
    type Output = Result<T, oneshot::Canceled>;

    ///
    /// Polls this future
    ///
    fn poll(mut self: Pin<&mut Self>, context: &mut task::Context) -> task::Poll<Self::Output> {
        // Lock the result and determine which action to take
        let next_action = {
            let mut future_result = self.result.lock().expect("Scheduler future result");

            if let Some(result) = future_result.result.take() {
                // The result is available: we should return it immediately
                SchedulerAction::ReturnValue(result)
            } else {
                // If the queue is idle when this is called, we need to schedule this task on this thread rather than one owned by the background process
                let run_action = {
                    let mut core = self.queue.core.lock().expect("JobQueue core lock");

                    match core.state {
                        QueueState::Running                     => SchedulerAction::WaitForCompletion,
                        QueueState::WaitingForWake              => SchedulerAction::WaitForCompletion,
                        QueueState::WaitingForUnpark            => SchedulerAction::WaitForCompletion,
                        QueueState::AwokenWhileRunning          => SchedulerAction::WaitForCompletion,
                        QueueState::Panicked                    => SchedulerAction::Panic,
                        QueueState::Pending                     => { core.state = QueueState::Running; SchedulerAction::DrainQueue },
                        QueueState::Idle                        => { core.state = QueueState::Running; SchedulerAction::DrainQueue }

                        QueueState::WaitingForPoll(owner_id)    => { 
                            if owner_id == self.id {
                                // Continue polling on this future
                                core.state = QueueState::Running; SchedulerAction::DrainQueue
                            } else {
                                // Wait for the owning future to complete
                                SchedulerAction::WaitForCompletion
                            }
                        },
                    }
                };

                if let SchedulerAction::WaitForCompletion = run_action {
                    // Wake us up when the future is available
                    future_result.waker = Some(context.waker().clone());
                }

                run_action
            }
        };

        match next_action {
            SchedulerAction::WaitForCompletion  => task::Poll::Pending,
            SchedulerAction::ReturnValue(value) => task::Poll::Ready(value),
            SchedulerAction::DrainQueue         => self.drain_queue(context),
            SchedulerAction::Panic              => panic!("Cannot schedule jobs on a panicked queue"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn returns_immediately_if_signaled() {
        use futures::executor;

        let scheduler           = scheduler();
        let queue               = queue();
        let (future, signaller) = SchedulerFuture::new(&queue, Arc::clone(&scheduler.core));

        signaller.signal(42);

        assert!(executor::block_on(future) == Ok(42));
    }

    #[test]
    fn cancels_when_dropped() {
        use futures::executor;

        let scheduler           = scheduler();
        let queue               = queue();
        let future              = { SchedulerFuture::<i32>::new(&queue, Arc::clone(&scheduler.core)).0 };

        assert!(executor::block_on(future) == Err(oneshot::Canceled));
    }

    #[test]
    fn signals_from_another_thread() {
        use futures::executor;
        use std::thread;
        use std::time::Duration;

        let scheduler           = scheduler();
        let queue               = queue();
        let (future, signaller) = SchedulerFuture::new(&queue, Arc::clone(&scheduler.core));

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));

            signaller.signal(42);
        });

        assert!(executor::block_on(future) == Ok(42));
    }

    #[test]
    fn forces_queue_drain() {
        use futures::executor;
        use std::thread;
        use std::time::Duration;

        let scheduler   = scheduler();
        let queue       = queue();

        scheduler.set_max_threads(0);
        scheduler.despawn_threads_if_overloaded();

        let (future, signaller) = SchedulerFuture::new(&queue, Arc::clone(&scheduler.core));

        scheduler.desync(&queue, move || {
            thread::sleep(Duration::from_millis(100));

            signaller.signal(42);
        });

        assert!(executor::block_on(future) == Ok(42));
    }
}
