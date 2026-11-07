//! Offline check of a finished paper against its recorded claims.
//!
//! [`check_paper`] reads the files a paper directory already holds
//! (`claims_grounding.json`, `paper_data.json`, `paper.tex` with the TeX files
//! it imports, `references.bib`, `references_provenance.json` and
//! `paper_evidence_audit.json`). It uses no model and no network, and the same
//! directory always gives the same answer.
//!
//! The report keeps two kinds of result apart:
//!
//! * `mechanical_check` is computed by this run from the files: whether each
//!   claim names sources and citation keys that exist, and whether every number
//!   the manuscript prints is backed by a recorded value or a recorded
//!   derivation (see [`check`] for the rule). Its `passed` verdict needs every
//!   declared claim to resolve; `unresolved_claims` lists the ones that do not,
//!   each with the reason.
//! * `saved_model_audit` is read from `paper_evidence_audit.json`, which the
//!   evidence-audit agent wrote earlier. This check does not run the audit; the
//!   report says so beside the state.
//!
//! The check reads only what both the Polaris scientist and the original
//! system write: `paper.tex` and the TeX files it imports, `claims_grounding.json`
//! (`claim`, `source_ids`, `citation_keys`), `paper_data.json`
//! (`grounding_sources`, each with a numeric `value`) and, when present,
//! `references_provenance.json` and `paper_evidence_audit.json`. The optional
//! `derivations` of a claim and the optional `values` of a source are used when
//! they are there; a paper without them simply has fewer ways to back a number.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::latex::text;

pub mod check;
pub mod derive;
pub mod manuscript;
pub mod numbers;

pub use check::{Backing, ClaimInput, NumberFinding, NumberReport, Reason, SourceValues};
pub use manuscript::Manuscript;

/// Version of the rules in this module; recorded in every report.
pub const CHECKER_VERSION: &str = "4";

/// File name of the report the check writes.
pub const REPORT_FILE: &str = "claims_check.json";

/// What the evidence audit of the paper concluded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditState {
    /// The audit ran and found nothing to fix.
    Passed,
    /// The audit ran and listed findings.
    Findings,
    /// The audit could not run or its output could not be read.
    AuditDegraded,
    /// There is no audit record.
    Missing,
}

/// What this run computed from the files of the paper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MechanicalCheck {
    /// What the section is, in plain words.
    pub what: String,
    /// True when every claim in `claims_grounding.json` resolves, every printed
    /// number is backed, every recorded derivation computes, and every import was
    /// read. This says nothing about the saved model audit.
    pub passed: bool,
    /// Claims listed in `claims_grounding.json`.
    pub claims_listed: usize,
    /// Of those, claims whose sources and citation keys all exist.
    pub claims_resolved: usize,
    /// The claims that do not resolve, each with the reason. Empty when
    /// `claims_resolved` equals `claims_listed`.
    #[serde(default)]
    pub unresolved_claims: Vec<UnresolvedClaim>,
    /// TeX files read: `paper.tex` and the files it imports.
    pub files_read: Vec<String>,
    /// Imports that could not be followed.
    pub import_problems: Vec<String>,
    /// Numbers the manuscript prints.
    pub numbers_printed: usize,
    /// Of those, numbers backed by a recorded value or derivation.
    pub numbers_backed: usize,
    /// Numbers that are not backed.
    pub numbers_unbacked: usize,
    /// Problems with the derivations that claims record.
    pub derivation_problems: Vec<String>,
    /// The numbers that are not backed, each with its place and the reason.
    pub unbacked: Vec<NumberFinding>,
    /// The numbers that are backed, each with the claim and value behind it.
    pub backed: Vec<NumberFinding>,
}

/// A claim of `claims_grounding.json` that does not resolve: it names no source
/// and no citation key, or it names one that does not exist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedClaim {
    /// The id of the claim, or `claims[<index>]` when it has none.
    pub id: String,
    /// The text of the claim.
    pub claim: String,
    /// Why the claim does not resolve, in plain words.
    pub reason: String,
    /// Source ids the claim names that no recorded source or reference has.
    pub unknown_source_ids: Vec<String>,
    /// Citation keys the claim names that `references.bib` does not hold.
    pub unknown_citation_keys: Vec<String>,
}

/// What the saved evidence audit says, copied from its file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedAudit {
    /// What the section is, in plain words.
    pub what: String,
    /// The state the saved audit file records.
    pub state: AuditState,
    /// The file the state was read from.
    pub file: String,
}

/// Result of [`check_paper`]; the field names are the report's keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimsCheck {
    /// [`CHECKER_VERSION`] when the report was made.
    pub checker_version: String,
    /// The part this run computed.
    pub mechanical_check: MechanicalCheck,
    /// The part that is only read from the saved audit file.
    pub saved_model_audit: SavedAudit,
}

/// A paper directory the check cannot read.
#[derive(Debug, thiserror::Error)]
pub enum ClaimsError {
    /// A file the check needs is not there.
    #[error("{} is missing", .path.display())]
    Missing {
        /// The missing file.
        path: PathBuf,
    },
    /// A file exists but cannot be read.
    #[error("cannot read {}: {source}", .path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// A file is read but is not in the expected shape.
    #[error("{} is not usable: {reason}", .path.display())]
    Malformed {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
}

/// Checks the claims, numbers and audit record of the paper in `paper_dir`.
///
/// # Errors
///
/// Returns [`ClaimsError`] when `claims_grounding.json`, `paper_data.json` or
/// `paper.tex` is missing or unreadable, or when either JSON file is not in
/// the expected shape, or when `references_provenance.json` is present and is
/// not JSON. A missing `references.bib` or `references_provenance.json` is not
/// an error: they leave citation keys and reference ids unresolved. A missing
/// audit record reports [`AuditState::Missing`].
pub fn check_paper(paper_dir: &Path) -> Result<ClaimsCheck, ClaimsError> {
    let grounding = read_json(&paper_dir.join("claims_grounding.json"))?;
    let data = read_json(&paper_dir.join("paper_data.json"))?;
    let tex_path = paper_dir.join("paper.tex");
    let tex = manuscript::read_regular(&tex_path).map_err(|reason| {
        if reason.ends_with("is missing") {
            ClaimsError::Missing {
                path: tex_path.clone(),
            }
        } else {
            ClaimsError::Malformed {
                path: tex_path.clone(),
                reason,
            }
        }
    })?;
    let bib = read_optional_text(&paper_dir.join("references.bib"))?;

    let claims = claim_inputs(&grounding, &paper_dir.join("claims_grounding.json"))?;
    let sources = grounding_sources(&data, &paper_dir.join("paper_data.json"))?;
    let keys = bib.as_deref().map(text::bib_keys).unwrap_or_default();
    let mut known: BTreeSet<String> = sources.keys().cloned().collect();
    known.extend(reference_ids(
        &paper_dir.join("references_provenance.json"),
    )?);

    let unresolved: Vec<UnresolvedClaim> = grounding_claims(&grounding)
        .iter()
        .enumerate()
        .filter_map(|(index, claim)| unresolved_claim(claim, index, &known, &keys))
        .collect();
    let resolved = claims.len() - unresolved.len();

    let manuscript = Manuscript::from_main(paper_dir, &tex);
    let import_problems: Vec<String> = manuscript
        .problems
        .iter()
        .chain(&manuscript.unread)
        .cloned()
        .collect();
    let report = check::check(
        &manuscript.printed_numbers(),
        &claims,
        &SourceValues::from_sources(&sources),
    );
    let (backed, unbacked): (Vec<_>, Vec<_>) = report
        .findings
        .iter()
        .cloned()
        .partition(NumberFinding::is_backed);

    Ok(ClaimsCheck {
        checker_version: CHECKER_VERSION.to_string(),
        mechanical_check: MechanicalCheck {
            what: "Computed by this run from the files of the paper folder.".to_string(),
            passed: unresolved.is_empty()
                && unbacked.is_empty()
                && report.problems.is_empty()
                && import_problems.is_empty(),
            claims_listed: claims.len(),
            claims_resolved: resolved,
            unresolved_claims: unresolved,
            files_read: manuscript.files.clone(),
            import_problems,
            numbers_printed: report.printed(),
            numbers_backed: backed.len(),
            numbers_unbacked: unbacked.len(),
            derivation_problems: report.problems,
            unbacked,
            backed,
        },
        saved_model_audit: SavedAudit {
            what: "Read from the saved audit file. This check did not run the evidence audit; \
                   the agent that audited the paper wrote the file earlier."
                .to_string(),
            state: audit_state(paper_dir),
            file: "paper_evidence_audit.json".to_string(),
        },
    })
}

/// The hard errors a number check gives the repair step: one message for each
/// sentence or table row that prints a number nothing backs, and one for each
/// derivation that cannot be computed. At most `limit` messages are returned;
/// the last one counts the rest.
#[must_use]
pub fn explain(report: &NumberReport, limit: usize) -> Vec<String> {
    let mut messages: Vec<String> = report
        .problems
        .iter()
        .map(|problem| format!("claims_grounding.json: {problem}"))
        .collect();
    let mut places: Vec<(&NumberFinding, Vec<&NumberFinding>)> = Vec::new();
    for finding in report.unbacked() {
        match places.last_mut() {
            Some((first, group))
                if first.file == finding.file
                    && first.line == finding.line
                    && first.excerpt == finding.excerpt =>
            {
                group.push(finding);
            }
            _ => places.push((finding, vec![finding])),
        }
    }
    for (first, group) in &places {
        let printed: Vec<&str> = group.iter().map(|f| f.printed.as_str()).collect();
        messages.push(format!(
            "{}:{} prints {} in \"{}\": {}.",
            first.file,
            first.line,
            join_numbers(&printed),
            first.excerpt,
            reason_text(group)
        ));
    }
    // Room for the closing advice.
    let room = limit.saturating_sub(1);
    if messages.len() > room && room > 0 {
        let rest = messages.len() - (room - 1);
        messages.truncate(room - 1);
        messages.push(format!(
            "{rest} more places print numbers that nothing backs; fix these first."
        ));
    }
    if places.is_empty() {
        return messages;
    }
    messages.push(NUMBER_ADVICE.to_string());
    messages
}

/// The advice that closes a list of unbacked numbers.
const NUMBER_ADVICE: &str = "To fix an unbacked number: add or correct a claim in claims_grounding.json that writes the number exactly as printed, shares words with the sentence or table it appears in, and cites the source that holds the value (or records a derivation from recorded sources); if no recorded value gives the number, remove it from the text. Do not delete the number from a claim while the text still prints it.";

fn join_numbers(printed: &[&str]) -> String {
    match printed {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn reason_text(group: &[&NumberFinding]) -> String {
    let mut parts = Vec::new();
    for finding in group {
        let part = match finding.reason {
            Some(Reason::NotStated) => format!(
                "no claim states {} (a claim must write it exactly as printed)",
                finding.printed
            ),
            Some(Reason::ClaimIsAboutOtherText) => format!(
                "claim {} states {} but shares no words with this sentence",
                finding.claims_considered.join(", "),
                finding.printed
            ),
            Some(Reason::NoRecordedValue) => {
                let nearest = finding.nearest.as_ref().map_or_else(
                    || "its sources hold no number".to_string(),
                    |n| {
                        format!(
                            "the nearest recorded value is {} in `{}`",
                            n.value, n.source_id
                        )
                    },
                );
                format!(
                    "claim {} states {} but no source it cites and no derivation it records gives that number with that sign ({nearest})",
                    finding.claims_considered.join(", "),
                    finding.printed
                )
            }
            Some(Reason::TableNotCovered) => format!(
                "no claim is about this table, so {} is not covered",
                finding.printed
            ),
            None => continue,
        };
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    parts.join("; ")
}

/// Writes `check` to `path` as indented JSON with a trailing newline.
///
/// # Errors
///
/// Returns the I/O error from creating the parent directory or the file.
pub fn write_report(check: &ClaimsCheck, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = serde_json::to_string_pretty(check).map_err(io::Error::other)?;
    body.push('\n');
    std::fs::write(path, body)
}

fn read_optional_text(path: &Path) -> Result<Option<String>, ClaimsError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ClaimsError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_json(path: &Path) -> Result<Value, ClaimsError> {
    let body = read_optional_text(path)?.ok_or_else(|| ClaimsError::Missing {
        path: path.to_path_buf(),
    })?;
    serde_json::from_str(&body).map_err(|e| ClaimsError::Malformed {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

fn grounding_claims(grounding: &Value) -> &[Value] {
    grounding
        .get("claims")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn string_list(claim: &Value, key: &str) -> Vec<String> {
    claim
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn claim_inputs(grounding: &Value, path: &Path) -> Result<Vec<ClaimInput>, ClaimsError> {
    if !grounding.get("claims").is_some_and(Value::is_array) {
        return Err(ClaimsError::Malformed {
            path: path.to_path_buf(),
            reason: "`claims` is not a list".to_string(),
        });
    }
    Ok(grounding_claims(grounding)
        .iter()
        .enumerate()
        .map(|(index, claim)| {
            let id = claim
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("claims[{index}]"), str::to_string);
            let (derivations, problems) = check::read_derivations(claim, &id);
            ClaimInput {
                text: claim
                    .get("claim")
                    .or_else(|| claim.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                source_ids: string_list(claim, "source_ids"),
                derivations,
                problems,
                id,
            }
        })
        .collect())
}

/// The `grounding_sources` of `paper_data.json` as an id-to-record map. The
/// field may be an object keyed by id or a list of records that carry an `id`.
pub(crate) fn grounding_sources(
    data: &Value,
    path: &Path,
) -> Result<Map<String, Value>, ClaimsError> {
    match data.get("grounding_sources") {
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(Value::Array(items)) => Ok(items
            .iter()
            .filter_map(|item| Some((item.get("id")?.as_str()?.to_string(), item.clone())))
            .collect()),
        _ => Err(ClaimsError::Malformed {
            path: path.to_path_buf(),
            reason: "`grounding_sources` is not an object or a list".to_string(),
        }),
    }
}

/// The ids of the references in `references_provenance.json`. A claim may cite
/// a reference by id as well as a recorded result. The file is optional; a
/// file without a `references` list gives no ids.
fn reference_ids(path: &Path) -> Result<BTreeSet<String>, ClaimsError> {
    if read_optional_text(path)?.is_none() {
        return Ok(BTreeSet::new());
    }
    let record = read_json(path)?;
    Ok(record
        .get("references")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// A claim resolves when it names at least one source or citation key and
/// every source and key it names exists. Returns why it does not, or `None` when
/// it does. `index` is the place of the claim in the list.
fn unresolved_claim(
    claim: &Value,
    index: usize,
    known: &BTreeSet<String>,
    keys: &BTreeSet<String>,
) -> Option<UnresolvedClaim> {
    let ids = string_list(claim, "source_ids");
    let cites = string_list(claim, "citation_keys");
    let unknown_source_ids: Vec<String> = ids
        .iter()
        .filter(|id| !known.contains(*id))
        .cloned()
        .collect();
    let unknown_citation_keys: Vec<String> = cites
        .iter()
        .filter(|key| !keys.contains(*key))
        .cloned()
        .collect();
    let mut reasons = Vec::new();
    if ids.is_empty() && cites.is_empty() {
        reasons.push("it names no source and no citation key".to_string());
    }
    if !unknown_source_ids.is_empty() {
        reasons.push(format!(
            "no recorded source or reference has the id {}",
            quoted(&unknown_source_ids)
        ));
    }
    if !unknown_citation_keys.is_empty() {
        reasons.push(format!(
            "references.bib has no entry {}",
            quoted(&unknown_citation_keys)
        ));
    }
    if reasons.is_empty() {
        return None;
    }
    Some(UnresolvedClaim {
        id: claim
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("claims[{index}]"), str::to_string),
        claim: claim
            .get("claim")
            .or_else(|| claim.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        reason: reasons.join("; "),
        unknown_source_ids,
        unknown_citation_keys,
    })
}

fn quoted(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("`{item}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Reads `paper_evidence_audit.json`. A record that carries a `state` uses it;
/// a record in the older shape is read from `verdict` and `finding_count`.
fn audit_state(paper_dir: &Path) -> AuditState {
    let path = paper_dir.join("paper_evidence_audit.json");
    let Ok(Some(body)) = read_optional_text(&path) else {
        return if path.exists() {
            AuditState::AuditDegraded
        } else {
            AuditState::Missing
        };
    };
    let Ok(record) = serde_json::from_str::<Value>(&body) else {
        return AuditState::AuditDegraded;
    };
    if let Some(state) = record.get("state").and_then(Value::as_str) {
        return match state {
            "passed" => AuditState::Passed,
            "findings" => AuditState::Findings,
            _ => AuditState::AuditDegraded,
        };
    }
    let findings = record
        .get("finding_count")
        .and_then(Value::as_u64)
        .or_else(|| {
            record
                .get("findings")
                .and_then(Value::as_array)
                .map(|f| f.len() as u64)
        });
    match (record.get("verdict").and_then(Value::as_str), findings) {
        (Some("pass"), Some(0)) => AuditState::Passed,
        (Some("pass" | "fail"), Some(n)) if n > 0 => AuditState::Findings,
        (Some("fail"), _) => AuditState::Findings,
        _ => AuditState::AuditDegraded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paper(audit: Option<Value>, claim_text: &str, tex_body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        let put = |name: &str, value: &Value| {
            std::fs::write(
                dir.path().join(name),
                serde_json::to_string(value).expect("json"),
            )
            .expect("write");
        };
        put(
            "claims_grounding.json",
            &json!({"claims": [
                {"claim": claim_text, "source_ids": ["m.a"], "citation_keys": ["smith2020"]},
                {"claim": "no sources at all", "source_ids": [], "citation_keys": []},
                {"claim": "unknown source", "source_ids": ["m.zzz"], "citation_keys": []}
            ]}),
        );
        put(
            "paper_data.json",
            &json!({"grounding_sources": {"m.a": {"id": "m.a", "type": "metric", "value": 0.959_764}}}),
        );
        std::fs::write(
            dir.path().join("paper.tex"),
            format!("\\begin{{document}}\n{tex_body}\n\\end{{document}}\n"),
        )
        .expect("write");
        std::fs::write(
            dir.path().join("references.bib"),
            "@article{smith2020,\n title={T}\n}\n",
        )
        .expect("write");
        if let Some(audit) = audit {
            put("paper_evidence_audit.json", &audit);
        }
        dir
    }

    #[test]
    fn check_counts_claims_numbers_and_keeps_the_saved_audit_apart() {
        let dir = paper(
            Some(json!({"verdict": "pass", "finding_count": 0, "findings": []})),
            "The seed ring scores 0.9598 on the held-out set",
            "The ring scores 0.9598 on the held-out set. % 0.1111 is a comment",
        );
        let check = check_paper(dir.path()).expect("check");
        let mechanical = &check.mechanical_check;
        assert_eq!(mechanical.claims_listed, 3);
        assert_eq!(mechanical.claims_resolved, 1);
        assert_eq!(
            (
                mechanical.numbers_printed,
                mechanical.numbers_backed,
                mechanical.numbers_unbacked
            ),
            (1, 1, 0)
        );
        assert!(mechanical.unbacked.is_empty());
        assert_eq!(
            mechanical.backed[0]
                .backed_by
                .as_ref()
                .map(|b| b.claim_id.as_str()),
            Some("claims[0]")
        );
        assert_eq!(check.saved_model_audit.state, AuditState::Passed);
        assert!(
            check
                .saved_model_audit
                .what
                .contains("did not run the evidence audit")
        );
        assert_eq!(check.checker_version, CHECKER_VERSION);
    }

    #[test]
    fn one_unresolved_claim_fails_the_verdict_and_is_named() {
        let dir = paper(None, "The seed ring scores 0.9598 on the held-out set", "x");
        let write = |claims: Value| {
            std::fs::write(
                dir.path().join("claims_grounding.json"),
                serde_json::to_string(&json!({ "claims": claims })).expect("json"),
            )
            .expect("write");
        };
        write(json!([
            {"id": "c1", "claim": "The ring scores well", "source_ids": ["m.a"], "citation_keys": ["smith2020"]},
            {"id": "c2", "claim": "The ablation removes the polish", "source_ids": ["ablation.nowhere"], "citation_keys": ["jones1999"]}
        ]));
        let mechanical = check_paper(dir.path()).expect("check").mechanical_check;
        assert_eq!(mechanical.claims_listed, 2);
        assert_eq!(mechanical.claims_resolved, 1);
        assert!(
            mechanical.unbacked.is_empty() && mechanical.derivation_problems.is_empty(),
            "everything else is fine, so the claim alone decides the verdict"
        );
        assert!(!mechanical.passed);
        assert_eq!(mechanical.unresolved_claims.len(), 1);
        let claim = &mechanical.unresolved_claims[0];
        assert_eq!(claim.id, "c2");
        assert_eq!(claim.claim, "The ablation removes the polish");
        assert_eq!(claim.unknown_source_ids, ["ablation.nowhere"]);
        assert_eq!(claim.unknown_citation_keys, ["jones1999"]);
        assert!(
            claim.reason.contains("`ablation.nowhere`") && claim.reason.contains("`jones1999`"),
            "{}",
            claim.reason
        );

        write(json!([
            {"id": "c1", "claim": "The ring scores well", "source_ids": ["m.a"], "citation_keys": ["smith2020"]},
            {"id": "c2", "claim": "The ablation removes the polish", "source_ids": ["m.a"], "citation_keys": []}
        ]));
        let mechanical = check_paper(dir.path()).expect("check").mechanical_check;
        assert_eq!(mechanical.claims_resolved, 2);
        assert!(mechanical.unresolved_claims.is_empty());
        assert!(mechanical.passed);
    }

    #[test]
    fn a_claim_that_names_nothing_is_unresolved_and_a_claim_without_an_id_is_named_by_place() {
        let dir = paper(None, "x", "x");
        let claims = json!({"claims": [
            {"claim": "fine", "source_ids": ["m.a"], "citation_keys": []},
            {"claim": "names nothing", "source_ids": [], "citation_keys": []}
        ]});
        std::fs::write(
            dir.path().join("claims_grounding.json"),
            serde_json::to_string(&claims).expect("json"),
        )
        .expect("write");
        let mechanical = check_paper(dir.path()).expect("check").mechanical_check;
        assert!(!mechanical.passed);
        assert_eq!(mechanical.unresolved_claims.len(), 1);
        assert_eq!(mechanical.unresolved_claims[0].id, "claims[1]");
        assert_eq!(
            mechanical.unresolved_claims[0].reason,
            "it names no source and no citation key"
        );
    }

    #[test]
    fn a_report_written_before_unresolved_claims_were_listed_still_reads() {
        let dir = paper(None, "x", "x");
        let report = serde_json::to_value(check_paper(dir.path()).expect("check")).expect("json");
        let mut older = report.clone();
        older["mechanical_check"]
            .as_object_mut()
            .expect("object")
            .remove("unresolved_claims");
        let read: ClaimsCheck = serde_json::from_value(older).expect("an older report reads");
        assert!(read.mechanical_check.unresolved_claims.is_empty());
    }

    #[test]
    fn a_number_the_claim_states_but_the_source_does_not_hold_is_reported() {
        let dir = paper(
            None,
            "The seed ring scores 0.9611 on the held-out set",
            "The ring scores 0.9611 on the held-out set.",
        );
        let check = check_paper(dir.path()).expect("check");
        let mechanical = &check.mechanical_check;
        assert_eq!(
            (mechanical.numbers_printed, mechanical.numbers_backed),
            (1, 0)
        );
        let finding = &mechanical.unbacked[0];
        assert_eq!(finding.printed, "0.9611");
        assert_eq!(finding.reason, Some(Reason::NoRecordedValue));
        assert_eq!(finding.nearest.as_ref().map(|n| n.value), Some(0.959_764));
        assert_eq!(check.saved_model_audit.state, AuditState::Missing);
    }

    #[test]
    fn a_number_the_prose_prints_without_a_claim_fails() {
        let dir = paper(
            Some(json!({"verdict": "pass", "finding_count": 0})),
            "The seed ring scores 0.9598 on the held-out set",
            "The ring scores 0.9598 on the held-out set. We ran 57 starts of the ring.",
        );
        let check = check_paper(dir.path()).expect("check");
        let mechanical = &check.mechanical_check;
        assert_eq!(
            (mechanical.numbers_printed, mechanical.numbers_backed),
            (2, 1)
        );
        assert_eq!(mechanical.unbacked[0].printed, "57");
        assert_eq!(mechanical.unbacked[0].reason, Some(Reason::NotStated));
        assert_eq!(
            check.saved_model_audit.state,
            AuditState::Passed,
            "the saved audit still reads as it was saved"
        );
    }

    #[test]
    fn explanations_group_numbers_by_sentence_and_say_what_to_do() {
        let dir = paper(
            None,
            "The seed ring scores 0.9598 on the held-out set",
            "The ring scores 0.9598 on the held-out set. We ran 57 starts of 21 rings.",
        );
        let check = check_paper(dir.path()).expect("check");
        let report = NumberReport {
            findings: [
                check.mechanical_check.backed.clone(),
                check.mechanical_check.unbacked.clone(),
            ]
            .concat(),
            problems: Vec::new(),
        };
        let messages = explain(&report, 10);
        assert_eq!(
            messages.len(),
            2,
            "one sentence and the closing advice: {messages:?}"
        );
        assert!(
            messages[0].starts_with("paper.tex:2 prints 57 and 21 in \""),
            "{}",
            messages[0]
        );
        assert!(messages[0].contains("no claim states 57"));
        assert!(
            messages[1].contains("remove it from the text"),
            "{}",
            messages[1]
        );
        assert!(messages[1].contains("Do not delete the number from a claim"));
        let capped = explain(&report, 2);
        assert_eq!(capped.len(), 2, "{capped:?}");
        assert!(explain(&NumberReport::default(), 5).is_empty());
    }

    #[test]
    fn check_is_repeatable_and_writes_the_frozen_keys() {
        let dir = paper(
            Some(json!({"verdict": "fail", "finding_count": 2})),
            "scores 0.9598 held-out",
            "scores 0.9598 held-out",
        );
        let first = check_paper(dir.path()).expect("check");
        assert_eq!(first, check_paper(dir.path()).expect("check again"));
        assert_eq!(first.saved_model_audit.state, AuditState::Findings);
        let out = dir.path().join("out").join(REPORT_FILE);
        write_report(&first, &out).expect("write");
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(out).expect("read")).expect("json");
        let mut keys: Vec<&str> = written
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["checker_version", "mechanical_check", "saved_model_audit"]
        );
        assert_eq!(written["saved_model_audit"]["state"], "findings");
        assert_eq!(written["mechanical_check"]["numbers_printed"], 1);
    }

    #[test]
    fn audit_state_reads_both_record_shapes_and_damage() {
        let cases = [
            (json!({"state": "passed"}), AuditState::Passed),
            (json!({"state": "findings"}), AuditState::Findings),
            (
                json!({"state": "audit_degraded"}),
                AuditState::AuditDegraded,
            ),
            (
                json!({"verdict": "pass", "finding_count": 0}),
                AuditState::Passed,
            ),
            (
                json!({"verdict": "fail", "finding_count": 3}),
                AuditState::Findings,
            ),
            (json!({"verdict": "pass"}), AuditState::AuditDegraded),
            (json!({"unrelated": true}), AuditState::AuditDegraded),
        ];
        for (record, expected) in cases {
            let dir = paper(Some(record.clone()), "x", "x");
            assert_eq!(
                check_paper(dir.path())
                    .expect("check")
                    .saved_model_audit
                    .state,
                expected,
                "{record}"
            );
        }
        let dir = paper(None, "x", "x");
        std::fs::write(dir.path().join("paper_evidence_audit.json"), "{not json").expect("write");
        assert_eq!(
            check_paper(dir.path())
                .expect("check")
                .saved_model_audit
                .state,
            AuditState::AuditDegraded
        );
    }

    #[test]
    fn missing_or_malformed_inputs_are_errors() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(matches!(
            check_paper(dir.path()),
            Err(ClaimsError::Missing { .. })
        ));
        let dir = paper(None, "x", "x");
        std::fs::write(dir.path().join("claims_grounding.json"), "{\"claims\": 3}").expect("write");
        assert!(matches!(
            check_paper(dir.path()),
            Err(ClaimsError::Malformed { .. })
        ));
        std::fs::write(dir.path().join("claims_grounding.json"), "nope").expect("write");
        assert!(matches!(
            check_paper(dir.path()),
            Err(ClaimsError::Malformed { .. })
        ));
    }
}
