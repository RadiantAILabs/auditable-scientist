//! The paper manifest, written when the phase ends, and the per-round archive.
//!
//! `paper_manifest.json` says whether the paper is complete, how many repairs
//! it took, which audit state it ended in, and which caps the phase reached.
//! An audit that could not be read is recorded as `audit_degraded`; the
//! manifest never calls such a paper audited.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::claims::AuditState;

/// File name of the manifest.
pub const MANIFEST_FILE: &str = "paper_manifest.json";

/// Folder holding one copy of the round files per repair round.
pub const ROUNDS_DIR: &str = "rounds";

/// Files copied into a round folder when they exist.
const ROUND_FILES: [&str; 7] = [
    "paper.tex",
    "claims_grounding.json",
    "validation_report.json",
    "paper_evidence_audit.json",
    "paper_evidence_audit.md",
    "repair_feedback.md",
    "review_feedback.md",
];

/// How the phase ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The paper passed its checks.
    Complete,
    /// The phase stopped before the paper passed.
    Incomplete,
}

/// A file the manifest points at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Path relative to the paper folder.
    pub path: String,
    /// SHA-256 of the contents.
    pub sha256: String,
    /// Size in bytes.
    pub bytes: u64,
}

/// The manifest of one paper phase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// How the phase ended.
    pub status: Status,
    /// Why the phase stopped early; `None` for a complete paper.
    pub reason: Option<String>,
    /// Venue template the paper was built with.
    pub venue: String,
    /// Repairs paid for from the shared counter.
    pub repair_rounds_used: u32,
    /// Most repairs the counter allows.
    pub repair_round_limit: u32,
    /// What the evidence audit concluded; `missing` when no audit finished.
    pub audit_state: AuditState,
    /// Why the audit is degraded, when it is.
    pub audit_note: Option<String>,
    /// Names of the caps the phase reached.
    pub caps_hit: Vec<String>,
    /// Planned review rounds that ran.
    pub review_rounds_done: u32,
    /// True when the last paper check had no hard error.
    pub validation_ok: bool,
    /// Hard errors of the last paper check.
    pub hard_errors: Vec<String>,
    /// Printed numbers the last check found no recorded value or derivation for.
    pub unbacked_numbers: usize,
    /// The compiled paper, when there is one.
    pub pdf: Option<FileRecord>,
    /// The TeX source.
    pub tex: Option<FileRecord>,
}

/// What the phase learned, before the files are looked at.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// How the phase ended.
    pub status: Status,
    /// Why it stopped early.
    pub reason: Option<String>,
    /// Venue name.
    pub venue: String,
    /// Repairs used.
    pub repair_rounds_used: u32,
    /// Repair cap.
    pub repair_round_limit: u32,
    /// Audit state, or `None` when no audit finished.
    pub audit_state: Option<AuditState>,
    /// Reason for a degraded audit.
    pub audit_note: Option<String>,
    /// Caps reached.
    pub caps_hit: BTreeSet<String>,
    /// Planned review rounds that ran.
    pub review_rounds_done: u32,
    /// Hard errors of the last check.
    pub hard_errors: Vec<String>,
    /// Printed numbers the last check found unbacked.
    pub unbacked_numbers: usize,
}

/// Failure to write the manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// A complete paper has no compiled PDF.
    #[error("the paper is complete but {0} is missing")]
    MissingPdf(String),
    /// A file could not be read or written.
    #[error("{what}: {source}")]
    Io {
        /// What was being done.
        what: String,
        /// The cause.
        source: std::io::Error,
    },
}

fn io_error(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> ManifestError {
    let what = what.into();
    move |source| ManifestError::Io { what, source }
}

/// Describes `dir/name`, or `None` when it is not a regular file.
///
/// # Errors
///
/// Returns an error when the file exists and cannot be read.
pub fn file_record(dir: &Path, name: &str) -> Result<Option<FileRecord>, ManifestError> {
    let path = dir.join(name);
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Ok(None);
    }
    let mut file = std::fs::File::open(&path).map_err(io_error(format!("cannot read {name}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(io_error(format!("cannot read {name}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(Some(FileRecord {
        path: name.to_string(),
        sha256: hex::encode(hasher.finalize()),
        bytes: meta.len(),
    }))
}

/// Builds the manifest for `outcome` from the files in `dir`.
///
/// A complete paper must have `paper.pdf`; an incomplete one records the PDF
/// only when an earlier compile left one.
///
/// # Errors
///
/// Returns [`ManifestError::MissingPdf`] for a complete paper without a PDF,
/// or an I/O error from reading the files.
pub fn build(dir: &Path, outcome: &Outcome) -> Result<Manifest, ManifestError> {
    let pdf = file_record(dir, "paper.pdf")?;
    if outcome.status == Status::Complete && pdf.is_none() {
        return Err(ManifestError::MissingPdf("paper.pdf".to_string()));
    }
    Ok(Manifest {
        status: outcome.status,
        reason: outcome.reason.clone(),
        venue: outcome.venue.clone(),
        repair_rounds_used: outcome.repair_rounds_used,
        repair_round_limit: outcome.repair_round_limit,
        audit_state: outcome.audit_state.unwrap_or(AuditState::Missing),
        audit_note: outcome.audit_note.clone(),
        caps_hit: outcome.caps_hit.iter().cloned().collect(),
        review_rounds_done: outcome.review_rounds_done,
        validation_ok: outcome.hard_errors.is_empty(),
        hard_errors: outcome.hard_errors.clone(),
        unbacked_numbers: outcome.unbacked_numbers,
        pdf,
        tex: file_record(dir, "paper.tex")?,
    })
}

/// Writes `paper_manifest.json` into `dir`.
///
/// # Errors
///
/// Returns the error from [`build`] or from writing the file.
pub fn write(dir: &Path, outcome: &Outcome) -> Result<Manifest, ManifestError> {
    let manifest = build(dir, outcome)?;
    let mut body = serde_json::to_string_pretty(&manifest).map_err(|e| ManifestError::Io {
        what: "cannot encode the manifest".to_string(),
        source: std::io::Error::other(e),
    })?;
    body.push('\n');
    std::fs::write(dir.join(MANIFEST_FILE), body)
        .map_err(io_error(format!("cannot write {MANIFEST_FILE}")))?;
    Ok(manifest)
}

/// Copies the files of one round into `rounds/NN/` and returns the folder.
///
/// Files that do not exist are skipped. Round numbers start at 1.
///
/// # Errors
///
/// Returns an I/O error when the folder or a copy cannot be written.
pub fn archive_round(dir: &Path, round: u32) -> Result<std::path::PathBuf, ManifestError> {
    let target = dir.join(ROUNDS_DIR).join(format!("{round:02}"));
    std::fs::create_dir_all(&target)
        .map_err(io_error(format!("cannot create {}", target.display())))?;
    for name in ROUND_FILES {
        let from = dir.join(name);
        let is_file = std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_file());
        if is_file {
            std::fs::copy(&from, target.join(name))
                .map_err(io_error(format!("cannot copy {name}")))?;
        }
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(status: Status) -> Outcome {
        Outcome {
            status,
            reason: None,
            venue: "iclr".into(),
            repair_rounds_used: 2,
            repair_round_limit: 10,
            audit_state: Some(AuditState::Passed),
            audit_note: None,
            caps_hit: BTreeSet::new(),
            review_rounds_done: 0,
            hard_errors: Vec::new(),
            unbacked_numbers: 0,
        }
    }

    #[test]
    fn a_complete_paper_needs_its_pdf() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("paper.tex"), "x").expect("write");
        let err = build(dir.path(), &outcome(Status::Complete)).expect_err("no pdf");
        assert!(err.to_string().contains("paper.pdf"));
        std::fs::write(dir.path().join("paper.pdf"), "%PDF").expect("write");
        let manifest = write(dir.path(), &outcome(Status::Complete)).expect("manifest");
        assert_eq!(manifest.pdf.expect("pdf").bytes, 4);
        assert_eq!(manifest.repair_rounds_used, 2);
    }

    #[test]
    fn the_manifest_records_state_and_caps() {
        let dir = tempfile::tempdir().expect("dir");
        let mut run = outcome(Status::Incomplete);
        run.reason = Some("repair counter used up".into());
        run.audit_state = Some(AuditState::Findings);
        run.caps_hit.insert("paper.max_repair_rounds".into());
        run.hard_errors = vec!["bad".into()];
        write(dir.path(), &run).expect("write");
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(MANIFEST_FILE)).expect("read"),
        )
        .expect("json");
        assert_eq!(json["status"], "incomplete");
        assert_eq!(json["audit_state"], "findings");
        assert_eq!(json["repair_rounds_used"], 2);
        assert_eq!(json["caps_hit"][0], "paper.max_repair_rounds");
        assert_eq!(json["validation_ok"], false);
        assert!(json["pdf"].is_null());
    }

    #[test]
    fn a_degraded_audit_is_never_recorded_as_passed() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("paper.pdf"), "%PDF").expect("write");
        let mut run = outcome(Status::Complete);
        run.audit_state = Some(AuditState::AuditDegraded);
        run.audit_note = Some("no verdict line".into());
        write(dir.path(), &run).expect("write");
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_FILE)).expect("read");
        assert!(text.contains("\"audit_degraded\""));
        assert!(!text.contains("\"passed\""));
    }

    #[test]
    fn an_unfinished_audit_is_recorded_as_missing() {
        let dir = tempfile::tempdir().expect("dir");
        let mut run = outcome(Status::Incomplete);
        run.audit_state = None;
        let manifest = write(dir.path(), &run).expect("write");
        assert_eq!(manifest.audit_state, AuditState::Missing);
    }

    #[test]
    fn rounds_are_archived_by_number() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("paper.tex"), "v1").expect("write");
        std::fs::write(dir.path().join("repair_feedback.md"), "fix it").expect("write");
        let folder = archive_round(dir.path(), 3).expect("archive");
        assert!(folder.ends_with("rounds/03"));
        assert_eq!(
            std::fs::read_to_string(folder.join("paper.tex")).expect("read"),
            "v1"
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("repair_feedback.md")).expect("read"),
            "fix it"
        );
        assert!(!folder.join("validation_report.json").exists());
    }
}
