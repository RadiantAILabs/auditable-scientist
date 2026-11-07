//! Correctness-gated strict improvement, independent of rubric success.

use crate::eval::{EvalError, entrypoint};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Selected complete program evaluation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Incumbent {
    /// Correct finite numeric score.
    pub combined_score: f64,
    /// Always true for a selected program.
    pub correct: bool,
    /// Entrypoint path relative to the program directory.
    pub entrypoint_path: PathBuf,
    /// Absolute program directory for downstream readers.
    pub program_dir: PathBuf,
    /// Attempt that produced the candidate; seed uses `baseline`.
    pub source_attempt_id: String,
    /// UTC timestamp of selection.
    pub updated_at: String,
}
/// Read only complete, correct, finite, numeric evaluations with a valid entrypoint.
pub fn candidate(
    program: &Path,
    results: &Path,
    attempt: &str,
) -> Result<Option<Incumbent>, EvalError> {
    let correct: Value = serde_json::from_slice(&fs::read(results.join("correct.json"))?)?;
    let metrics: Value = serde_json::from_slice(&fs::read(results.join("metrics.json"))?)?;
    if correct.get("correct").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let Some(score) = metrics
        .get("combined_score")
        .and_then(Value::as_f64)
        .filter(|s| s.is_finite())
    else {
        return Ok(None);
    };
    let program_dir = program.canonicalize()?;
    let entrypoint_path = entrypoint(program)?
        .strip_prefix(&program_dir)
        .map_err(|e| EvalError(e.to_string()))?
        .to_owned();
    Ok(Some(Incumbent {
        combined_score: score,
        correct: true,
        entrypoint_path,
        program_dir,
        source_attempt_id: attempt.into(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    }))
}
/// Promote with strict `>`; ties keep the earlier incumbent.
pub fn promote(incumbent: &mut Option<Incumbent>, candidate: Incumbent) -> bool {
    if !candidate.correct || !candidate.combined_score.is_finite() {
        return false;
    }
    if incumbent
        .as_ref()
        .is_none_or(|old| candidate.combined_score > old.combined_score)
    {
        *incumbent = Some(candidate);
        true
    } else {
        false
    }
}
/// Merge lane winners in ascending lane order, preserving the lowest lane on ties.
pub fn join(mut lanes: Vec<(usize, Incumbent)>) -> Option<Incumbent> {
    lanes.sort_by_key(|(index, _)| *index);
    let mut best = None;
    for (_, candidate) in lanes {
        promote(&mut best, candidate);
    }
    best
}
