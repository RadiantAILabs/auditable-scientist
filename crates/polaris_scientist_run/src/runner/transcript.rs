//! The record of one call: every message the session sent, one JSON object per
//! line.

use std::path::Path;

use polaris::plugins::IOMessage;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The file a call's transcript is written to, inside the call's folder.
pub const TRANSCRIPT_FILE: &str = "transcript.jsonl";

/// Create (or empty) the transcript at `path`.
pub(super) async fn create(path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::File::create(path).await
}

/// Write every message that arrives on `output` to `file`, until the channel
/// closes.
///
/// A write that fails is reported once and the rest of the messages are
/// dropped, but the channel keeps being read: the session sends into a bounded
/// channel, and one that nobody reads would stall it.
pub(super) fn spawn(
    mut file: tokio::fs::File,
    mut output: mpsc::Receiver<IOMessage>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut failed = false;
        while let Some(message) = output.recv().await {
            if failed {
                continue;
            }
            let mut line = match serde_json::to_vec(&message) {
                Ok(line) => line,
                Err(error) => {
                    tracing::warn!("a transcript message could not be encoded: {error}");
                    continue;
                }
            };
            line.push(b'\n');
            if let Err(error) = file.write_all(&line).await {
                tracing::warn!("the transcript can no longer be written: {error}");
                failed = true;
            }
        }
        if let Err(error) = file.flush().await {
            tracing::warn!("the transcript could not be flushed: {error}");
        }
    })
}
