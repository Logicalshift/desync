use desync::scheduler::*;

#[test]
fn will_despawn_extra_threads() {
    // As we join with the threads, we'll timeout if any of the spawned threads fail to end
    let scheduler = scheduler();

    // Maximum of 10 threads, but we'll spawn 20
    scheduler.set_max_threads(10);
    for _ in 1..20 {
        scheduler.spawn_thread();
    }

    scheduler.despawn_threads_if_overloaded();
}
