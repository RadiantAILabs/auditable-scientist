//! Durable campaign envelope and final reporting records.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
/// Immutable run identity written when a campaign opens.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    /// Shared run identifier.
    pub run_id: String,
    /// Initial entry mode.
    pub mode: String,
    /// UTC creation time.
    pub started: String,
    /// Crate version used to create the run.
    pub binary_version: String,
    /// Original task source, reused only as identity on resume.
    pub task_path: PathBuf,
    /// Digest of the source evaluator when present.
    pub evaluator_sha256: Option<String>,
}
/// Final campaign result written to outcome.json.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Outcome {
    /// completed, completed_no_winner, paper_incomplete, failed or interrupted.
    pub outcome: String,
    /// Original phase names mapped to lifecycle statuses.
    pub phases: BTreeMap<String, String>,
    /// Exact settled dollars.
    pub usd_settled: String,
    /// Exact ceilings held by unfinished calls.
    pub usd_reserved_unspent: String,
    /// Elapsed wall-clock seconds across resumes.
    pub wall_seconds: f64,
    /// Recorded cap observations.
    pub caps_hit: Vec<serde_json::Value>,
    /// Explicitly skipped steps and reasons.
    pub skipped_steps: Vec<serde_json::Value>,
    /// Actual model identities and invocation counts.
    pub models_called: BTreeMap<String, u64>,
    /// Reasons for optional evidence degradation.
    pub degraded_reasons: Vec<String>,
}
/// Recorded phase lifecycle state used by graph checks and final reporting.
pub use polaris_cosmos::flow::PhaseControl as PhaseBook;
