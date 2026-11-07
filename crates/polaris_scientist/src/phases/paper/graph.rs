//! The graph of the paper phase.
//!
//! The phase is a sequence of blocks, each built from the blocks of
//! `polaris_cosmos::flow`: a role call is `<id>.call` (builds the call),
//! [`flow::agent`] (runs it) and `<id>.check` (reads the outcome and the
//! files, accepts or asks for a retry). A step that may be retried sits in a
//! [`flow::repair_loop`] with a counter of its own; the repairs of the paper
//! itself share one counter, `paper.repair_round`.
//!
//! ```text
//! paper.stage
//! → paper.refs.plan ⟲ → paper.refs.fetch → paper.strategy ⟲ → paper.rank ⟲
//! → paper.draft
//! → paper.gaps ⟲ → paper.refs.plan2 ⟲ → paper.refs.fetch2 → paper.enrich
//! → paper.figures
//! → ⟲ paper.loop: paper.check → paper.audit ⟲ → paper.review ⟲
//!                 → paper.decide  ─ repair → paper.repair ─┐
//! → paper.conclude → paper.finalize | paper.incomplete     │
//!      (loop back to paper.check) ←────────────────────────┘
//! ```
//!
//! A block runs only while the phase is still working toward a paper; once a
//! step has ended it (see [`PaperState::end`]) the remaining blocks pass
//! without work and `paper.conclude` reports the reason.

use std::sync::Arc;

use polaris::graph::Graph;
use polaris::system::param::{Out, Res, ResMut};
use polaris::system::system::SystemError;
use polaris_cosmos::flow::{
    self, AgentCall, AgentOutcome, FlowResult, FlowSettings, PhaseStatus, RepairCounters, RoleBook,
    RunHandle, StepKey, Verdict, bind,
};

use super::calls::{self, CallEnv, Request, RoleSpec};
use super::decision::{AuditResult, REPAIR_COUNTER};
use super::refs::{self, Retrieval};
use super::stage;
use super::state::{
    AUDIT_RETRY_COUNTER, GAPS_COUNTER, PREPARATION_COUNTERS, PaperPlan, PaperState, RANK_COUNTER,
    REFS_PLAN_COUNTER, REFS_PLAN2_COUNTER, REPLY_RETRIES, REVIEW_RETRY_COUNTER, STRATEGY_COUNTER,
    paper_dir,
};
use super::steps::{self, CheckResult, Cx};

/// Marker for the guard of a block.
#[derive(Clone)]
struct Live(bool);

/// Marker: the stage step has run.
#[derive(Clone)]
struct Staged;

/// What the paper check found.
#[derive(Clone)]
struct Checked;

/// Marker: a reply-only step starts.
#[derive(Clone)]
struct Begun;

/// Marker for the branch that decides whether the audit runs.
#[derive(Clone)]
struct AuditPlanned(bool);

/// Marker for the branch that decides whether a review round runs.
#[derive(Clone)]
struct ReviewPlanned(bool);

/// How the phase ended, for the branch after the loop.
#[derive(Clone)]
struct Concluded {
    complete: bool,
    reason: String,
}

/// The key and the retry feedback of a call.
type Step = FlowResult<(StepKey, Option<String>)>;

/// Builds the key and the retry feedback of a call from the state.
type StepFn = fn(&PaperPlan, &PaperState, &RepairCounters) -> Step;

/// A check after a role call.
type CheckFn = fn(&mut Cx<'_>) -> Verdict;

fn failed(message: impl Into<String>) -> SystemError {
    SystemError::ExecutionError(message.into())
}

/// Adds `<id>.call`: the system that builds the call of a role.
fn add_call(
    graph: &mut Graph,
    name: &'static str,
    plan: &Arc<PaperPlan>,
    role: &'static RoleSpec,
    step: StepFn,
) {
    let plan = Arc::clone(plan);
    graph.add_system(bind::<
        (
            Res<PaperState>,
            Res<RunHandle>,
            Res<FlowSettings>,
            Res<RoleBook>,
            Res<RepairCounters>,
        ),
        AgentCall,
        _,
    >(
        name,
        move |(state, handle, settings, roles, counters), _| {
            let plan = Arc::clone(&plan);
            Box::pin(async move {
                let dir = paper_dir(&handle);
                let (key, feedback) = step(&plan, &state, &counters)?;
                let env = CallEnv {
                    plan: &plan,
                    state: &state,
                    paper_dir: &dir,
                    assets: &settings.assets,
                    roles: &roles,
                };
                calls::assemble(
                    &env,
                    Request {
                        role,
                        step: key,
                        feedback,
                    },
                )
                .map_err(failed)
            })
        },
    ));
}

/// Adds `<id>.check`: the system that reads a call's outcome.
fn add_check(graph: &mut Graph, name: &'static str, plan: &Arc<PaperPlan>, check: CheckFn) {
    let plan = Arc::clone(plan);
    graph.add_system(bind::<
        (
            Out<AgentOutcome>,
            Out<AgentCall>,
            ResMut<PaperState>,
            Res<RunHandle>,
        ),
        Verdict,
        _,
    >(name, move |(outcome, call, mut state, handle), _| {
        let plan = Arc::clone(&plan);
        Box::pin(async move {
            let dir = paper_dir(&handle);
            let mut cx = Cx {
                plan: &plan,
                state: &mut state,
                call: &call,
                outcome: &outcome,
                root: &handle.root,
                paper_dir: &dir,
            };
            Ok(check(&mut cx))
        })
    }));
}

/// One role call: call, agent, check.
fn call_block(
    plan: &Arc<PaperPlan>,
    id: &'static str,
    role: &'static RoleSpec,
    names: (&'static str, &'static str),
    step: StepFn,
    check: CheckFn,
) -> Graph {
    let mut graph = Graph::new();
    add_call(&mut graph, names.0, plan, role, step);
    graph
        .append(flow::agent(id, role.key))
        .expect("an agent block has a single entry and exit");
    add_check(&mut graph, names.1, plan, check);
    graph
}

/// A role call that is retried until its output passes, on a counter of its own.
fn retried(loop_name: &'static str, counter: &'static str, call: Graph) -> Graph {
    flow::repair_loop(loop_name, counter, call, Graph::new())
}

/// After a retried step: a step that stayed invalid ends the phase when it is
/// required and only notes the cap when it is optional.
fn add_settle(
    graph: &mut Graph,
    name: &'static str,
    what: &'static str,
    counter: &'static str,
    required: bool,
) {
    graph.add_system(bind::<(Out<Verdict>, ResMut<PaperState>), (), _>(
        name,
        move |(verdict, mut state), _| {
            Box::pin(async move {
                if let Verdict::Fail(message) = &*verdict {
                    state.caps_hit.insert(counter.to_string());
                    if required {
                        state.end(format!(
                            "{what} stayed invalid after the allowed attempts: {message}"
                        ));
                    }
                }
                state.retry_feedback = None;
                Ok(())
            })
        },
    ));
}

/// Wraps a block so that it runs only when `when` holds for the state.
fn guarded(
    names: (&'static str, &'static str),
    when: fn(&PaperState) -> bool,
    block: Graph,
) -> Graph {
    let mut graph = Graph::new();
    graph.add_system(bind::<Res<PaperState>, Live, _>(
        names.0,
        move |state, _| Box::pin(async move { Ok(Live(when(&state))) }),
    ));
    graph.add_conditional_branch::<Live, _, _, _>(
        names.1,
        |live| live.0,
        move |arm| {
            arm.append(block).expect("a guarded block is connected");
        },
        |arm| {
            arm.add_system(|| async {});
        },
    );
    graph
}

fn live(state: &PaperState) -> bool {
    state.live()
}

fn live_and_drafted(state: &PaperState) -> bool {
    state.live() && state.drafted
}

fn has_gaps(state: &PaperState) -> bool {
    state.live() && state.gaps.as_ref().is_some_and(|gaps| !gaps.none)
}

fn has_plan2(state: &PaperState) -> bool {
    state.live() && !state.plan2_queries.is_empty()
}

fn has_analyses(state: &PaperState) -> bool {
    state.live() && !state.analyses.is_empty()
}

// --- step keys ------------------------------------------------------------

fn attempt(
    plan: &PaperPlan,
    name: &str,
    counters: &RepairCounters,
    counter: &str,
) -> FlowResult<StepKey> {
    plan.key(&format!("{name}.a{}", counters.used(counter)))
}

fn refs_plan_step(plan: &PaperPlan, state: &PaperState, counters: &RepairCounters) -> Step {
    Ok((
        attempt(plan, "refs.plan", counters, REFS_PLAN_COUNTER)?,
        state.retry_feedback.clone(),
    ))
}

fn strategy_step(plan: &PaperPlan, state: &PaperState, counters: &RepairCounters) -> Step {
    Ok((
        attempt(plan, "strategy", counters, STRATEGY_COUNTER)?,
        state.retry_feedback.clone(),
    ))
}

fn rank_step(plan: &PaperPlan, state: &PaperState, counters: &RepairCounters) -> Step {
    Ok((
        attempt(plan, "rank", counters, RANK_COUNTER)?,
        state.retry_feedback.clone(),
    ))
}

fn draft_step(plan: &PaperPlan, _: &PaperState, _: &RepairCounters) -> Step {
    Ok((plan.key("draft")?, None))
}

fn gaps_step(plan: &PaperPlan, state: &PaperState, counters: &RepairCounters) -> Step {
    Ok((
        attempt(plan, "gaps", counters, GAPS_COUNTER)?,
        state.retry_feedback.clone(),
    ))
}

fn plan2_step(plan: &PaperPlan, state: &PaperState, counters: &RepairCounters) -> Step {
    Ok((
        attempt(plan, "refs.plan2", counters, REFS_PLAN2_COUNTER)?,
        state.retry_feedback.clone(),
    ))
}

fn enrich_step(plan: &PaperPlan, _: &PaperState, _: &RepairCounters) -> Step {
    Ok((plan.key("enrich")?, None))
}

fn round_key(
    plan: &PaperPlan,
    name: &str,
    counters: &RepairCounters,
    retries: &str,
) -> FlowResult<StepKey> {
    plan.key(&format!(
        "{name}.r{:02}.a{}",
        counters.used(REPAIR_COUNTER),
        counters.used(retries)
    ))
}

fn audit_step(plan: &PaperPlan, state: &PaperState, counters: &RepairCounters) -> Step {
    Ok((
        round_key(plan, "audit", counters, AUDIT_RETRY_COUNTER)?,
        steps::contract_feedback(state.audit_slot.last_error.as_ref()),
    ))
}

fn review_step(plan: &PaperPlan, state: &PaperState, counters: &RepairCounters) -> Step {
    Ok((
        round_key(plan, "review", counters, REVIEW_RETRY_COUNTER)?,
        steps::contract_feedback(state.review_slot.last_error.as_ref()),
    ))
}

fn repair_step(plan: &PaperPlan, _: &PaperState, counters: &RepairCounters) -> Step {
    Ok((
        plan.key(&format!("repair.r{:02}", counters.used(REPAIR_COUNTER)))?,
        None,
    ))
}

// --- the blocks -------------------------------------------------------------

/// `paper.stage`: stages the evidence and sets the counters.
fn add_stage(graph: &mut Graph, plan: &Arc<PaperPlan>) {
    let plan = Arc::clone(plan);
    graph.add_system(bind::<
        (ResMut<PaperState>, Res<RunHandle>, Res<RepairCounters>),
        Staged,
        _,
    >(
        "paper.stage",
        move |(mut state, handle, counters), _| {
            let plan = Arc::clone(&plan);
            Box::pin(async move {
                *state = PaperState::default();
                counters.reset(REPAIR_COUNTER, plan.max_repair_rounds)?;
                for name in PREPARATION_COUNTERS {
                    counters.reset(name, plan.max_repair_rounds)?;
                }
                counters.reset(AUDIT_RETRY_COUNTER, REPLY_RETRIES)?;
                counters.reset(REVIEW_RETRY_COUNTER, REPLY_RETRIES)?;
                let dir = paper_dir(&handle);
                if let Err(e) = stage::clean(&dir) {
                    state.end(format!("the paper folder could not be cleaned: {e}"));
                    return Ok(Staged);
                }
                match stage::run(&handle.root, &dir) {
                    Ok(dossier) => {
                        state.source_ids = dossier.source_ids;
                        state.analyses = dossier.analyses;
                        state.task_goal = dossier.goal;
                    }
                    Err(e) => state.end(e),
                }
                Ok(Staged)
            })
        },
    ));
}

/// `paper.refs.fetch` or `paper.refs.fetch2`: retrieval kept in the journal
/// so that a restarted phase does not ask the services again, then stored.
fn fetch_block(
    plan: &Arc<PaperPlan>,
    key: &'static str,
    names: (&'static str, &'static str),
    second_round: bool,
) -> FlowResult<Graph> {
    let mut body = Graph::new();
    let fetch_plan = Arc::clone(plan);
    body.add_system(bind::<Res<PaperState>, Retrieval, _>(
        names.0,
        move |state, _| {
            let plan = Arc::clone(&fetch_plan);
            let queries = if second_round {
                state.plan2_queries.clone()
            } else {
                state.plan_queries.clone()
            };
            Box::pin(async move {
                let stage = if second_round { "postdraft" } else { "plan" };
                Ok(refs::retrieve(&plan.refs, &queries, stage).await)
            })
        },
    ));
    let mut graph = flow::memo::<Retrieval>(plan.key(key)?, body);
    let store_plan = Arc::clone(plan);
    graph.add_system(bind::<
        (Out<Retrieval>, Res<PaperState>, Res<RunHandle>),
        (),
        _,
    >(names.1, move |(retrieval, state, handle), _| {
        let plan = Arc::clone(&store_plan);
        Box::pin(async move {
            let dir = paper_dir(&handle);
            steps::store_references(&plan, &state, &dir, &retrieval, second_round).map_err(failed)
        })
    }));
    Ok(graph)
}

/// `paper.figures`: the early look at the figure contract.
fn add_figures(graph: &mut Graph) {
    graph.add_system(bind::<(Res<PaperState>, Res<RunHandle>), (), _>(
        "paper.figures",
        |(state, handle), _| {
            Box::pin(async move {
                let dir = paper_dir(&handle);
                steps::early_figure_look(&state, &dir);
                Ok(())
            })
        },
    ));
}

/// The two-stage part: gaps, post-draft plan, second retrieval, enrichment.
fn two_stage_block(plan: &Arc<PaperPlan>) -> FlowResult<Graph> {
    let mut graph = Graph::new();
    graph
        .append(retried(
            "paper.gaps.loop",
            GAPS_COUNTER,
            call_block(
                plan,
                "paper.gaps",
                &calls::GAP_ANALYST,
                ("paper.gaps.call", "paper.gaps.check"),
                gaps_step,
                steps::gaps,
            ),
        ))
        .expect("the gap loop is connected");
    add_settle(
        &mut graph,
        "paper.gaps.settle",
        "the draft gap analysis",
        GAPS_COUNTER,
        false,
    );

    let mut enrichment = Graph::new();
    enrichment
        .append(retried(
            "paper.refs.plan2.loop",
            REFS_PLAN2_COUNTER,
            call_block(
                plan,
                "paper.refs.plan2",
                &calls::POSTDRAFT_PLANNER,
                ("paper.refs.plan2.call", "paper.refs.plan2.check"),
                plan2_step,
                steps::postdraft_plan,
            ),
        ))
        .expect("the post-draft plan loop is connected");
    add_settle(
        &mut enrichment,
        "paper.refs.plan2.settle",
        "the post-draft reference plan",
        REFS_PLAN2_COUNTER,
        false,
    );
    let mut fetch_and_weave = fetch_block(
        plan,
        "refs.fetch2",
        ("paper.refs.fetch2", "paper.refs.store2"),
        true,
    )?;
    fetch_and_weave
        .append(guarded(
            ("paper.enrich.when", "paper.enrich.guard"),
            live,
            call_block(
                plan,
                "paper.enrich",
                &calls::PAPER_ENRICHER,
                ("paper.enrich.call", "paper.enrich.check"),
                enrich_step,
                steps::enrich,
            ),
        ))
        .expect("the enrichment block is connected");
    enrichment
        .append(guarded(
            ("paper.refs.fetch2.when", "paper.refs.fetch2.guard"),
            has_plan2,
            fetch_and_weave,
        ))
        .expect("the second retrieval is connected");
    graph
        .append(guarded(
            ("paper.refs.plan2.when", "paper.refs.plan2.guard"),
            has_gaps,
            enrichment,
        ))
        .expect("the enrichment is connected");
    Ok(graph)
}

/// What `paper.check` found, kept apart from the state because the check
/// awaits the TeX build.
fn add_check_round(graph: &mut Graph, plan: &Arc<PaperPlan>) {
    let plan = Arc::clone(plan);
    graph.add_system(bind::<(Res<RunHandle>,), CheckResult, _>(
        "paper.check",
        move |(handle,), _| {
            let plan = Arc::clone(&plan);
            Box::pin(async move { Ok(steps::check_round(&plan, &paper_dir(&handle)).await) })
        },
    ));
    graph.add_system(bind::<(Out<CheckResult>, ResMut<PaperState>), Checked, _>(
        "paper.check.record",
        |(result, mut state), _| {
            Box::pin(async move {
                steps::record_check(&mut state, &result);
                Ok(Checked)
            })
        },
    ));
}

/// The names of the systems of a reply-only step.
struct Reply {
    begin: &'static str,
    record: &'static str,
    id: &'static str,
    loop_name: &'static str,
    counter: &'static str,
    call: (&'static str, &'static str),
    role: &'static RoleSpec,
    step: StepFn,
    check: CheckFn,
    begin_fn: fn(&mut PaperState, &std::path::Path),
    record_fn: fn(&mut PaperState, &std::path::Path, &std::path::Path),
}

/// A reply-only step: begin, a retried call, record. The inputs are hashed at
/// the beginning and again when the replies are recorded; a change fails the step.
fn reply_block(plan: &Arc<PaperPlan>, reply: Reply) -> Graph {
    let mut graph = Graph::new();
    let begin_fn = reply.begin_fn;
    let counter = reply.counter;
    graph.add_system(bind::<
        (ResMut<PaperState>, Res<RunHandle>, Res<RepairCounters>),
        Begun,
        _,
    >(
        reply.begin,
        move |(mut state, handle, counters), _| {
            Box::pin(async move {
                counters.reset(counter, REPLY_RETRIES)?;
                begin_fn(&mut state, &paper_dir(&handle));
                Ok(Begun)
            })
        },
    ));
    graph
        .append(retried(
            reply.loop_name,
            reply.counter,
            call_block(
                plan,
                reply.id,
                reply.role,
                reply.call,
                reply.step,
                reply.check,
            ),
        ))
        .expect("the reply loop is connected");
    let record_fn = reply.record_fn;
    graph.add_system(bind::<(ResMut<PaperState>, Res<RunHandle>), (), _>(
        reply.record,
        move |(mut state, handle), _| {
            Box::pin(async move {
                record_fn(&mut state, &handle.root, &paper_dir(&handle));
                Ok(())
            })
        },
    ));
    graph
}

fn audit_block(plan: &Arc<PaperPlan>) -> Graph {
    reply_block(
        plan,
        Reply {
            begin: "paper.audit.begin",
            record: "paper.audit.record",
            id: "paper.audit",
            loop_name: "paper.audit.loop",
            counter: AUDIT_RETRY_COUNTER,
            call: ("paper.audit.call", "paper.audit.check"),
            role: &calls::EVIDENCE_AUDITOR,
            step: audit_step,
            check: steps::audit,
            begin_fn: steps::begin_reply,
            record_fn: steps::record_audit,
        },
    )
}

fn review_block(plan: &Arc<PaperPlan>) -> Graph {
    reply_block(
        plan,
        Reply {
            begin: "paper.review.begin",
            record: "paper.review.record",
            id: "paper.review",
            loop_name: "paper.review.loop",
            counter: REVIEW_RETRY_COUNTER,
            call: ("paper.review.call", "paper.review.check"),
            role: &calls::PRESENTATION_REVIEWER,
            step: review_step,
            check: steps::review,
            begin_fn: steps::begin_reply,
            record_fn: steps::record_review,
        },
    )
}

/// One round of the paper loop: check, audit, planned review, decision.
fn round_check(plan: &Arc<PaperPlan>) -> Graph {
    let mut round = Graph::new();
    add_check_round(&mut round, plan);

    let audit_plan = Arc::clone(plan);
    round.add_system(
        bind::<(ResMut<PaperState>, Res<RunHandle>), AuditPlanned, _>(
            "paper.audit.plan",
            move |(mut state, handle), _| {
                let _ = &audit_plan;
                Box::pin(async move {
                    let run = paper_dir(&handle).join("paper.tex").is_file();
                    if !run {
                        steps::no_manuscript(&mut state);
                    }
                    Ok(AuditPlanned(run))
                })
            },
        ),
    );
    round.add_conditional_branch::<AuditPlanned, _, _, _>(
        "paper.audit.planned",
        |planned| planned.0,
        {
            let plan = Arc::clone(plan);
            move |arm| {
                arm.append(audit_block(&plan))
                    .expect("the audit is connected");
            }
        },
        |arm| {
            arm.add_system(|| async {});
        },
    );

    let review_plan = Arc::clone(plan);
    round.add_system(bind::<(Res<PaperState>, Res<RunHandle>), ReviewPlanned, _>(
        "paper.review.plan",
        move |(state, handle), _| {
            let plan = Arc::clone(&review_plan);
            Box::pin(async move {
                let manuscript = paper_dir(&handle).join("paper.tex").is_file();
                Ok(ReviewPlanned(
                    manuscript && steps::review_is_due(&plan, &state),
                ))
            })
        },
    ));
    round.add_conditional_branch::<ReviewPlanned, _, _, _>(
        "paper.review.planned",
        |planned| planned.0,
        {
            let plan = Arc::clone(plan);
            move |arm| {
                arm.append(review_block(&plan))
                    .expect("the review is connected");
            }
        },
        |arm| {
            arm.add_system(|| async {});
        },
    );

    round.add_system(bind::<
        (ResMut<PaperState>, Res<RepairCounters>, Res<RunHandle>),
        Verdict,
        _,
    >("paper.decide", |(mut state, counters, handle), _| {
        Box::pin(async move { Ok(steps::decide(&mut state, &counters, &paper_dir(&handle))) })
    }));

    // Once the phase has ended, the loop passes without work.
    let mut guarded_round = Graph::new();
    guarded_round.add_system(bind::<Res<PaperState>, Live, _>(
        "paper.loop.when",
        |state, _| Box::pin(async move { Ok(Live(state.live())) }),
    ));
    guarded_round.add_conditional_branch::<Live, _, _, _>(
        "paper.loop.guard",
        |live| live.0,
        move |arm| {
            arm.append(round).expect("the round is connected");
        },
        |arm| {
            arm.add_system(|| async { Verdict::Pass });
        },
    );
    guarded_round
}

/// The repair step: one call of the repairer, paid from the shared counter.
fn repair(plan: &Arc<PaperPlan>) -> Graph {
    call_block(
        plan,
        "paper.repair",
        &calls::PAPER_REPAIRER,
        ("paper.repair.call", "paper.repair.check"),
        repair_step,
        steps::repair,
    )
}

/// `paper.conclude` and the two ways out.
fn add_conclusion(graph: &mut Graph, plan: &Arc<PaperPlan>) {
    graph.add_system(bind::<
        (ResMut<PaperState>, Res<RepairCounters>),
        Concluded,
        _,
    >("paper.conclude", |(mut state, counters), _| {
        Box::pin(async move {
            let ending = steps::conclude(&mut state, &counters);
            Ok(Concluded {
                complete: ending.complete,
                reason: ending.reason.unwrap_or_default(),
            })
        })
    }));
    let finalize_plan = Arc::clone(plan);
    let incomplete_plan = Arc::clone(plan);
    graph.add_conditional_branch::<Concluded, _, _, _>(
        "paper.concluded",
        |ending| ending.complete,
        move |arm| {
            arm.add_system(bind::<
                (Res<PaperState>, Res<RepairCounters>, Res<RunHandle>),
                PhaseStatus,
                _,
            >(
                "paper.finalize",
                move |(state, counters, handle), _| {
                    let plan = Arc::clone(&finalize_plan);
                    Box::pin(async move {
                        let dir = paper_dir(&handle);
                        Ok(match steps::finalize(&plan, &state, &counters, &dir) {
                            Ok(()) => PhaseStatus::Completed,
                            Err(reason) => {
                                steps::record_incomplete(&plan, &state, &counters, &dir, &reason);
                                PhaseStatus::Failed(reason)
                            }
                        })
                    })
                },
            ));
        },
        move |arm| {
            arm.add_system(bind::<
                (
                    Out<Concluded>,
                    Res<PaperState>,
                    Res<RepairCounters>,
                    Res<RunHandle>,
                ),
                PhaseStatus,
                _,
            >(
                "paper.incomplete",
                move |(ending, state, counters, handle), _| {
                    let plan = Arc::clone(&incomplete_plan);
                    Box::pin(async move {
                        let dir = paper_dir(&handle);
                        steps::record_incomplete(&plan, &state, &counters, &dir, &ending.reason);
                        Ok(PhaseStatus::Failed(ending.reason.clone()))
                    })
                },
            ));
        },
    );
}

/// Builds the paper phase from `plan` onto `graph`.
///
/// # Errors
///
/// Returns the error of a block that cannot be connected.
pub fn build(mut graph: Graph, plan: PaperPlan) -> Result<Graph, super::super::BuildError> {
    let plan = Arc::new(plan);
    add_stage(&mut graph, &plan);

    graph.append(guarded(
        ("paper.refs.plan.when", "paper.refs.plan.guard"),
        live,
        {
            let mut block = retried(
                "paper.refs.plan.loop",
                REFS_PLAN_COUNTER,
                call_block(
                    &plan,
                    "paper.refs.plan",
                    &calls::REFERENCE_PLANNER,
                    ("paper.refs.plan.call", "paper.refs.plan.check"),
                    refs_plan_step,
                    steps::reference_plan,
                ),
            );
            add_settle(
                &mut block,
                "paper.refs.plan.settle",
                "the reference query plan",
                REFS_PLAN_COUNTER,
                true,
            );
            block
        },
    ))?;
    graph.append(guarded(
        ("paper.refs.fetch.when", "paper.refs.fetch.guard"),
        live,
        fetch_block(
            &plan,
            "refs.fetch",
            ("paper.refs.fetch", "paper.refs.store"),
            false,
        )?,
    ))?;
    graph.append(guarded(
        ("paper.strategy.when", "paper.strategy.guard"),
        live,
        {
            let mut block = retried(
                "paper.strategy.loop",
                STRATEGY_COUNTER,
                call_block(
                    &plan,
                    "paper.strategy",
                    &calls::PAPER_STRATEGIST,
                    ("paper.strategy.call", "paper.strategy.check"),
                    strategy_step,
                    steps::strategy,
                ),
            );
            add_settle(
                &mut block,
                "paper.strategy.settle",
                "the paper strategy",
                STRATEGY_COUNTER,
                true,
            );
            block
        },
    ))?;
    graph.append(guarded(
        ("paper.rank.when", "paper.rank.guard"),
        has_analyses,
        {
            let mut block = retried(
                "paper.rank.loop",
                RANK_COUNTER,
                call_block(
                    &plan,
                    "paper.rank",
                    &calls::POSTHOC_RANKER,
                    ("paper.rank.call", "paper.rank.check"),
                    rank_step,
                    steps::ranking,
                ),
            );
            add_settle(
                &mut block,
                "paper.rank.settle",
                "the post-hoc ranking",
                RANK_COUNTER,
                false,
            );
            block
        },
    ))?;
    graph.append(guarded(
        ("paper.draft.when", "paper.draft.guard"),
        live,
        call_block(
            &plan,
            "paper.draft",
            &calls::PAPER_WRITER,
            ("paper.draft.call", "paper.draft.check"),
            draft_step,
            steps::draft,
        ),
    ))?;
    if plan.two_stage {
        graph.append(guarded(
            ("paper.two_stage.when", "paper.two_stage.guard"),
            live_and_drafted,
            two_stage_block(&plan)?,
        ))?;
    }
    let mut figures = Graph::new();
    add_figures(&mut figures);
    graph.append(guarded(
        ("paper.figures.when", "paper.figures.guard"),
        live,
        figures,
    ))?;
    graph.append(flow::repair_loop(
        "paper.loop",
        REPAIR_COUNTER,
        round_check(&plan),
        repair(&plan),
    ))?;
    add_conclusion(&mut graph, &plan);
    Ok(graph)
}

/// Marker so the unused imports of the audit types stay referenced.
#[allow(dead_code)]
fn _types(_: AuditResult) {}
