//! `SIGTERM` and `SIGINT` stop a campaign cooperatively; a second signal ends
//! the process at once.
//!
//! The signals are raised against the test process itself. This is the only
//! test in its binary, so no other test is disturbed by them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use polaris_cosmos::flow::StopFlag;
use polaris_scientist_run::signals;

fn raise(signal: libc::c_int) {
    // SAFETY: `raise` sends a signal to the calling thread; a handler for it is
    // installed before this is called.
    let status = unsafe { libc::raise(signal) };
    assert_eq!(status, 0, "the signal was raised");
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gave up waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_signal_raises_the_stop_flag_and_the_second_ends_the_process() {
    let stop = StopFlag::default();
    let second = Arc::new(AtomicUsize::new(0));
    let watcher = signals::install(stop.clone(), {
        let second = second.clone();
        move || {
            second.fetch_add(1, Ordering::SeqCst);
        }
    })
    .expect("the handlers are installed");

    assert!(!stop.is_requested(), "nothing was asked yet");

    // The first signal only asks for a stop.
    raise(libc::SIGTERM);
    until("the stop flag", || stop.is_requested()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(second.load(Ordering::SeqCst), 0, "one signal does not exit");
    assert!(!watcher.is_finished(), "the watcher waits for a second");

    // The second, of either kind, takes the last resort once.
    raise(libc::SIGINT);
    until("the exit action", || second.load(Ordering::SeqCst) == 1).await;
    until("the watcher to end", || watcher.is_finished()).await;
    assert_eq!(second.load(Ordering::SeqCst), 1);

    // The other order works the same: SIGINT first, then SIGTERM.
    let stop = StopFlag::default();
    let again = Arc::new(AtomicUsize::new(0));
    let watcher = signals::install(stop.clone(), {
        let again = again.clone();
        move || {
            again.fetch_add(1, Ordering::SeqCst);
        }
    })
    .expect("the handlers are installed again");
    raise(libc::SIGINT);
    until("the stop flag after SIGINT", || stop.is_requested()).await;
    assert_eq!(again.load(Ordering::SeqCst), 0);
    raise(libc::SIGTERM);
    until("the exit action after SIGTERM", || {
        again.load(Ordering::SeqCst) == 1
    })
    .await;
    until("the second watcher to end", || watcher.is_finished()).await;
    assert_eq!(
        second.load(Ordering::SeqCst),
        1,
        "the first watcher is gone and was not called again"
    );
}
