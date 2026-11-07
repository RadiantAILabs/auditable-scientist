//! The paper phase: from the results of a campaign to a compiled, checked
//! paper.
//!
//! The phase stages the evidence, plans and retrieves references, lets a
//! strategist choose the story, a writer draft the paper, and then loops:
//! deterministic checks, an independent evidence audit, an optional review,
//! and a repair paid from one shared counter. [`graph`] holds the topology;
//! [`steps`] the decisions after each step; the other modules the rules those
//! decisions apply.
//!
//! What the phase leaves in `<run>/paper/`: `paper_data.json`,
//! `references.bib`, `references_provenance.json`, `claims_grounding.json`,
//! `paper.tex`, `paper.pdf`, `figure_manifest.json`, `validation_report.json`,
//! `paper_evidence_audit.json` and `.md`, `paper_manifest.json`,
//! `claims_check.json`, `rounds/NN/` and `figures/`.

use crate::latex::text;
use crate::phases::{BuildError, PhaseCtx, PhaseRegistry, RegisterError};
use polaris::graph::Graph;

pub mod audit;
pub mod calls;
pub mod checks;
pub mod decision;
pub mod figures;
pub mod files;
pub mod graph;
pub mod manifest;
pub mod plan;
pub mod presentation;
pub mod refs;
pub mod report;
pub mod stage;
pub mod state;
pub mod steps;
pub mod venue;

pub use state::PaperPlan;

/// The TeX source as the compiler would read it: comments, false conditional
/// branches and verbatim text removed.
pub(crate) fn masked(tex: &str) -> String {
    text::mask_verbatim(&text::strip_false_conditionals(&text::strip_comments(tex)))
}

/// Registers the phase's state, a fresh copy for each campaign.
///
/// # Errors
///
/// Never fails; the signature is the one every phase has.
pub fn register(reg: &mut PhaseRegistry) -> Result<(), RegisterError> {
    reg.local(state::PaperState::default);
    Ok(())
}

/// Adds the phase's systems and edges.
///
/// # Errors
///
/// Returns the reason when a setting asks for something the phase does not
/// build, or a block cannot be connected.
pub fn build(graph: Graph, ctx: &PhaseCtx) -> Result<Graph, BuildError> {
    let plan = PaperPlan::from_config(&ctx.config, ctx.prefix.clone())?;
    build_with_plan(graph, plan)
}

/// Adds the phase's systems and edges for an explicit plan. Tests use it to
/// point the phase at stand-in TeX programs and reference services.
///
/// # Errors
///
/// Returns the error of a block that cannot be connected.
pub fn build_with_plan(graph: Graph, plan: PaperPlan) -> Result<Graph, BuildError> {
    graph::build(graph, plan)
}
