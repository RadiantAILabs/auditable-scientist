//! The decision after each paper check: accept the paper, ask for a repair, or
//! give up with an incomplete paper.
//!
//! The decision is a pure function of what the round found, so each rule can
//! be tested without running a graph. One repair counter pays for every kind
//! of repair; the loop that owns the counter stops the phase when it is used
//! up, which the caller reports as an incomplete paper.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::claims::AuditState;
use crate::phases::paper::audit::Audit;

/// Name of the counter that every repair of the paper draws on.
pub const REPAIR_COUNTER: &str = "paper.repair_round";

/// What the audit step produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditResult {
    /// The reply followed the contract.
    Parsed(Audit),
    /// The reply broke the contract on every allowed attempt.
    Degraded {
        /// What was wrong with the last reply.
        reason: String,
        /// Replies the auditor was given the chance to send.
        attempts: u32,
    },
    /// The audit could not run or could not be trusted: no call result, a
    /// refused budget, or inputs that changed while the auditor read them.
    Failed(String),
    /// The audit was not started because the paper has no manuscript yet; the
    /// repair that the missing manuscript causes comes first.
    NotRun,
}

impl AuditResult {
    /// The state the manifest records, or `None` when the audit did not finish.
    #[must_use]
    pub fn state(&self) -> Option<AuditState> {
        match self {
            Self::Parsed(audit) => Some(audit.state()),
            Self::Degraded { .. } => Some(AuditState::AuditDegraded),
            Self::Failed(_) | Self::NotRun => None,
        }
    }
}

/// Everything the decision looks at.
#[derive(Clone, Debug)]
pub struct Round<'a> {
    /// Hard errors of the paper check.
    pub hard_errors: &'a [String],
    /// What the audit produced.
    pub audit: &'a AuditResult,
    /// Reviewer feedback when a planned review ran in this round.
    pub review: Option<&'a str>,
    /// Fingerprint of the previous round's validation failure, if it failed.
    pub previous_failure: Option<&'a str>,
    /// Fingerprint of the failure in this round, from [`fingerprint`].
    pub failure: Option<&'a str>,
}

/// What to do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Accept the paper; the audit state goes into the manifest.
    Pass(AuditState),
    /// Run the repairer with this feedback.
    Repair(String),
    /// Stop with an incomplete paper.
    Incomplete(String),
}

/// Decides what follows one round of checking.
///
/// A real audit finding, a validation error and a planned review each call for
/// a repair. An audit whose reply could not be read is recorded as degraded
/// and does not gate the paper. An audit that could not run ends the phase,
/// and so does the same validation failure on a paper that did not change.
#[must_use]
pub fn decide(round: &Round<'_>) -> Decision {
    if let AuditResult::Failed(reason) = round.audit {
        return Decision::Incomplete(format!("the evidence audit could not run: {reason}"));
    }
    if matches!(round.audit, AuditResult::NotRun) && round.hard_errors.is_empty() {
        return Decision::Incomplete("the evidence audit did not run".to_string());
    }
    let mut feedback = Vec::new();
    if !round.hard_errors.is_empty() {
        if round.failure.is_some() && round.failure == round.previous_failure {
            return Decision::Incomplete(format!(
                "the same validation failure repeated after a repair that changed nothing: {}",
                round.hard_errors[0]
            ));
        }
        feedback.push(format!(
            "Validation errors to fix:\n{}",
            round
                .hard_errors
                .iter()
                .map(|e| format!("- {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if let AuditResult::Parsed(audit) = round.audit
        && !audit.findings.is_empty()
    {
        let lines: Vec<String> = audit
            .findings
            .iter()
            .map(|f| {
                format!(
                    "- {} ({}): {} Evidence: {} Fix: {}",
                    f.id, f.section, f.issue, f.evidence, f.fix
                )
            })
            .collect();
        feedback.push(format!(
            "Evidence audit findings to fix:\n{}",
            lines.join("\n")
        ));
    }
    if let Some(review) = round.review {
        feedback.push(format!("Reviewer feedback to apply:\n{}", review.trim()));
    }
    if feedback.is_empty() {
        return Decision::Pass(round.audit.state().unwrap_or(AuditState::Missing));
    }
    Decision::Repair(feedback.join("\n\n"))
}

/// Fingerprint of a failing round: the hard errors and the hash of every paper
/// input. It repeats only when the same errors stand on an unchanged paper.
#[must_use]
pub fn fingerprint(hard_errors: &[String], inputs: &BTreeMap<String, String>) -> Option<String> {
    if hard_errors.is_empty() {
        return None;
    }
    let mut errors: Vec<&str> = hard_errors.iter().map(String::as_str).collect();
    errors.sort_unstable();
    errors.dedup();
    let mut hasher = Sha256::new();
    for error in errors {
        hasher.update(error.as_bytes());
        hasher.update([0]);
    }
    hasher.update([1]);
    for (name, hash) in inputs {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(hash.as_bytes());
        hasher.update([0]);
    }
    Some(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phases::paper::audit::{Finding, Verdict};

    fn errors(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    fn finding() -> Finding {
        Finding {
            id: "F1".into(),
            section: "Results".into(),
            claim: "c".into(),
            issue: "the margin is within noise".into(),
            evidence: "paper_data.json#a".into(),
            fix: "soften the claim".into(),
        }
    }

    fn passed() -> AuditResult {
        AuditResult::Parsed(Audit {
            verdict: Verdict::Pass,
            summary: "ok".into(),
            findings: Vec::new(),
        })
    }

    fn failed() -> AuditResult {
        AuditResult::Parsed(Audit {
            verdict: Verdict::Fail,
            summary: "bad".into(),
            findings: vec![finding()],
        })
    }

    fn round<'a>(hard: &'a [String], audit: &'a AuditResult) -> Round<'a> {
        Round {
            hard_errors: hard,
            audit,
            review: None,
            previous_failure: None,
            failure: None,
        }
    }

    #[test]
    fn a_missing_manuscript_is_repaired_before_any_audit_and_never_passes_unaudited() {
        let hard = errors(&["paper.tex is missing"]);
        let Decision::Repair(text) = decide(&round(&hard, &AuditResult::NotRun)) else {
            panic!("the missing manuscript must be repaired");
        };
        assert!(text.contains("paper.tex is missing"));
        assert!(matches!(
            decide(&round(&[], &AuditResult::NotRun)),
            Decision::Incomplete(_)
        ));
    }

    #[test]
    fn clean_validation_and_audit_pass() {
        assert_eq!(
            decide(&round(&[], &passed())),
            Decision::Pass(AuditState::Passed)
        );
    }

    #[test]
    fn an_audit_finding_calls_for_a_repair_with_its_fix() {
        let audit = failed();
        let Decision::Repair(text) = decide(&round(&[], &audit)) else {
            panic!("a finding must gate");
        };
        assert!(
            text.contains("F1") && text.contains("soften the claim"),
            "{text}"
        );
    }

    #[test]
    fn a_validation_error_calls_for_a_repair() {
        let hard = errors(&["unknown claim id c9"]);
        let audit = passed();
        let Decision::Repair(text) = decide(&round(&hard, &audit)) else {
            panic!("an error must gate");
        };
        assert!(text.contains("unknown claim id c9"));
    }

    #[test]
    fn errors_and_findings_share_one_repair() {
        let hard = errors(&["missing figure"]);
        let audit = failed();
        let Decision::Repair(text) = decide(&round(&hard, &audit)) else {
            panic!("must repair");
        };
        assert!(text.contains("missing figure") && text.contains("F1"));
    }

    #[test]
    fn an_unreadable_audit_does_not_gate_and_is_never_a_pass() {
        let audit = AuditResult::Degraded {
            reason: "no verdict line".into(),
            attempts: 3,
        };
        assert_eq!(
            decide(&round(&[], &audit)),
            Decision::Pass(AuditState::AuditDegraded)
        );
        let hard = errors(&["bad"]);
        assert!(matches!(decide(&round(&hard, &audit)), Decision::Repair(_)));
    }

    #[test]
    fn an_audit_that_could_not_run_ends_the_phase() {
        let audit = AuditResult::Failed("budget refused".into());
        let decision = decide(&round(&[], &audit));
        let Decision::Incomplete(reason) = decision else {
            panic!("operational failure must end");
        };
        assert!(reason.contains("budget refused"));
    }

    #[test]
    fn the_same_failure_on_an_unchanged_paper_ends_the_phase() {
        let hard = errors(&["unresolved citation key k1"]);
        let inputs = BTreeMap::from([("paper.tex".to_string(), "aa".to_string())]);
        let print = fingerprint(&hard, &inputs).expect("fingerprint");
        let audit = passed();
        let mut again = round(&hard, &audit);
        again.failure = Some(&print);
        again.previous_failure = Some(&print);
        assert!(matches!(decide(&again), Decision::Incomplete(_)));

        let changed = BTreeMap::from([("paper.tex".to_string(), "bb".to_string())]);
        let other = fingerprint(&hard, &changed).expect("fingerprint");
        again.failure = Some(&other);
        assert!(matches!(decide(&again), Decision::Repair(_)));
    }

    #[test]
    fn fingerprints_ignore_error_order_and_need_an_error() {
        let inputs = BTreeMap::new();
        assert_eq!(
            fingerprint(&errors(&["a", "b"]), &inputs),
            fingerprint(&errors(&["b", "a", "a"]), &inputs)
        );
        assert_eq!(fingerprint(&[], &inputs), None);
    }

    #[test]
    fn a_planned_review_calls_for_a_repair_even_when_the_paper_is_clean() {
        let audit = passed();
        let mut with_review = round(&[], &audit);
        with_review.review = Some("The abstract is vague.");
        let Decision::Repair(text) = decide(&with_review) else {
            panic!("a review costs a repair");
        };
        assert!(text.contains("The abstract is vague."));
    }

    #[test]
    fn the_counter_has_one_shared_name() {
        assert_eq!(REPAIR_COUNTER, "paper.repair_round");
    }
}
