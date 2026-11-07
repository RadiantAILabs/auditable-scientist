//! The evidence audit: parsing the auditor's raw reply, hashing the paper
//! inputs it saw, and writing `paper_evidence_audit.json` and `.md`.
//!
//! The auditor replies in plain lines (`AUDIT_VERDICT: PASS|FAIL`,
//! `AUDIT_SUMMARY: …`, `AUDIT_FINDING_COUNT: n` and one
//! `AUDIT_FINDING: id=… || section=… || claim=… || issue=… || evidence=… || fix=…`
//! line per finding). A reply that breaks that contract is not a finding:
//! after the allowed retries the audit is recorded as degraded, which is
//! weaker than a passed audit and never reported as one.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::claims::AuditState;

/// File name of the machine-readable audit record.
pub const AUDIT_FILE: &str = "paper_evidence_audit.json";

/// File name of the readable audit record.
pub const AUDIT_MARKDOWN_FILE: &str = "paper_evidence_audit.md";

/// Largest reply the parser reads.
const MAX_REPLY_CHARS: usize = 128 * 1024;

/// Most findings one audit may list.
const MAX_FINDINGS: usize = 20;

/// Most files whose hashes are recorded.
const MAX_HASHED_FILES: usize = 4000;

/// Files the paper steps write themselves; they are not audit inputs.
const GENERATED: [&str; 11] = [
    "repair_feedback.md",
    "review_feedback.md",
    "paper_review.json",
    "validation_report.json",
    "paper_evidence_audit.json",
    "paper_evidence_audit.md",
    "paper_manifest.json",
    "claims_check.json",
    "paper.pdf",
    "paper.build.tex",
    "audit_response.txt",
];

/// Build products of the TeX run.
const BUILD_EXTENSIONS: [&str; 8] = [
    "aux",
    "log",
    "bbl",
    "blg",
    "out",
    "toc",
    "synctex.gz",
    "fls",
];

/// One mismatch the auditor found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Short identifier.
    pub id: String,
    /// Section of the paper.
    pub section: String,
    /// The claim as the paper states it.
    pub claim: String,
    /// What is wrong.
    pub issue: String,
    /// What the recorded evidence says.
    pub evidence: String,
    /// The change that would fix it.
    pub fix: String,
}

/// What the auditor decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// No mismatch.
    Pass,
    /// At least one mismatch.
    Fail,
}

/// A parsed audit reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audit {
    /// The verdict.
    pub verdict: Verdict,
    /// One-paragraph summary.
    pub summary: String,
    /// The findings; empty exactly when the verdict is `Pass`.
    pub findings: Vec<Finding>,
}

impl Audit {
    /// The state this audit gives the paper.
    #[must_use]
    pub fn state(&self) -> AuditState {
        match self.verdict {
            Verdict::Pass => AuditState::Passed,
            Verdict::Fail => AuditState::Findings,
        }
    }
}

/// The reply does not follow the line contract.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ContractError(pub String);

fn bad<T>(message: impl Into<String>) -> Result<T, ContractError> {
    Err(ContractError(message.into()))
}

/// Parses the auditor's reply.
///
/// # Errors
///
/// Returns [`ContractError`] when the reply is too long, lacks the verdict,
/// summary or count line, declares a count that differs from the findings
/// listed, or has a `PASS` with findings or a `FAIL` without any.
pub fn parse_response(reply: &str) -> Result<Audit, ContractError> {
    if reply.chars().count() > MAX_REPLY_CHARS {
        return bad("the audit reply is longer than the contract allows");
    }
    let mut verdict: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut declared: Option<usize> = None;
    let mut findings = Vec::new();
    for line in reply.replace("\r\n", "\n").lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("AUDIT_VERDICT:") {
            verdict = Some(rest.trim().to_ascii_uppercase());
        } else if let Some(rest) = line.strip_prefix("AUDIT_SUMMARY:") {
            summary = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("AUDIT_FINDING_COUNT:") {
            let rest = rest.trim();
            match rest.parse::<usize>() {
                Ok(n) if rest.bytes().all(|b| b.is_ascii_digit()) => declared = Some(n),
                _ => return bad(format!("AUDIT_FINDING_COUNT is not a number: {line}")),
            }
        } else if let Some(rest) = line.strip_prefix("AUDIT_FINDING:") {
            findings.push(parse_finding(rest)?);
        }
    }
    let verdict = match verdict.as_deref() {
        Some("PASS") => Verdict::Pass,
        Some("FAIL") => Verdict::Fail,
        _ => return bad("AUDIT_VERDICT is missing or is not PASS or FAIL"),
    };
    let summary = match summary {
        Some(s) if !s.is_empty() => s,
        _ => return bad("AUDIT_SUMMARY is missing"),
    };
    let Some(declared) = declared else {
        return bad("AUDIT_FINDING_COUNT is missing");
    };
    if declared != findings.len() {
        return bad(format!(
            "AUDIT_FINDING_COUNT is {declared} but {} findings are listed",
            findings.len()
        ));
    }
    if findings.len() > MAX_FINDINGS {
        return bad(format!("the audit lists more than {MAX_FINDINGS} findings"));
    }
    match (verdict, findings.is_empty()) {
        (Verdict::Pass, false) => return bad("AUDIT_VERDICT PASS cannot list findings"),
        (Verdict::Fail, true) => return bad("AUDIT_VERDICT FAIL must list at least one finding"),
        _ => {}
    }
    Ok(Audit {
        verdict,
        summary,
        findings,
    })
}

fn parse_finding(body: &str) -> Result<Finding, ContractError> {
    const KEYS: [&str; 6] = ["id", "section", "claim", "issue", "evidence", "fix"];
    let mut fields: BTreeMap<&'static str, String> = BTreeMap::new();
    for part in body.trim().split(" || ") {
        let Some((key, value)) = part.split_once('=') else {
            return bad(format!("AUDIT_FINDING field has no `=`: {}", part.trim()));
        };
        let Some(key) = KEYS.into_iter().find(|k| *k == key.trim()) else {
            return bad(format!(
                "AUDIT_FINDING has an unknown field `{}`",
                key.trim()
            ));
        };
        if fields.insert(key, value.trim().to_string()).is_some() {
            return bad(format!("AUDIT_FINDING repeats the field `{key}`"));
        }
    }
    let mut take = |key: &str| -> Result<String, ContractError> {
        match fields.remove(key) {
            Some(v) if !v.is_empty() => Ok(v),
            _ => bad(format!("AUDIT_FINDING lacks a value for `{key}`")),
        }
    };
    Ok(Finding {
        id: take("id")?,
        section: take("section")?,
        claim: take("claim")?,
        issue: take("issue")?,
        evidence: take("evidence")?,
        fix: take("fix")?,
    })
}

/// The readable form of an audit.
#[must_use]
pub fn render_markdown(audit: &Audit) -> String {
    let verdict = match audit.verdict {
        Verdict::Pass => "pass",
        Verdict::Fail => "fail",
    };
    let mut out = format!(
        "# Evidence audit\n\n- verdict: `{verdict}`\n- finding_count: `{}`\n\n## Summary\n{}\n\n## Findings\n",
        audit.findings.len(),
        audit.summary
    );
    if audit.findings.is_empty() {
        out.push_str("- None.\n");
        return out;
    }
    for (index, f) in audit.findings.iter().enumerate() {
        out.push_str(&format!(
            "### {}. {} — {}\n- claim: {}\n- issue: {}\n- evidence: {}\n- requested_fix: {}\n\n",
            index + 1,
            f.id,
            f.section,
            f.claim,
            f.issue,
            f.evidence,
            f.fix
        ));
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// SHA-256 of every file the audit reads in `dir`, by path relative to `dir`.
///
/// Files the paper steps generate and the TeX build products are left out.
/// Symbolic links are not followed and are recorded as `symlink`, so a link
/// that appears or changes is a change of the inputs.
///
/// # Errors
///
/// Returns a message when a file cannot be read or the directory holds more
/// than 4000 files.
pub fn input_hashes(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for entry in WalkDir::new(dir).follow_links(false).sort_by_file_name() {
        let entry = entry.map_err(|e| format!("cannot walk {}: {e}", dir.display()))?;
        let Ok(relative) = entry.path().strip_prefix(dir) else {
            continue;
        };
        let name = relative.to_string_lossy().replace('\\', "/");
        if name.is_empty() || is_generated(&name) {
            continue;
        }
        let kind = entry.file_type();
        if kind.is_symlink() {
            out.insert(name, "symlink".to_string());
        } else if kind.is_file() {
            if out.len() >= MAX_HASHED_FILES {
                return Err(format!(
                    "the paper directory holds more than {MAX_HASHED_FILES} files"
                ));
            }
            out.insert(name.clone(), hash_file(entry.path(), &name)?);
        }
    }
    Ok(out)
}

fn is_generated(name: &str) -> bool {
    if GENERATED.contains(&name) || name.starts_with("rounds/") {
        return true;
    }
    if name.contains('/') {
        return false;
    }
    BUILD_EXTENSIONS
        .iter()
        .any(|ext| name.strip_prefix("paper.").is_some_and(|rest| rest == *ext))
}

fn hash_file(path: &Path, name: &str) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("cannot read {name}: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("cannot read {name}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Names the files that differ between two hash sets, in sorted order.
#[must_use]
pub fn changed_files(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut names: Vec<String> = before
        .keys()
        .chain(after.keys())
        .filter(|k| before.get(*k) != after.get(*k))
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Writes the audit record and its readable copy into `dir`.
///
/// # Errors
///
/// Returns the I/O error from writing either file.
pub fn write_record(
    dir: &Path,
    audit: &Audit,
    hashes: &BTreeMap<String, String>,
) -> std::io::Result<()> {
    let state = match audit.state() {
        AuditState::Passed => "passed",
        _ => "findings",
    };
    let verdict = match audit.verdict {
        Verdict::Pass => "pass",
        Verdict::Fail => "fail",
    };
    let record = serde_json::json!({
        "state": state,
        "verdict": verdict,
        "summary": audit.summary,
        "finding_count": audit.findings.len(),
        "findings": audit.findings,
        "input_sha256": hashes,
    });
    write_json(&dir.join(AUDIT_FILE), &record)?;
    std::fs::write(dir.join(AUDIT_MARKDOWN_FILE), render_markdown(audit))
}

/// Writes the record of an audit that could not be read after `attempts`
/// tries. The record says `audit_degraded` and carries no verdict.
///
/// # Errors
///
/// Returns the I/O error from writing either file.
pub fn write_degraded(
    dir: &Path,
    reason: &str,
    attempts: u32,
    hashes: &BTreeMap<String, String>,
) -> std::io::Result<()> {
    let summary =
        format!("The audit reply broke the output contract after {attempts} attempts: {reason}");
    let record = serde_json::json!({
        "state": "audit_degraded",
        "verdict": "unavailable",
        "summary": summary,
        "reason": reason,
        "attempts": attempts,
        "finding_count": 0,
        "findings": [],
        "input_sha256": hashes,
    });
    write_json(&dir.join(AUDIT_FILE), &record)?;
    std::fs::write(
        dir.join(AUDIT_MARKDOWN_FILE),
        format!(
            "# Evidence audit\n\n- state: `audit_degraded`\n- attempts: `{attempts}`\n\n## Summary\n{summary}\n\nThis paper was not audited. It is not an audit pass.\n"
        ),
    )
}

fn write_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let mut body = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    body.push('\n');
    std::fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINDING: &str = "AUDIT_FINDING: id=F1 || section=Results || claim=beats baseline || issue=margin is within noise || evidence=paper_data.json#a || fix=say the gap is not significant";

    fn reply(verdict: &str, count: usize, findings: &[&str]) -> String {
        let mut text = format!(
            "```\nAUDIT_VERDICT: {verdict}\nAUDIT_SUMMARY: checked all claims\nAUDIT_FINDING_COUNT: {count}\n"
        );
        for f in findings {
            text.push_str(f);
            text.push('\n');
        }
        text.push_str("```\n");
        text
    }

    #[test]
    fn a_pass_without_findings_parses() {
        let audit = parse_response(&reply("PASS", 0, &[])).expect("parse");
        assert_eq!(audit.verdict, Verdict::Pass);
        assert_eq!(audit.state(), AuditState::Passed);
        assert!(audit.findings.is_empty());
    }

    #[test]
    fn a_fail_lists_its_findings() {
        let audit = parse_response(&reply("fail", 1, &[FINDING])).expect("parse");
        assert_eq!(audit.state(), AuditState::Findings);
        assert_eq!(audit.findings[0].id, "F1");
        assert_eq!(audit.findings[0].fix, "say the gap is not significant");
    }

    #[test]
    fn contract_breaks_are_errors() {
        let cases = [
            ("no lines at all", "AUDIT_VERDICT is missing"),
            ("AUDIT_VERDICT: MAYBE\n", "AUDIT_VERDICT"),
            (
                "AUDIT_VERDICT: PASS\nAUDIT_FINDING_COUNT: 0\n",
                "AUDIT_SUMMARY",
            ),
            (
                "AUDIT_VERDICT: PASS\nAUDIT_SUMMARY: ok\n",
                "AUDIT_FINDING_COUNT is missing",
            ),
            (
                "AUDIT_VERDICT: PASS\nAUDIT_SUMMARY: ok\nAUDIT_FINDING_COUNT: x\n",
                "not a number",
            ),
        ];
        for (text, expect) in cases {
            let err = parse_response(text).expect_err(text).to_string();
            assert!(err.contains(expect), "{text:?} -> {err}");
        }
        let err = parse_response(&reply("PASS", 1, &[FINDING])).expect_err("pass with finding");
        assert!(err.to_string().contains("cannot list findings"));
        let err = parse_response(&reply("FAIL", 0, &[])).expect_err("fail without finding");
        assert!(err.to_string().contains("at least one"));
        let err = parse_response(&reply("FAIL", 2, &[FINDING])).expect_err("count differs");
        assert!(err.to_string().contains("2 but 1"));
    }

    #[test]
    fn malformed_finding_lines_are_errors() {
        for line in [
            "AUDIT_FINDING: id=F1 || section=S",
            "AUDIT_FINDING: id=F1 || nonsense",
            "AUDIT_FINDING: id=F1 || colour=red || section=S || claim=c || issue=i || evidence=e || fix=f",
            "AUDIT_FINDING: id=F1 || id=F2 || section=S || claim=c || issue=i || evidence=e || fix=f",
            "AUDIT_FINDING: id= || section=S || claim=c || issue=i || evidence=e || fix=f",
        ] {
            assert!(
                parse_response(&reply("FAIL", 1, &[line])).is_err(),
                "{line}"
            );
        }
    }

    #[test]
    fn hashes_skip_generated_files_and_notice_changes() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("paper.tex"), "a").expect("write");
        std::fs::write(dir.path().join("paper.aux"), "x").expect("write");
        std::fs::write(dir.path().join("validation_report.json"), "{}").expect("write");
        std::fs::create_dir(dir.path().join("figures")).expect("dir");
        std::fs::write(dir.path().join("figures/f.png"), "p").expect("write");
        let before = input_hashes(dir.path()).expect("hash");
        assert_eq!(
            before.keys().cloned().collect::<Vec<_>>(),
            ["figures/f.png", "paper.tex"]
        );
        std::fs::write(dir.path().join("paper.tex"), "b").expect("write");
        std::fs::write(dir.path().join("paper.pdf"), "pdf").expect("write");
        let after = input_hashes(dir.path()).expect("hash");
        assert_eq!(changed_files(&before, &after), ["paper.tex"]);
    }

    #[test]
    fn records_carry_the_state_and_never_pass_a_degraded_audit() {
        let dir = tempfile::tempdir().expect("dir");
        let hashes = BTreeMap::from([("paper.tex".to_string(), "00".to_string())]);
        write_degraded(dir.path(), "no verdict line", 3, &hashes).expect("write");
        let record: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(AUDIT_FILE)).expect("read"),
        )
        .expect("json");
        assert_eq!(record["state"], "audit_degraded");
        assert_eq!(record["attempts"], 3);
        assert_eq!(record["finding_count"], 0);
        assert_eq!(record["input_sha256"]["paper.tex"], "00");
        let markdown = std::fs::read_to_string(dir.path().join(AUDIT_MARKDOWN_FILE)).expect("read");
        assert!(markdown.contains("not an audit pass"));

        let audit = parse_response(&reply("FAIL", 1, &[FINDING])).expect("parse");
        write_record(dir.path(), &audit, &hashes).expect("write");
        let record: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(AUDIT_FILE)).expect("read"),
        )
        .expect("json");
        assert_eq!(record["state"], "findings");
        assert_eq!(record["verdict"], "fail");
        assert_eq!(record["finding_count"], 1);
    }
}
