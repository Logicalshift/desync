extern crate desync;
extern crate futures;

use desync::Desync;

mod scheduler;
use self::scheduler::timeout::*;

use std::time::*;
use std::thread::*;

#[derive(Debug)]
struct TestData {
    val: u32
}

#[test]
fn can_retrieve_data_synchronously() {
    let desynced = Desync::new(TestData { val: 0 });

    assert!(desynced.sync(|data| data.val) == 0);
}

#[test]
fn can_update_data_asynchronously() {
    let desynced = Desync::new(TestData { val: 0 });

    desynced.async(|data| {
        sleep(Duration::from_millis(100));
        data.val = 42;
    });
    
    assert!(desynced.sync(|data| data.val) == 42);
}

#[test]
fn can_update_data_asynchronously_1000_times() {
    for _i in 0..1000 {
        timeout(|| {
            let desynced = Desync::new(TestData { val: 0 });

            desynced.async(|data| {
                data.val = 42;
            });
            desynced.async(|data| {
                data.val = 43;
            });
            
            assert!(desynced.sync(|data| data.val) == 43);
        }, 500);
    }
}

#[test]
fn can_update_data_with_future() {
    timeout(|| {
        use futures::executor;

        let desynced = Desync::new(TestData { val: 0 });

        desynced.async(|data| {
            sleep(Duration::from_millis(100));
            data.val = 42;
        });

        let mut future = executor::spawn(desynced.future(|data| {
            data.val
        }));
        
        assert!(future.wait_future().unwrap() == 42);
    }, 500);
}

#[test]
fn can_update_data_with_future_1000_times() {
    // Seems to timeout fairly reliably after signalling the future
    use futures::executor;

    for _i in 0..1000 {
        timeout(|| {
            let desynced = Desync::new(TestData { val: 0 });

            desynced.async(|data| {
                data.val = 42;
            });
            desynced.async(|data| {
                data.val = 43;
            });

            let mut future = executor::spawn(desynced.future(|data| data.val));
            
            assert!(future.wait_future().unwrap() == 43);
        }, 500);
    }
}

#[test]
fn dropping_while_running_isnt_obviously_bad() {
    let desynced = Desync::new(TestData { val: 0 });

    desynced.async(|data| {
        sleep(Duration::from_millis(100));
        data.val = 42;
    });
    desynced.async(|data| {
        sleep(Duration::from_millis(100));
        data.val = 42;
    });
}
