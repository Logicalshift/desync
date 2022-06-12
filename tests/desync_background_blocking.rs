extern crate desync;
extern crate futures;

use desync::Desync;

mod timeout;
use self::timeout::*;

use std::sync::*;

#[test]
fn fill_thread_pool_with_wait_in_background() {
    timeout(|| {
        use std::thread;
        use std::sync::mpsc;
        use std::time::{Duration};

        // This is potentially an issue with how we do thread stealing: the idea here is to create
        // 'WaitInBackground' status syncs across the whole thread pool, so when a 'foreground' sync
        // finishes there are no threads available to carry it on.
        //
        // A 'green' threads library would be ideal here as we could re-use the blocked threads: this
        // test mostly looks at the case where this isn't available. 
        let (waiting_send, waiting_recv)    = mpsc::channel();
        let (release_send, release_recv)    = mpsc::channel();
        let wait_on                         = Arc::new(Desync::new(()));

        // In a background thread, block 'wait_on' until something is sent to the 'release' channel
        let background_wait                 = Arc::clone(&wait_on);
        thread::spawn(move || {
            background_wait.sync(move |_| {
                // Tell the main thread that we've reached this point (desync will steal the thread we created, which is how we exhaust the thread pool later on)
                waiting_send.send(()).unwrap();

                println!("Blocking Desync waiting...");

                // Wait for the release message
                release_recv.recv().unwrap();

                println!("Blocking Desync finished (unblocking)");
            });
        });

        // Wait for the spawned thread to signal that it has started running
        waiting_recv.recv().unwrap();

        // Create a bunch of desyncs and tell them to wait on the same sync (this will eventually fill the thread pool with threads that are waiting for a background desync)
        let background_desyncs = (0..100).into_iter().map(|_| Arc::new(Desync::new(()))).collect::<Vec<_>>();

        background_desyncs.iter()
            .for_each(|background| {
                let background_wait = Arc::clone(&wait_on);

                background.desync(move |_| {
                    // Doesn't really matter what the task is...
                    println!("Waiting for blocking Desync...");
                    background_wait.sync(|_| { });
                    println!("Finished background task");
                })
            });

        // Sleep to allow the thread pool to saturate (tests all run in parallel so this will be most reliable when this test is run by itself)
        // (Another problem as this blocks the scheduler when it fails is it could cause other tests to time out, so definitely run this by itself
        // to check if there are issues)
        thread::sleep(Duration::from_millis(200));

        // Send the release so our background thread finishes, and now the background desyncs can run
        println!("Releasing blocking desync...");
        release_send.send(()).unwrap();

        // Should be able to sync with all the background tasks
        println!("Synchronising with background tasks...");
        background_desyncs.into_iter()
            .for_each(|background| background.sync(|_| { }));

        // Should also be able to sync with the 'wait_on' desync (after all the background things)
        println!("Synchronising with blocking desync to finish");
        wait_on.sync(|_| { });
    }, 20000);
}
