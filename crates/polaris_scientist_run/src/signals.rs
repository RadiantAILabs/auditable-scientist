//! Turn `SIGTERM` and `SIGINT` into a cooperative stop.
//!
//! The first signal raises the campaign's [`StopFlag`]: calls in flight are
//! told to stop, the campaign records where it was, and the command exits with
//! the interrupted status. A second signal means the stop is taking too long,
//! so the process ends at once through the action it was given.

use std::io;

use polaris_cosmos::flow::StopFlag;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinHandle;

/// Watch for `SIGTERM` and `SIGINT`.
///
/// The first one calls [`StopFlag::request`] on `stop`; the second calls
/// `on_second` and the watcher ends. The handlers are in place when this
/// returns. The watcher runs until the second signal or until the returned
/// handle is aborted.
///
/// Must be called inside a Tokio runtime.
///
/// # Errors
///
/// The error from registering a signal handler.
pub fn install(
    stop: StopFlag,
    on_second: impl Fn() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(tokio::spawn(async move {
        let mut seen = 0_u32;
        loop {
            let name = tokio::select! {
                _ = terminate.recv() => "SIGTERM",
                _ = interrupt.recv() => "SIGINT",
            };
            seen += 1;
            if seen == 1 {
                tracing::warn!("{name} received: stopping; send it again to exit at once");
                stop.request();
            } else {
                tracing::warn!("{name} received again: exiting now");
                on_second();
                return;
            }
        }
    }))
}
