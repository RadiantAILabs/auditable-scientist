//! The review phase.
//!
//! The external reviewer is not a node of the campaign graph: when
//! `review.mode` is `off` the phase only records that the review was skipped,
//! and when it is `adapter` the review runs after the campaign on the finished
//! `paper.pdf` and the phase only records that it is deferred. Either way the
//! phase writes `paper/ai_review_status.json`.
//!
//! After that the phase looks for a review to act on: `paper/ai_review.json`,
//! which the outside reviewer leaves. With no review the revision step does
//! nothing. With one it reports `not_built` in `paper/revision_status.json`,
//! because the review-driven edit of the paper is not built yet.

use std::path::Path;

use polaris::graph::Graph;
use polaris::system::param::{Out, Res};
use polaris_cosmos::flow::{PhaseStatus, RunHandle, bind};
use serde_json::json;

use crate::phases::paper::files::write_json;
use crate::phases::{BuildError, PhaseCtx, PhaseRegistry, RegisterError};

/// Name of the status file in the paper folder.
pub const STATUS_FILE: &str = "ai_review_status.json";

/// Name of the review the outside reviewer leaves in the paper folder.
pub const EXTERNAL_REVIEW_FILE: &str = "ai_review.json";

/// Name of the revision record in the paper folder.
pub const REVISION_FILE: &str = "revision_status.json";

/// Why the review was skipped.
pub const SKIP_REASON: &str = "external reviewer disabled; scored after the campaign";

/// Why the review is deferred.
pub const DEFERRED_REASON: &str = "the external reviewer runs after the campaign";

/// What the revision step found to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revision {
    /// There is no review, so there is nothing to revise.
    NoReview,
    /// A review exists, and the review-driven edit is not built.
    NotBuilt,
}

/// The note the phase ends with.
fn note(mode: &str) -> &'static str {
    if mode == "off" {
        SKIP_REASON
    } else {
        DEFERRED_REASON
    }
}

/// Writes `ai_review_status.json` for a review mode.
///
/// # Errors
///
/// Returns the I/O error from writing the file.
pub fn write_status(paper_dir: &Path, mode: &str) -> std::io::Result<()> {
    let status = if mode == "off" { "skipped" } else { "deferred" };
    write_json(
        &paper_dir.join(STATUS_FILE),
        &json!({"status": status, "reason": note(mode)}),
    )
}

/// Looks for a review to revise the paper from. With none, nothing is written;
/// with one, the revision record says the step is not built.
///
/// # Errors
///
/// Returns the I/O error from writing the record.
pub fn revise(paper_dir: &Path) -> std::io::Result<Revision> {
    if !paper_dir.join(EXTERNAL_REVIEW_FILE).is_file() {
        return Ok(Revision::NoReview);
    }
    write_json(
        &paper_dir.join(REVISION_FILE),
        &json!({
            "status": "not_built",
            "reason": "the review-driven edit of the paper is not built",
        }),
    )?;
    Ok(Revision::NotBuilt)
}

/// Registers the phase's resource providers.
///
/// # Errors
///
/// Never fails; the signature is the one every phase has.
pub fn register(_reg: &mut PhaseRegistry) -> Result<(), RegisterError> {
    Ok(())
}

/// Adds the phase's systems and edges: `review.skip`, `rev.gate`, `rev.report`.
///
/// # Errors
///
/// Never fails; the signature is the one every phase has.
pub fn build(mut graph: Graph, ctx: &PhaseCtx) -> Result<Graph, BuildError> {
    let mode = ctx.config.string("review.mode").to_string();
    let skip_mode = mode.clone();
    graph.add_system(bind::<Res<RunHandle>, (), _>(
        "review.skip",
        move |handle, _| {
            let mode = skip_mode.clone();
            Box::pin(async move {
                write_status(&handle.root.join("paper"), &mode).map_err(|e| {
                    polaris::system::system::SystemError::ExecutionError(e.to_string())
                })
            })
        },
    ));
    graph.add_system(bind::<Res<RunHandle>, Revision, _>(
        "rev.gate",
        |handle, _| {
            Box::pin(async move {
                revise(&handle.root.join("paper")).map_err(|e| {
                    polaris::system::system::SystemError::ExecutionError(e.to_string())
                })
            })
        },
    ));
    graph.add_system(bind::<Out<Revision>, PhaseStatus, _>(
        "rev.report",
        move |_revision, _| {
            let note = note(&mode);
            Box::pin(async move { Ok(PhaseStatus::Skipped(note.to_string())) })
        },
    ));
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skip_record_says_why() {
        let dir = tempfile::tempdir().unwrap();
        write_status(dir.path(), "off").unwrap();
        let text = std::fs::read_to_string(dir.path().join(STATUS_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["status"], "skipped");
        assert_eq!(value["reason"], SKIP_REASON);
    }

    #[test]
    fn revision_does_nothing_without_a_review_and_reports_not_built_with_one() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(revise(dir.path()).unwrap(), Revision::NoReview);
        assert!(!dir.path().join(REVISION_FILE).exists());
        std::fs::write(dir.path().join(EXTERNAL_REVIEW_FILE), "{}").unwrap();
        assert_eq!(revise(dir.path()).unwrap(), Revision::NotBuilt);
        let text = std::fs::read_to_string(dir.path().join(REVISION_FILE)).unwrap();
        assert!(text.contains("not_built"));
    }
}
