//! Settings and mutable state of one paper phase.
//!
//! [`PaperPlan`] holds what is fixed when the graph is built: the venue, the
//! reference counts, the repair cap and the TeX programs. [`PaperState`] is a
//! local resource that the systems of the phase read and write while it runs.
//! It holds decisions in memory only: a restarted phase replays its steps, and
//! every finished model call is restored from the step journal, so the state
//! is rebuilt in the same order.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use polaris::system::resource::LocalResource;
use polaris_cosmos::flow::{FlowResult, RunHandle, StepKey};
use serde_json::Value;

use super::audit::Audit;
use super::decision::AuditResult;
use super::plan::Gaps;
use super::presentation::Review;
use crate::config::ScientistConfig;
use crate::latex::LatexTools;

/// Phase name used for spend, events and the journal.
pub const PHASE: &str = "paper";

/// Folder of the run that holds the paper.
pub const PAPER_DIR: &str = "paper";

/// Folder inside the paper folder that holds the staged evidence.
pub const DOSSIER_DIR: &str = "dossier";

/// Counter for repeated reference query plans.
pub const REFS_PLAN_COUNTER: &str = "paper.refs_plan_round";
/// Counter for repeated strategies.
pub const STRATEGY_COUNTER: &str = "paper.strategy_round";
/// Counter for repeated rankings.
pub const RANK_COUNTER: &str = "paper.rank_round";
/// Counter for repeated draft gap analyses.
pub const GAPS_COUNTER: &str = "paper.gaps_round";
/// Counter for repeated post-draft query plans.
pub const REFS_PLAN2_COUNTER: &str = "paper.refs_plan2_round";
/// Counter for repeated audit replies that broke the output contract.
pub const AUDIT_RETRY_COUNTER: &str = "paper.audit_retry";
/// Counter for repeated review replies that broke the output contract.
pub const REVIEW_RETRY_COUNTER: &str = "paper.review_retry";
/// Most times a reply-only role is asked again after a reply that broke the
/// contract.
pub const REPLY_RETRIES: u32 = 2;

/// Counters of the preparatory loops, each with the cap `paper.max_repair_rounds`.
pub const PREPARATION_COUNTERS: [&str; 5] = [
    REFS_PLAN_COUNTER,
    STRATEGY_COUNTER,
    RANK_COUNTER,
    GAPS_COUNTER,
    REFS_PLAN2_COUNTER,
];

/// What is fixed when the graph is built.
#[derive(Clone, Debug)]
pub struct PaperPlan {
    /// Prefix of every step key of the phase.
    pub prefix: StepKey,
    /// Venue name (`paper.venue`).
    pub venue: String,
    /// Whether gap analysis and enrichment run after the first draft.
    pub two_stage: bool,
    /// References the retrieval aims for.
    pub min_refs: usize,
    /// Most references retrieval keeps.
    pub max_refs: usize,
    /// Cap of the shared repair counter and of the preparatory counters.
    pub max_repair_rounds: u32,
    /// Planned presentation review rounds.
    pub review_rounds: u32,
    /// TeX programs and their time limit.
    pub tools: LatexTools,
    /// How the reference services are reached.
    pub refs: super::refs::Settings,
}

impl PaperPlan {
    /// Reads the settings of the phase from a resolved configuration.
    ///
    /// # Errors
    ///
    /// Returns a sentence when a setting asks for something the phase does not
    /// build: another prompt family than the standalone one, an unknown venue,
    /// or no reference.
    pub fn from_config(config: &ScientistConfig, prefix: StepKey) -> Result<Self, String> {
        if !config.bool("paper.standalone") {
            return Err(
                "paper.standalone=false is not built; only the standalone paper prompt exists"
                    .into(),
            );
        }
        let venue: String = config.string("paper.venue").to_string();
        if super::venue::Venue::named(&venue).is_none() {
            return Err(format!("paper.venue `{venue}` has no template"));
        }
        let min_refs = usize::try_from(config.u64("paper.min_refs")).unwrap_or(usize::MAX);
        let max_refs = usize::try_from(config.u64("paper.max_refs")).unwrap_or(usize::MAX);
        if min_refs == 0 || max_refs < min_refs {
            return Err("paper.min_refs must be positive and not above paper.max_refs".into());
        }
        Ok(Self {
            prefix,
            venue,
            two_stage: config.bool("paper.two_stage"),
            min_refs,
            max_refs,
            max_repair_rounds: u32::try_from(config.u64("paper.max_repair_rounds"))
                .unwrap_or(u32::MAX),
            review_rounds: u32::try_from(config.u64("paper.review_rounds")).unwrap_or(u32::MAX),
            tools: LatexTools::from_values(&config.values),
            refs: super::refs::Settings::from_environment(min_refs, max_refs),
        })
    }

    /// Step key of a step of the phase, such as `paper/refs.plan.a0`.
    ///
    /// # Errors
    ///
    /// Returns an error when the step makes an invalid key, such as a step made
    /// only of digits.
    pub fn key(&self, step: &str) -> FlowResult<StepKey> {
        self.prefix.child(step)
    }
}

/// The paper folder of a run.
#[must_use]
pub fn paper_dir(handle: &RunHandle) -> PathBuf {
    handle.root.join(PAPER_DIR)
}

/// An analysis the post-hoc phase accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analysis {
    /// Analysis id.
    pub id: String,
    /// Folder with the files of the analysis.
    pub dir: PathBuf,
}

/// The replies of one reply-only role in the current round.
#[derive(Debug)]
pub struct Slot<T> {
    /// The reply that followed the contract.
    pub parsed: Option<T>,
    /// Why the role could not give a reply at all.
    pub failure: Option<String>,
    /// What was wrong with the latest reply.
    pub last_error: Option<String>,
    /// Replies asked for so far.
    pub attempts: u32,
    /// Step key of the latest call.
    pub step: Option<StepKey>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            parsed: None,
            failure: None,
            last_error: None,
            attempts: 0,
            step: None,
        }
    }
}

/// Mutable state of the phase.
#[derive(Debug, Default)]
pub struct PaperState {
    /// Why the phase ended without a paper, once it has.
    pub ending: Option<String>,
    /// Caps the phase reached.
    pub caps_hit: BTreeSet<String>,
    /// Ids a claim or a figure may cite.
    pub source_ids: BTreeSet<String>,
    /// Accepted post-hoc analyses.
    pub analyses: Vec<Analysis>,
    /// Task goal, read from `task.md`, for the prompts.
    pub task_goal: String,
    /// Feedback for the next try of a preparatory step.
    pub retry_feedback: Option<String>,
    /// Plan record of the first reference round.
    pub plan_record: Value,
    /// Queries of the first plan.
    pub plan_queries: Vec<String>,
    /// Gaps found in the first draft.
    pub gaps: Option<Gaps>,
    /// True when the first draft left a manuscript behind.
    pub drafted: bool,
    /// What the writer reported when it did not finish.
    pub manuscript_note: Option<String>,
    /// Hard errors of the latest check.
    pub hard_errors: Vec<String>,
    /// Printed numbers the latest check found unbacked.
    pub unbacked_numbers: usize,
    /// Fingerprint of the latest failing check.
    pub failure: Option<String>,
    /// Fingerprint of the failing check before it.
    pub previous_failure: Option<String>,
    /// Input hashes taken before the audit or the review.
    pub reply_inputs: std::collections::BTreeMap<String, String>,
    /// The audit replies of the current round.
    pub audit_slot: Slot<Audit>,
    /// The audit of the latest round.
    pub audit: Option<AuditResult>,
    /// The review replies of the current round.
    pub review_slot: Slot<Review>,
    /// Feedback of a planned review that ran in this round.
    pub review: Option<String>,
    /// Planned review rounds that ran.
    pub review_rounds_done: u32,
    /// Queries of the post-draft plan.
    pub plan2_queries: Vec<String>,
    /// Plan record of the post-draft plan.
    pub plan2_record: Value,
    /// Feedback the repairer gets.
    pub repair_feedback: String,
    /// True while the latest decision asked for a repair.
    pub repair_pending: bool,
    /// Repairs started so far; names the step of the next repair.
    pub repairs_started: u32,
}

impl LocalResource for PaperState {}

impl PaperState {
    /// Records that the phase cannot produce a paper. The first reason stays.
    pub fn end(&mut self, reason: impl Into<String>) {
        if self.ending.is_none() {
            self.ending = Some(reason.into());
        }
    }

    /// True while the phase is still working toward a paper.
    #[must_use]
    pub fn live(&self) -> bool {
        self.ending.is_none()
    }
}

/// Reads a file of the paper folder for a prompt, or returns an empty string.
#[must_use]
pub fn read_or_empty(dir: &Path, name: &str) -> String {
    super::checks::read_regular(&dir.join(name)).unwrap_or_default()
}
