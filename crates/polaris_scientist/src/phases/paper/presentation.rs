//! The presentation review: parsing the reviewer's reply and writing its
//! feedback for the repairer.
//!
//! The reviewer answers with one JSON object (`status`, `summary`, comments,
//! `score`). A reply that breaks that contract is not feedback: after the
//! allowed retries the phase goes on without structured feedback, as the
//! original does, and the repair that follows a review still happens.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// File name of the machine-readable review.
pub const REVIEW_FILE: &str = "paper_review.json";

/// File name of the review as feedback for the repairer.
pub const FEEDBACK_FILE: &str = "review_feedback.md";

/// Most characters of a reply that are read.
const MAX_REPLY_CHARS: usize = 128 * 1024;

/// One comment on the paper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    /// Short identifier.
    pub id: String,
    /// Section of the paper.
    pub section: String,
    /// What is wrong.
    pub issue: String,
    /// What backs the comment.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// The change asked for.
    pub requested_change: String,
}

/// The reviewer's score.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Score {
    /// From 1 to 10.
    pub value: u8,
    /// A word for the value.
    pub label: String,
}

/// Whether the paper may stay as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Only discretionary polish is left.
    Pass,
    /// At least one major comment needs a revision.
    Revise,
}

/// A parsed review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// The verdict.
    pub status: Status,
    /// One-paragraph assessment.
    pub summary: String,
    /// What the paper does well.
    #[serde(default)]
    pub strengths: Vec<String>,
    /// Problems that call for a revision.
    #[serde(default)]
    pub major_comments: Vec<Issue>,
    /// Smaller points.
    #[serde(default)]
    pub minor_comments: Vec<Issue>,
    /// Questions for the authors.
    #[serde(default)]
    pub questions: Vec<String>,
    /// The score.
    pub score: Score,
}

/// Parses the reviewer's reply.
///
/// A single surrounding code fence is accepted.
///
/// # Errors
///
/// Returns one sentence when the reply is too long, is not the JSON object of
/// the contract, has empty text where text is required, repeats an issue id,
/// has a major comment without evidence, or contradicts itself: `pass` with a
/// major comment, or `revise` without one.
pub fn parse_response(reply: &str) -> Result<Review, String> {
    if reply.chars().count() > MAX_REPLY_CHARS {
        return Err("the review reply is longer than the contract allows".into());
    }
    let body = unfence(reply.trim());
    let review: Review = serde_json::from_str(body)
        .map_err(|e| format!("the review reply is not the JSON object of the contract: {e}"))?;
    let empty = |text: &str| text.trim().is_empty();
    if empty(&review.summary) || empty(&review.score.label) {
        return Err("the review needs a non-empty summary and score label".into());
    }
    if !(1..=10).contains(&review.score.value) {
        return Err("the review score value must be from 1 to 10".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for issue in review.major_comments.iter().chain(&review.minor_comments) {
        if [
            &issue.id,
            &issue.section,
            &issue.issue,
            &issue.requested_change,
        ]
        .into_iter()
        .any(|t| empty(t))
        {
            return Err(format!(
                "review issue `{}` needs an id, section, issue and requested_change",
                issue.id
            ));
        }
        if !seen.insert(issue.id.clone()) {
            return Err(format!("duplicate review issue id: {}", issue.id));
        }
    }
    for issue in &review.major_comments {
        if issue.evidence.iter().all(|e| empty(e)) {
            return Err(format!(
                "major comment `{}` must include evidence",
                issue.id
            ));
        }
    }
    match review.status {
        Status::Pass if !review.major_comments.is_empty() => {
            Err("review status `pass` cannot include major_comments".into())
        }
        Status::Revise if review.major_comments.is_empty() => {
            Err("review status `revise` must include at least one major comment".into())
        }
        _ => Ok(review),
    }
}

fn unfence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim().strip_suffix("```").map_or(text, str::trim)
}

/// The review as feedback for the repairer.
#[must_use]
pub fn render_feedback(review: &Review) -> String {
    let mut out = format!(
        "# Paper review\n\n- status: `{}`\n- score: `{}: {}`\n\n## Summary\n{}\n",
        match review.status {
            Status::Pass => "pass",
            Status::Revise => "revise",
        },
        review.score.value,
        review.score.label,
        review.summary.trim()
    );
    for (title, issues) in [
        ("Major comments", &review.major_comments),
        ("Minor comments", &review.minor_comments),
    ] {
        out.push_str(&format!("\n## {title}\n"));
        if issues.is_empty() {
            out.push_str("- None.\n");
        }
        for (index, issue) in issues.iter().enumerate() {
            out.push_str(&format!(
                "\n### {}. {} - {}\n- issue: {}\n- requested_change: {}\n",
                index + 1,
                issue.id,
                issue.section,
                issue.issue,
                issue.requested_change
            ));
            for item in &issue.evidence {
                out.push_str(&format!("- evidence: {item}\n"));
            }
        }
    }
    out.push_str("\n## Questions\n");
    if review.questions.is_empty() {
        out.push_str("- None.\n");
    }
    for question in &review.questions {
        out.push_str(&format!("- {question}\n"));
    }
    out
}

/// Writes `paper_review.json` and `review_feedback.md` into `dir`.
///
/// # Errors
///
/// Returns the I/O error from writing either file.
pub fn write_record(dir: &Path, review: &Review) -> std::io::Result<()> {
    let value = serde_json::to_value(review).map_err(std::io::Error::other)?;
    super::files::write_json(&dir.join(REVIEW_FILE), &value)?;
    std::fs::write(dir.join(FEEDBACK_FILE), render_feedback(review))
}

/// The text a repair gets when the reviewer's reply could not be read.
pub const UNREADABLE: &str = "The reviewer's reply did not follow the output contract, so there is no structured feedback. \
     Re-read the paper and improve its clarity and grounding where you can, without new claims.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn review(status: &str, majors: &[&str]) -> String {
        json!({
            "status": status, "summary": "s", "strengths": ["a"],
            "major_comments": majors.iter().map(|id| json!({
                "id": id, "section": "Results", "issue": "i",
                "evidence": ["e"], "requested_change": "c"})).collect::<Vec<_>>(),
            "minor_comments": [], "questions": [],
            "score": {"value": 5, "label": "borderline"}
        })
        .to_string()
    }

    #[test]
    fn a_review_is_read_with_or_without_a_fence() {
        let text = review("revise", &["m1"]);
        let plain = parse_response(&text).unwrap();
        let fenced = parse_response(&format!("```json\n{text}\n```")).unwrap();
        assert_eq!(plain, fenced);
        assert_eq!(plain.major_comments.len(), 1);
    }

    #[test]
    fn each_contract_break_is_named() {
        let mut bad_score: serde_json::Value = serde_json::from_str(&review("pass", &[])).unwrap();
        bad_score["score"]["value"] = json!(11);
        let mut no_evidence: serde_json::Value =
            serde_json::from_str(&review("revise", &["m1"])).unwrap();
        no_evidence["major_comments"][0]["evidence"] = json!([]);
        let mut extra: serde_json::Value = serde_json::from_str(&review("pass", &[])).unwrap();
        extra["verdict"] = json!("x");
        for (reply, needle) in [
            ("not json".to_string(), "not the JSON object"),
            (review("pass", &["m1"]), "cannot include major_comments"),
            (review("revise", &[]), "at least one major comment"),
            (review("revise", &["m1", "m1"]), "duplicate review issue id"),
            (bad_score.to_string(), "from 1 to 10"),
            (no_evidence.to_string(), "must include evidence"),
            (extra.to_string(), "not the JSON object"),
        ] {
            let error = parse_response(&reply).unwrap_err();
            assert!(error.contains(needle), "{error} lacks {needle}");
        }
    }

    #[test]
    fn the_feedback_lists_every_comment() {
        let parsed = parse_response(&review("revise", &["m1"])).unwrap();
        let text = render_feedback(&parsed);
        assert!(text.contains("### 1. m1 - Results"));
        assert!(text.contains("requested_change: c"));
        assert!(text.contains("## Minor comments\n- None."));
        let dir = tempfile::tempdir().unwrap();
        write_record(dir.path(), &parsed).unwrap();
        assert!(dir.path().join(REVIEW_FILE).is_file());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FEEDBACK_FILE)).unwrap(),
            text
        );
    }
}
