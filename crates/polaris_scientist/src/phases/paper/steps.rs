//! What the paper phase does after each role call and after each check.
//!
//! The functions here hold the phase's decisions as plain code over
//! [`PaperState`], so the graph in `graph.rs` only wires them together. A
//! check that follows a role call reads the call's outcome and the files the
//! role left in its workspace, promotes what is acceptable into the paper
//! folder, and answers with a [`Verdict`]. A retried step answers `Fail`
//! with the sentence that goes back to the role.

use std::path::{Path, PathBuf};

use polaris_cosmos::flow::{
    AgentCall, AgentFailure, AgentOutcome, AgentReport, RepairCounters, SkipReason, StopCause,
    Verdict,
};

use super::audit;
use super::checks::{self, CheckSettings};
use super::decision::{self, AuditResult, Decision, REPAIR_COUNTER, Round};
use super::figures::{self, FigureInput};
use super::files;
use super::manifest::{self, Outcome, Status};
use super::plan;
use super::presentation;
use super::refs;
use super::stage;
use super::state::{PaperPlan, PaperState, Slot};
use crate::claims::AuditState;

/// Cap name recorded when a role call is refused its budget.
pub const BUDGET_CAP: &str = "budget.paper";

/// What a check reads and may change.
pub struct Cx<'a> {
    /// Settings fixed at build time.
    pub plan: &'a PaperPlan,
    /// The phase's state.
    pub state: &'a mut PaperState,
    /// The call that was just made.
    pub call: &'a AgentCall,
    /// What the call returned.
    pub outcome: &'a AgentOutcome,
    /// Root of the run.
    pub root: &'a Path,
    /// The paper folder.
    pub paper_dir: &'a Path,
}

fn describe(failure: &AgentFailure) -> String {
    match failure {
        AgentFailure::Setup(m) => format!("the session could not be set up: {m}"),
        AgentFailure::Unmeasured(m) => format!("the call could not be measured: {m}"),
        AgentFailure::Internal(m) => format!("the call failed inside the host: {m}"),
        other => format!("the call failed: {other}"),
    }
}

fn skip_text(reason: &SkipReason) -> &'static str {
    match reason {
        SkipReason::Budget => "its budget was refused",
        SkipReason::Disabled => "the phase was stopped",
        SkipReason::NotApplicable => "it does not apply",
        _ => "it was skipped",
    }
}

impl<'a> Cx<'a> {
    /// The `outputs/` folder of the call, when the call got as far as having one.
    #[must_use]
    pub fn outputs(&self) -> Option<PathBuf> {
        files::outputs_of(self.root, &self.call.step)
    }

    /// Looks at how the call ended. Returns its report when the call ran and
    /// the phase may go on. A call that did not run ends the phase when the
    /// step is `required`; an interrupted call always does.
    pub fn gate(&mut self, what: &str, required: bool) -> Option<&'a AgentReport> {
        let outcome: &'a AgentOutcome = self.outcome;
        match outcome {
            AgentOutcome::Ran { report, .. } => {
                if report.stop == StopCause::Interrupted {
                    self.state.end("interrupted");
                    return None;
                }
                Some(report)
            }
            AgentOutcome::Failed(failure) => {
                tracing::warn!(step = %self.call.step, "{what}: {}", describe(failure));
                if required {
                    self.state
                        .end(format!("{what} could not run: {}", describe(failure)));
                }
                None
            }
            AgentOutcome::Skipped(reason) => {
                if *reason == SkipReason::Budget {
                    self.state.caps_hit.insert(BUDGET_CAP.to_string());
                }
                tracing::info!(step = %self.call.step, "{what} did not run: {}", skip_text(reason));
                if required {
                    self.state
                        .end(format!("{what} did not run: {}", skip_text(reason)));
                }
                None
            }
            _ => {
                if required {
                    self.state.end(format!("{what} did not run"));
                }
                None
            }
        }
    }

    /// Answers a retried step: the output was not acceptable.
    fn invalid(&mut self, message: impl Into<String>) -> Verdict {
        let message = message.into();
        self.state.retry_feedback = Some(message.clone());
        Verdict::Fail(message)
    }
}

/// Check after `paper.refs.plan`.
pub fn reference_plan(cx: &mut Cx<'_>) -> Verdict {
    let Some(_) = cx.gate("the reference planner", true) else {
        return Verdict::Pass;
    };
    let Some(dir) = cx.outputs() else {
        return cx.invalid("the reference planner left no outputs folder");
    };
    let files = plan::REFERENCE_PLAN;
    match plan::check_reference_plan(&dir, files, plan::MAX_QUERIES) {
        Ok(found) => match files::promote_named(&dir, cx.paper_dir, &[files.json, files.memo]) {
            Ok(_) => {
                cx.state.plan_queries = found.queries;
                cx.state.plan_record = found.record;
                cx.state.retry_feedback = None;
                Verdict::Pass
            }
            Err(e) => cx.invalid(e),
        },
        Err(e) => cx.invalid(e),
    }
}

/// Check after `paper.strategy`.
pub fn strategy(cx: &mut Cx<'_>) -> Verdict {
    let Some(_) = cx.gate("the strategist", true) else {
        return Verdict::Pass;
    };
    let Some(dir) = cx.outputs() else {
        return cx.invalid("the strategist left no outputs folder");
    };
    let files = plan::STRATEGY;
    match plan::check_strategy(&dir, &cx.state.source_ids) {
        Ok(_) => match files::promote_named(&dir, cx.paper_dir, &[files.json, files.memo]) {
            Ok(_) => {
                cx.state.retry_feedback = None;
                Verdict::Pass
            }
            Err(e) => cx.invalid(e),
        },
        Err(e) => cx.invalid(e),
    }
}

/// Check after `paper.rank`.
pub fn ranking(cx: &mut Cx<'_>) -> Verdict {
    let Some(_) = cx.gate("the post-hoc ranker", false) else {
        return Verdict::Pass;
    };
    let Some(dir) = cx.outputs() else {
        return cx.invalid("the ranker left no outputs folder");
    };
    let ids: Vec<String> = cx.state.analyses.iter().map(|a| a.id.clone()).collect();
    match plan::check_ranking(&dir, &ids) {
        Ok(ranked) => {
            let applied = files::promote_named(&dir, cx.paper_dir, &[plan::RANKING_FILE])
                .and_then(|_| stage::apply_ranking(cx.paper_dir, &ranked));
            match applied {
                Ok(()) => {
                    cx.state.retry_feedback = None;
                    Verdict::Pass
                }
                Err(e) => cx.invalid(e),
            }
        }
        Err(e) => cx.invalid(e),
    }
}

/// Check after `paper.draft`.
pub fn draft(cx: &mut Cx<'_>) -> Verdict {
    match cx.gate("the paper writer", true) {
        None => {
            if let AgentOutcome::Failed(failure) = cx.outcome {
                cx.state.manuscript_note = Some(describe(failure));
            }
        }
        Some(report) => {
            if report.stop != StopCause::Finished {
                cx.state.manuscript_note = Some(format!(
                    "the writer stopped before it finished ({:?})",
                    report.stop
                ));
            }
            if let Some(dir) = cx.outputs()
                && let Err(e) = files::promote_manuscript(&dir, cx.paper_dir)
            {
                cx.state.manuscript_note = Some(e);
            }
        }
    }
    cx.state.drafted = cx.paper_dir.join("paper.tex").is_file();
    Verdict::Pass
}

/// Check after `paper.gaps`.
pub fn gaps(cx: &mut Cx<'_>) -> Verdict {
    let Some(_) = cx.gate("the citation gap analyst", false) else {
        return Verdict::Pass;
    };
    let Some(dir) = cx.outputs() else {
        return cx.invalid("the gap analyst left no outputs folder");
    };
    let files = plan::GAPS;
    match plan::check_gaps(&dir) {
        Ok(found) => match files::promote_named(&dir, cx.paper_dir, &[files.json, files.memo]) {
            Ok(_) => {
                cx.state.gaps = Some(found);
                cx.state.retry_feedback = None;
                Verdict::Pass
            }
            Err(e) => cx.invalid(e),
        },
        Err(e) => cx.invalid(e),
    }
}

/// Check after `paper.refs.plan2`.
pub fn postdraft_plan(cx: &mut Cx<'_>) -> Verdict {
    let Some(_) = cx.gate("the post-draft reference planner", false) else {
        return Verdict::Pass;
    };
    let Some(dir) = cx.outputs() else {
        return cx.invalid("the post-draft planner left no outputs folder");
    };
    let files = plan::POSTDRAFT_PLAN;
    match plan::check_reference_plan(&dir, files, plan::MAX_QUERIES) {
        Ok(found) => match files::promote_named(&dir, cx.paper_dir, &[files.json, files.memo]) {
            Ok(_) => {
                cx.state.plan2_queries = found.queries;
                cx.state.plan2_record = found.record;
                cx.state.retry_feedback = None;
                Verdict::Pass
            }
            Err(e) => cx.invalid(e),
        },
        Err(e) => cx.invalid(e),
    }
}

/// Check after `paper.enrich`.
pub fn enrich(cx: &mut Cx<'_>) -> Verdict {
    if cx.gate("the paper enricher", false).is_some()
        && let Some(dir) = cx.outputs()
        && dir.join("paper.tex").is_file()
        && let Err(e) = files::promote_manuscript(&dir, cx.paper_dir)
    {
        tracing::warn!("the enriched manuscript was not promoted: {e}");
    }
    Verdict::Pass
}

/// Check after `paper.repair`.
pub fn repair(cx: &mut Cx<'_>) -> Verdict {
    cx.state.repair_pending = false;
    if cx.gate("the paper repairer", true).is_some()
        && let Some(dir) = cx.outputs()
        && let Err(e) = files::promote_manuscript(&dir, cx.paper_dir)
    {
        cx.state.manuscript_note = Some(e);
    }
    cx.state.drafted = cx.paper_dir.join("paper.tex").is_file();
    Verdict::Pass
}

fn reply_failure<T>(slot: &mut Slot<T>, text: String) -> Verdict {
    slot.failure = Some(text);
    Verdict::Pass
}

/// Check after one try of `paper.audit`.
pub fn audit(cx: &mut Cx<'_>) -> Verdict {
    let slot = &mut cx.state.audit_slot;
    slot.step = Some(cx.call.step.clone());
    slot.attempts += 1;
    let outcome: &AgentOutcome = cx.outcome;
    match outcome {
        AgentOutcome::Ran { report, .. } => {
            if report.stop == StopCause::Interrupted {
                cx.state.end("interrupted");
                return reply_failure(&mut cx.state.audit_slot, "interrupted".into());
            }
            if let StopCause::ModelError(message) = &report.stop {
                return reply_failure(slot, format!("the auditor's model call failed: {message}"));
            }
            match audit::parse_response(&report.final_text) {
                Ok(parsed) => {
                    slot.parsed = Some(parsed);
                    Verdict::Pass
                }
                Err(e) => {
                    slot.last_error = Some(e.0.clone());
                    Verdict::Fail(e.0)
                }
            }
        }
        AgentOutcome::Failed(failure) => reply_failure(slot, describe(failure)),
        AgentOutcome::Skipped(reason) => {
            if *reason == SkipReason::Budget {
                cx.state.caps_hit.insert(BUDGET_CAP.to_string());
            }
            let text = format!("the auditor did not run: {}", skip_text(reason));
            reply_failure(&mut cx.state.audit_slot, text)
        }
        _ => reply_failure(slot, "the auditor did not run".into()),
    }
}

/// Check after one try of `paper.review`.
pub fn review(cx: &mut Cx<'_>) -> Verdict {
    let slot = &mut cx.state.review_slot;
    slot.step = Some(cx.call.step.clone());
    slot.attempts += 1;
    let outcome: &AgentOutcome = cx.outcome;
    match outcome {
        AgentOutcome::Ran { report, .. } => {
            if report.stop == StopCause::Interrupted {
                cx.state.end("interrupted");
                return reply_failure(&mut cx.state.review_slot, "interrupted".into());
            }
            if let StopCause::ModelError(message) = &report.stop {
                return reply_failure(slot, format!("the reviewer's model call failed: {message}"));
            }
            match presentation::parse_response(&report.final_text) {
                Ok(parsed) => {
                    slot.parsed = Some(parsed);
                    Verdict::Pass
                }
                Err(e) => {
                    slot.last_error = Some(e.clone());
                    Verdict::Fail(e)
                }
            }
        }
        AgentOutcome::Failed(failure) => reply_failure(slot, describe(failure)),
        AgentOutcome::Skipped(reason) => {
            if *reason == SkipReason::Budget {
                cx.state.caps_hit.insert(BUDGET_CAP.to_string());
            }
            let text = format!("the reviewer did not run: {}", skip_text(reason));
            reply_failure(&mut cx.state.review_slot, text)
        }
        _ => reply_failure(slot, "the reviewer did not run".into()),
    }
}

/// The sentence a reply-only role gets when its previous reply was refused.
#[must_use]
pub fn contract_feedback(error: Option<&String>) -> Option<String> {
    error.map(|e| {
        format!("Your previous reply broke the output contract: {e} Reply again and follow the contract exactly.")
    })
}

/// What the paper check found. It is kept apart from the state because the
/// check awaits the TeX build.
#[derive(Clone, Debug)]
pub struct CheckResult {
    /// Hard errors of the check.
    pub hard_errors: Vec<String>,
    /// Printed numbers that no recorded value or derivation backs.
    pub unbacked_numbers: usize,
    /// Fingerprint of the failure, when there is one.
    pub failure: Option<String>,
}

/// Runs the paper check on the paper folder and writes `validation_report.json`.
pub async fn check_round(plan: &PaperPlan, paper_dir: &Path) -> CheckResult {
    let settings = CheckSettings {
        venue: plan.venue.clone(),
        tools: plan.tools.clone(),
        compile: true,
    };
    let report = checks::validate_and_write(paper_dir, &settings).await;
    let hashes = audit::input_hashes(paper_dir).unwrap_or_default();
    CheckResult {
        failure: decision::fingerprint(&report.hard_errors, &hashes),
        unbacked_numbers: report.unbacked_numbers.len(),
        hard_errors: report.hard_errors,
    }
}

/// Takes a check into the state and clears the replies of the round before.
pub fn record_check(state: &mut PaperState, result: &CheckResult) {
    state.audit = None;
    state.review = None;
    state.audit_slot = Slot::default();
    state.review_slot = Slot::default();
    state.hard_errors.clone_from(&result.hard_errors);
    state.unbacked_numbers = result.unbacked_numbers;
    state.previous_failure = state.failure.take();
    state.failure.clone_from(&result.failure);
}

/// Hashes the paper inputs before a reply-only role reads them.
pub fn begin_reply(state: &mut PaperState, paper_dir: &Path) {
    state.reply_inputs = audit::input_hashes(paper_dir).unwrap_or_default();
}

/// True when a planned review round is next: rounds are left and the audit
/// found nothing to repair.
#[must_use]
pub fn review_is_due(plan: &PaperPlan, state: &PaperState) -> bool {
    state.review_rounds_done < plan.review_rounds
        && match &state.audit {
            Some(AuditResult::Parsed(audit)) => audit.findings.is_empty(),
            Some(AuditResult::Degraded { .. }) => true,
            _ => false,
        }
}

/// The early look at the figure contract. The paper check repeats it and is
/// the one that gates; this only reports.
pub fn early_figure_look(state: &PaperState, paper_dir: &Path) {
    let Ok(tex) = checks::read_regular(&paper_dir.join("paper.tex")) else {
        return;
    };
    let section = figures::check(&FigureInput {
        dir: paper_dir,
        tex: &tex,
        source_ids: &state.source_ids,
    });
    tracing::info!(
        errors = section.section.hard_errors.len(),
        warnings = section.section.soft_warnings.len(),
        "figure contract checked before the paper check"
    );
}

/// Records the audit of this round after the last try. Returns the problem
/// when the record cannot be written.
pub fn record_audit(state: &mut PaperState, root: &Path, paper_dir: &Path) {
    let slot = std::mem::take(&mut state.audit_slot);
    let changed = reply_inputs_changed(state, root, paper_dir, slot.step.as_ref());
    let after = audit::input_hashes(paper_dir).unwrap_or_default();
    let result = if let Some(names) = changed {
        AuditResult::Failed(format!("the paper changed while it was audited: {names}"))
    } else if let Some(parsed) = slot.parsed {
        match audit::write_record(paper_dir, &parsed, &after) {
            Ok(()) => AuditResult::Parsed(parsed),
            Err(e) => AuditResult::Failed(format!("the audit record could not be written: {e}")),
        }
    } else if let Some(failure) = slot.failure {
        AuditResult::Failed(failure)
    } else {
        let reason = slot
            .last_error
            .unwrap_or_else(|| "the reply could not be read".to_string());
        match audit::write_degraded(paper_dir, &reason, slot.attempts, &after) {
            Ok(()) => AuditResult::Degraded {
                reason,
                attempts: slot.attempts,
            },
            Err(e) => AuditResult::Failed(format!("the audit record could not be written: {e}")),
        }
    };
    state.audit = Some(result);
}

/// Records the review of this round after the last try.
pub fn record_review(state: &mut PaperState, root: &Path, paper_dir: &Path) {
    let slot = std::mem::take(&mut state.review_slot);
    let changed = reply_inputs_changed(state, root, paper_dir, slot.step.as_ref());
    if let Some(names) = changed {
        state.end(format!("the paper changed while it was reviewed: {names}"));
        return;
    }
    let text = if let Some(parsed) = slot.parsed {
        match presentation::write_record(paper_dir, &parsed) {
            Ok(()) => presentation::render_feedback(&parsed),
            Err(e) => {
                state.end(format!("the review record could not be written: {e}"));
                return;
            }
        }
    } else if let Some(failure) = slot.failure {
        if state.caps_hit.contains(BUDGET_CAP) && failure.contains("did not run") {
            // A refused budget skips an optional review without a repair.
            return;
        }
        state.end(format!("the review could not run: {failure}"));
        return;
    } else {
        presentation::UNREADABLE.to_string()
    };
    state.review_rounds_done += 1;
    state.review = Some(text);
}

/// Names of the files that changed since the reply roles were started, from
/// the paper folder and from the read-only copy the role was given.
fn reply_inputs_changed(
    state: &PaperState,
    root: &Path,
    paper_dir: &Path,
    step: Option<&polaris_cosmos::flow::StepKey>,
) -> Option<String> {
    let mut changed = audit::changed_files(
        &state.reply_inputs,
        &audit::input_hashes(paper_dir).unwrap_or_default(),
    );
    if let Some(snapshot) = step
        .and_then(|s| files::workspace_of(root, s))
        .map(|w| w.join("inputs/paper"))
        .filter(|p| p.is_dir())
    {
        changed.extend(audit::changed_files(
            &state.reply_inputs,
            &audit::input_hashes(&snapshot).unwrap_or_default(),
        ));
    }
    changed.sort();
    changed.dedup();
    (!changed.is_empty()).then(|| changed.join(", "))
}

/// The decision after a round of checks. Writes the round archive and, for a
/// repair, the feedback files. Answers `Fail` when a repair is wanted.
pub fn decide(state: &mut PaperState, counters: &RepairCounters, paper_dir: &Path) -> Verdict {
    let audit = state.audit.clone().unwrap_or(AuditResult::NotRun);
    let round = Round {
        hard_errors: &state.hard_errors,
        audit: &audit,
        review: state.review.as_deref(),
        previous_failure: state.previous_failure.as_deref(),
        failure: state.failure.as_deref(),
    };
    let decision = decision::decide(&round);
    let number = counters.used(REPAIR_COUNTER) + 1;
    match decision {
        Decision::Pass(_) => {
            state.repair_pending = false;
            archive(paper_dir, number);
            Verdict::Pass
        }
        Decision::Incomplete(reason) => {
            state.repair_pending = false;
            state.end(reason);
            archive(paper_dir, number);
            Verdict::Pass
        }
        Decision::Repair(mut feedback) => {
            if let Some(note) = state.manuscript_note.take() {
                feedback.push_str(&format!(
                    "\n\nThe previous manuscript step reported: {note}"
                ));
            }
            state.repair_feedback = feedback.clone();
            state.repair_pending = true;
            let _ = std::fs::write(paper_dir.join("repair_feedback.md"), &feedback);
            archive(paper_dir, number);
            Verdict::Fail(feedback)
        }
    }
}

fn archive(paper_dir: &Path, number: u32) {
    if let Err(e) = manifest::archive_round(paper_dir, number) {
        tracing::warn!("round {number} was not archived: {e}");
    }
}

/// How the phase ended, as the manifest and the phase status report it.
pub struct Ending {
    /// True when the paper passed.
    pub complete: bool,
    /// Why not, when it did not.
    pub reason: Option<String>,
}

/// Decides whether the phase ended with a paper. A repair that is still
/// pending when the loop stops means the repair counter is used up.
pub fn conclude(state: &mut PaperState, counters: &RepairCounters) -> Ending {
    if state.live() && state.repair_pending {
        state.caps_hit.insert(REPAIR_COUNTER.to_string());
        let first = state
            .hard_errors
            .first()
            .cloned()
            .unwrap_or_else(|| "the audit found problems".to_string());
        state.end(format!(
            "the repair counter {REPAIR_COUNTER} is used up after {} repairs; the last check still found: {first}",
            counters.used(REPAIR_COUNTER)
        ));
    }
    if state.ending.is_some() {
        return Ending {
            complete: false,
            reason: state.ending.clone(),
        };
    }
    Ending {
        complete: true,
        reason: None,
    }
}

fn audit_state_of(state: &PaperState) -> (Option<AuditState>, Option<String>) {
    match &state.audit {
        Some(AuditResult::Degraded { reason, .. }) => {
            (Some(AuditState::AuditDegraded), Some(reason.clone()))
        }
        Some(result) => (result.state(), None),
        None => (None, None),
    }
}

/// Writes the manifest and the claims check of a finished paper. Returns the
/// reason when the paper cannot be finalized, for instance without a PDF.
pub fn finalize(
    plan: &PaperPlan,
    state: &PaperState,
    counters: &RepairCounters,
    paper_dir: &Path,
) -> Result<(), String> {
    let (audit_state, audit_note) = audit_state_of(state);
    let outcome = Outcome {
        status: Status::Complete,
        reason: None,
        venue: plan.venue.clone(),
        repair_rounds_used: counters.used(REPAIR_COUNTER),
        repair_round_limit: counters.limit(REPAIR_COUNTER).unwrap_or(0),
        audit_state,
        audit_note,
        caps_hit: state.caps_hit.clone(),
        review_rounds_done: state.review_rounds_done,
        hard_errors: state.hard_errors.clone(),
        unbacked_numbers: state.unbacked_numbers,
    };
    manifest::write(paper_dir, &outcome).map_err(|e| e.to_string())?;
    match crate::claims::check_paper(paper_dir) {
        Ok(check) => {
            if let Err(e) =
                crate::claims::write_report(&check, &paper_dir.join(crate::claims::REPORT_FILE))
            {
                tracing::warn!("claims_check.json was not written: {e}");
            }
        }
        Err(e) => tracing::warn!("the claims check could not run: {e}"),
    }
    Ok(())
}

/// Writes the manifest of a phase that ended without a paper.
pub fn record_incomplete(
    plan: &PaperPlan,
    state: &PaperState,
    counters: &RepairCounters,
    paper_dir: &Path,
    reason: &str,
) {
    let (audit_state, audit_note) = audit_state_of(state);
    let outcome = Outcome {
        status: Status::Incomplete,
        reason: Some(reason.to_string()),
        venue: plan.venue.clone(),
        repair_rounds_used: counters.used(REPAIR_COUNTER),
        repair_round_limit: counters.limit(REPAIR_COUNTER).unwrap_or(0),
        audit_state,
        audit_note,
        caps_hit: state.caps_hit.clone(),
        review_rounds_done: state.review_rounds_done,
        hard_errors: state.hard_errors.clone(),
        unbacked_numbers: state.unbacked_numbers,
    };
    if let Err(e) = std::fs::create_dir_all(paper_dir) {
        tracing::warn!("the paper folder could not be created: {e}");
    }
    if let Err(e) = manifest::write(paper_dir, &outcome) {
        tracing::warn!("the manifest of the incomplete paper was not written: {e}");
    }
}

/// Stores a retrieval round in the paper folder and notes a short result.
pub fn store_references(
    plan: &PaperPlan,
    state: &PaperState,
    paper_dir: &Path,
    retrieval: &refs::Retrieval,
    second_round: bool,
) -> Result<(), String> {
    let record = if second_round {
        &state.plan2_record
    } else {
        &state.plan_record
    };
    let metadata = refs::plan_metadata(record);
    let stored = if second_round {
        refs::store_merged(paper_dir, retrieval, &metadata)
    } else {
        refs::store(paper_dir, retrieval, &metadata)
    }
    .map_err(|e| format!("the references could not be written: {e}"))?;
    if !retrieval.is_complete(plan.min_refs) {
        tracing::warn!(
            found = retrieval.entries.len(),
            wanted = plan.min_refs,
            total = stored.count,
            "reference retrieval is short"
        );
    }
    Ok(())
}

/// Audit result for a paper that has no manuscript.
pub fn no_manuscript(state: &mut PaperState) {
    state.audit = Some(AuditResult::NotRun);
}
