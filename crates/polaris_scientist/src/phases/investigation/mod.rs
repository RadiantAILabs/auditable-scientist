//! Leased investigation cycles, bounded idea/candidate loops and independent promotion.
pub mod calls;
pub mod idea;
pub mod memory;
pub mod rubrics;
pub mod selection;

use crate::{
    config::ScientistConfig,
    eval::{self, EvalError},
    phases::{
        BuildError, PhaseCtx, PhaseRegistry, RegisterError,
        task::{self, files},
    },
};
use calls::{context, read_json, workspace};
use polaris::{
    graph::Graph,
    system::{
        param::{Out, Res, ResMut},
        resource::{GlobalResource, LocalResource},
        system,
    },
};
use polaris_cosmos::flow::{
    self, AgentCall, AgentOutcome, HaltReason, PhaseStatus, RepairCounters, RoleBook, RunHandle,
    RunMode, StepKey, Verdict,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Mutex};

#[derive(Default)]
struct IdeaState {
    terminal: bool,
    bundle: Value,
    feedback: String,
    attempt: u32,
}
impl LocalResource for IdeaState {}
#[derive(Default)]
struct CandidateCounter {
    index: u32,
}
impl LocalResource for CandidateCounter {}
#[derive(Default)]
struct Evaluations {
    candidates: Vec<selection::Incumbent>,
}
impl LocalResource for Evaluations {}
#[derive(Default)]
struct LaneBest {
    best: Option<selection::Incumbent>,
}
impl LocalResource for LaneBest {}
#[derive(Default)]
struct LaneOutcomes(
    Mutex<BTreeMap<usize, Result<selection::Incumbent, String>>>,
    Mutex<BTreeMap<usize, String>>,
    Mutex<()>,
);
impl GlobalResource for LaneOutcomes {}
#[derive(Clone, Serialize, Deserialize)]
struct CanonicalIdea {
    bundle: Value,
    directory: PathBuf,
    attempt_id: String,
}
#[derive(Clone)]
struct Evaluation {
    required_failure: bool,
    result: Result<eval::EvalResult, String>,
    program: PathBuf,
    author_finished: bool,
    workspace: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
struct Assessed {
    required_failure: bool,
    correct: bool,
    report: rubrics::RubricReport,
    author_finished: bool,
    workspace: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
struct Promotion {
    updated: bool,
}
fn lane_root(run: &RunHandle, cfg: &ScientistConfig, worker: usize) -> PathBuf {
    if task::number(cfg, "investigation.execution.workers", 1) == 1 {
        run.root.join("investigation")
    } else {
        run.root.join(format!("investigation/worker_{worker:02}"))
    }
}
/// The lease a worker holds while one cycle runs.
///
/// A cycle runs in its own scope and outputs do not cross a scope boundary, so
/// the first step of the cycle reads the lease the lane already holds and
/// publishes it again under this type for the steps that follow.
#[derive(Clone, Debug)]
struct CycleLease {
    worker: usize,
    lease: Option<flow::Lease>,
}
fn lease(active: &CycleLease) -> Result<&flow::Lease, EvalError> {
    active
        .lease
        .as_ref()
        .ok_or_else(|| EvalError("cycle started without a lease".into()))
}
fn cycle_key(active: &CycleLease) -> Result<StepKey, EvalError> {
    let lease = lease(active)?;
    Ok(StepKey::new(format!(
        "investigation/w{:02}/c{:03}",
        active.worker, lease.global
    ))?)
}
fn stamp() -> String {
    chrono::Utc::now().to_rfc3339()
}
/// The last component of `path` as text, for the history records.
fn dir_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
fn outer(best: &selection::Incumbent, cycle: u32, id: &str) -> Value {
    json!({"version":1,"best_combined_score":best.combined_score,"best_correct":best.correct,"best_program_dir":best.program_dir,"created_at":stamp(),"current_cycle":cycle,"failure_streak":0,"last_idea_id":id,"prune_chapter":0,"updated_at":stamp()})
}

fn write_outer(
    path: &std::path::Path,
    best: &selection::Incumbent,
    cycle: u32,
    id: &str,
) -> Result<(), EvalError> {
    let mut state = outer(best, cycle, id);
    if let Ok(previous) = read_json(path) {
        for name in ["created_at", "failure_streak", "prune_chapter"] {
            if let Some(value) = previous.get(name) {
                state[name] = value.clone();
            }
        }
    }
    files::write_json(path, &state)
}

#[system]
async fn inv_open(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    outcomes: Res<LaneOutcomes>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let manifest = run.root.join("investigation/best_program_manifest.json");
        if !manifest.exists() {
            files::copy_tree(&run.root.join("task/baseline/incumbent.json"), &manifest)?;
        }
        let best: selection::Incumbent = serde_json::from_value(read_json(&manifest)?)?;
        let state = run.root.join("investigation/outer_state.json");
        if !state.exists() {
            write_outer(&state, &best, 0, "")?;
        }
        if run.mode == RunMode::Resume {
            for worker in 0..task::number(&cfg, "investigation.execution.workers", 1) as usize {
                let lane = lane_root(&run, &cfg, worker);
                let saved = lane.join("best_program_manifest.json");
                if saved.exists() {
                    let best: selection::Incumbent = serde_json::from_value(read_json(&saved)?)?;
                    outcomes
                        .0
                        .lock()
                        .map_err(|e| EvalError(e.to_string()))?
                        .insert(worker, Ok(best));
                }
                let failure = lane.join("lane_failure.json");
                if failure.exists() {
                    let reason = read_json(&failure)?["reason"]
                        .as_str()
                        .unwrap_or("previous lane failed")
                        .to_owned();
                    outcomes
                        .1
                        .lock()
                        .map_err(|e| EvalError(e.to_string()))?
                        .insert(worker, reason);
                }
            }
        }
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn cycle_prune(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    events: Res<flow::EventLog>,
) -> Result<(), SystemError> {
    let result:Result<(),EvalError>=async move {
        let every=task::number(&cfg,"investigation.execution.prune_every_n_cycles",0);
        if every>0 && u64::from(lease(&active)?.global)>1 && (u64::from(lease(&active)?.global)-1)%every==0 {
            let status=json!({"status":"NotBuilt","reason":"chapter pruning and parallel rounds are unavailable","step":cycle_key(&active)?.child("cycle.prune")?});
            files::write_json(&lane_root(&run,&cfg,active.worker).join("pruning_status.json"),&status)?;
            events.emit("step_skipped",Some("investigation"),status)?;
        }
        Ok(())
    }.await;
    result.map_err(Into::into)
}
#[system]
async fn memory_pull(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    mut state: ResMut<IdeaState>,
    mut counter: ResMut<CandidateCounter>,
    mut evaluations: ResMut<Evaluations>,
    mut best: ResMut<LaneBest>,
    counters: Res<RepairCounters>,
    outcomes: Res<LaneOutcomes>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let _memory_lock = outcomes.2.lock().map_err(|e| EvalError(e.to_string()))?;
        let _ = lease(&active)?;
        outcomes
            .1
            .lock()
            .map_err(|e| EvalError(e.to_string()))?
            .insert(active.worker, "required investigation step failed".into());
        state.terminal = false;
        state.bundle = Value::Null;
        state.feedback.clear();
        state.attempt = 0;
        counter.index = 0;
        evaluations.candidates.clear();
        let lane = lane_root(&run, &cfg, active.worker);
        fs::create_dir_all(&lane)?;
        if best.best.is_none() {
            let path = lane.join("best_program_manifest.json");
            let path = if path.exists() {
                path
            } else {
                run.root.join("task/baseline/incumbent.json")
            };
            best.best = Some(serde_json::from_value(read_json(&path)?)?);
        }
        counters.reset(
            &format!("investigation.w{}.idea", active.worker),
            task::number(
                &cfg,
                "investigation.execution.max_idea_generation_attempts",
                2,
            )
            .saturating_sub(1) as u32,
        )?;
        counters.reset(
            &format!("investigation.w{}.program", active.worker),
            task::number(
                &cfg,
                "investigation.execution.max_program_evals_per_attempt",
                1,
            )
            .saturating_sub(1) as u32,
        )?;
        let shared = run.root.join("investigation/_shared_memory");
        fs::create_dir_all(shared.join("conclusions"))?;
        let mut others = String::new();
        for entry in fs::read_dir(shared.join("conclusions"))? {
            let entry = entry?;
            if entry.file_name() != format!("worker_{:02}.md", active.worker).as_str() {
                others.push_str(&context(&entry.path()));
                others.push('\n');
            }
        }
        files::atomic_write(
            &lane.join("conclusions_other_workers.md"),
            others.as_bytes(),
        )?;
        for name in ["memory.md", "memory.jsonl"] {
            if shared.join(name).exists() {
                files::copy_tree(&shared.join(name), &lane.join(name))?;
            }
        }
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn idea_call(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    roles: Res<RoleBook>,
    active: Out<CycleLease>,
    mut state: ResMut<IdeaState>,
    best: Res<LaneBest>,
) -> Result<AgentCall, SystemError> {
    let result: Result<AgentCall, EvalError> = async move {
        state.attempt += 1;
        let lease = lease(&active)?;
        let lane = lane_root(&run, &cfg, active.worker);
        let mut vars = calls::task_vars(&run.root);
        vars.extend(BTreeMap::from([
            ("CYCLE_INDEX".into(), lease.global.to_string()),
            ("WORKER_INDEX".into(), active.worker.to_string()),
            (
                "OWN_CONCLUSIONS".into(),
                context(&lane.join("conclusions.md")),
            ),
            (
                "OTHER_WORKERS_CONCLUSIONS_SECTION".into(),
                context(&lane.join("conclusions_other_workers.md")),
            ),
            (
                "RECENT_IDEAS_SUMMARY".into(),
                context(&lane.join("ideas_history.jsonl")),
            ),
            (
                "RECENT_ATTEMPTS_SUMMARY".into(),
                context(&lane.join("attempts_history.jsonl")),
            ),
            (
                "BEST_MANIFEST_SUMMARY".into(),
                serde_json::to_string(&best.best)?,
            ),
            (
                "GLOBAL_MEMORY_RECENT_SUMMARY".into(),
                context(&lane.join("memory.md")),
            ),
            (
                "HARNESS_RUBRIC_REQUIREMENTS".into(),
                "Use metric comparisons and executable source_contains patterns where possible."
                    .into(),
            ),
        ]));
        let mut inputs = calls::public_task(&run.root);
        for name in [
            "ideas_history.jsonl",
            "attempts_history.jsonl",
            "memory.md",
            "memory.jsonl",
            "conclusions.md",
            "conclusions_other_workers.md",
        ] {
            if lane.join(name).exists() {
                inputs.push((lane.join(name), name.into()));
            }
        }
        if let Some(best) = &best.best {
            inputs.push((best.program_dir.clone(), "incumbent".into()));
        }
        let request = if state.feedback.is_empty() {
            "Propose one idea.".to_string()
        } else {
            format!(
                "Propose one idea. The previous attempt was rejected: {}",
                state.feedback
            )
        };
        // The role prompt holds the task, the histories and the incumbent's summary,
        // so the agent writes its idea without a reading pass first.
        calls::call_briefed(
            &run,
            &roles,
            "investigation",
            cycle_key(&active)?.child(&format!("idea_{}/idea.write", state.attempt))?,
            "ideation",
            inputs,
            vars,
            vec![
                "outputs/idea_bundle.json".into(),
                "outputs/target_rubrics.json".into(),
            ],
            &request,
            calls::Briefing::Inline,
        )
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn idea_check(
    run: Res<RunHandle>,
    active: Out<CycleLease>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    mut state: ResMut<IdeaState>,
    outcomes: Res<LaneOutcomes>,
) -> Result<Verdict, SystemError> {
    let result: Result<Verdict, EvalError> = async move {
        if matches!(&*outcome, AgentOutcome::Skipped(_)) || matches!(&*outcome,AgentOutcome::Ran{report,..}if report.stop==flow::StopCause::Interrupted) {
            state.terminal = true;
            state.feedback = format!("required role skipped: {:?}", *outcome);
            return Ok(Verdict::Pass);
        }
        let work = workspace(&run, &call.step);
        let checked = (|| {
            calls::required(&outcome)?;
            idea::normalize(
                &read_json(&work.join("outputs/idea_bundle.json"))?,
                &read_json(&work.join("outputs/target_rubrics.json"))?,
                lease(&active)?.global,
            )
        })();
        match checked {
            Ok(bundle) => {
                state.bundle = bundle;
                state.feedback.clear();
                Ok(Verdict::Pass)
            }
            Err(error) => {
                state.feedback = error.to_string().chars().take(2000).collect();
                outcomes
                    .1
                    .lock()
                    .map_err(|e| EvalError(e.to_string()))?
                    .insert(
                        active.worker,
                        format!("IdeationExhausted: {}", state.feedback),
                    );
                Ok(Verdict::Fail(state.feedback.clone()))
            }
        }
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn idea_canon(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    state: Res<IdeaState>,
    events: Res<flow::EventLog>,
) -> Result<CanonicalIdea, SystemError> {
    let result:Result<CanonicalIdea,EvalError> = async move {
    if state.bundle.is_null() {
        if !state.terminal{events.emit("cap_hit",Some("investigation"),json!({"cap":"max_idea_generation_attempts","limit":task::number(&cfg,"investigation.execution.max_idea_generation_attempts",2),"used":state.attempt,"step":cycle_key(&active)?.child("idea.check")?}))?;}

        return Err(EvalError(if state.terminal{state.feedback.clone()}else{"IdeationExhausted".into()}));
    }
    let cycle = lease(&active)?.global;
    let id = state.bundle["idea_id"]
        .as_str()
        .ok_or_else(|| EvalError("accepted idea has no id".into()))?;
    let lane = lane_root(&run, &cfg, active.worker);
    let directory = lane.join("ideas").join(format!(
        "{cycle:03}_{}",
        idea::slug(state.bundle["title"].as_str().unwrap_or(id))
    ));
    if !directory.join("idea_bundle.json").exists() {
        files::write_json(&directory.join("idea_bundle.json"), &state.bundle)?;
        files::write_json(
            &directory.join("target_rubrics.json"),
            &state.bundle["target_rubrics"],
        )?;
        files::append_json(
            &lane.join("ideas_history.jsonl"),
            &json!({"agent":"polaris","agent_model":task::text(&cfg,"runner.model","aws.bedrock/global.anthropic.claude-sonnet-5-5"),"cycle_index":cycle,"idea_dir_name":dir_name(&directory),"idea_id":id,"reasoning_effort":null,"target_rubrics":state.bundle["target_rubrics"],"timestamp":stamp(),"title":state.bundle["title"],"hypothesis":state.bundle["hypothesis"]}),
        )?;
    }
    let incumbent_path=lane.join("best_program_manifest.json");
    let incumbent_path=if incumbent_path.exists(){incumbent_path}else{run.root.join("task/baseline/incumbent.json")};
    let incumbent:selection::Incumbent=serde_json::from_value(read_json(&incumbent_path)?)?;
    write_outer(&lane.join("outer_state.json"),&incumbent,cycle,id)?;
    Ok(CanonicalIdea {
        bundle: state.bundle.clone(),
        directory,
        attempt_id: format!("{id}_attempt_01"),
    })

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn program_call(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    roles: Res<RoleBook>,
    active: Out<CycleLease>,
    idea: Out<CanonicalIdea>,
    mut count: ResMut<CandidateCounter>,
    best: Res<LaneBest>,
) -> Result<AgentCall, SystemError> {
    let result:Result<AgentCall,EvalError> = async move {
    count.index += 1;
    let best = best
        .best
        .as_ref()
        .ok_or_else(|| EvalError("no incumbent".into()))?;
    let seed = if count.index == 1 {
        best.program_dir.clone()
    } else {
        idea.directory.join(format!("program_{}", count.index - 1))
    };
    let seed_copy = idea.directory.join(format!("seed_{}", count.index));
    if !seed_copy.exists() {
        files::copy_tree(&seed, &seed_copy)?;
        super::ablations::artifacts::remove_results(&seed_copy)?;
    }
    let mut inputs = calls::public_task(&run.root);
    inputs.extend([
        (seed_copy, "program_0".into()),
        (
            idea.directory.join("idea_bundle.json"),
            "idea_bundle.json".into(),
        ),
        (
            idea.directory.join("target_rubrics.json"),
            "target_rubrics.json".into(),
        ),
    ]);
    let mut vars = calls::task_vars(&run.root);
    vars.extend(BTreeMap::from([
        ("ATTEMPT_ID".into(), idea.attempt_id.clone()),
            ("CYCLE_INDEX".into(),lease(&active)?.global.to_string()),
            ("WORKER_INDEX".into(),active.worker.to_string()),
        (
            "IDEA_ID".into(),
            idea.bundle["idea_id"].as_str().unwrap_or("").into(),
        ),
        ("ACTIVE_IDEA_JSON".into(), serde_json::to_string_pretty(&idea.bundle)?),
        (
            "ACTIVE_RUBRICS_JSON".into(),
            serde_json::to_string_pretty(&idea.bundle["target_rubrics"])?,
        ),
        (
            "MAX_PROGRAM_EVALS_PER_ATTEMPT".into(),
            calls::cap(
                &cfg,
                "investigation.execution.max_program_evals_per_attempt",
                1,
            ),
        ),
    ]));
    calls::call(
        &run,
        &roles,
        "investigation",
        cycle_key(&active)?.child(&format!("a{}/prog.edit", count.index))?,
        "program_author",
        inputs,
        vars,
        vec!["outputs/program_1/program_spec.toml".into()],
        "Author exactly outputs/program_1 from inputs/program_0. The host scores after you stop.",
    )

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn program_eval(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    idea: Out<CanonicalIdea>,
    count: Res<CandidateCounter>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    mut evaluations: ResMut<Evaluations>,
) -> Result<Evaluation, SystemError> {
    let result: Result<Evaluation, EvalError> = async move {
        let required_failure = matches!(&*outcome, AgentOutcome::Skipped(_)) || matches!(&*outcome,AgentOutcome::Ran{report,..}if report.stop==flow::StopCause::Interrupted);
        let work = workspace(&run, &call.step);
        let source = work.join("outputs/program_1");
        let program = idea.directory.join(format!("program_{}", count.index));
        if !required_failure && !program.exists() && source.exists() {
            files::copy_editable_tree(&source, &program)?;
        }
        let result = if required_failure {
            Err("required program author reservation refused".into())
        } else if !program.exists() {
            Err("candidate missing".into())
        } else {
            let result = eval::run_evaluator(task::evaluator_request(
                &run.root,
                &cfg,
                program.clone(),
                program.join("results"),
            )?)
            .await;
            match result {
                Ok(result) => {
                    if let Some(candidate) =
                        selection::candidate(&program, &program.join("results"), &idea.attempt_id)?
                    {
                        evaluations.candidates.push(candidate);
                    }
                    Ok(result)
                }
                Err(error) => Err(error.to_string()),
            }
        };
        Ok(Evaluation {
            required_failure,
            result,
            program,
            author_finished: calls::finished(&outcome),
            workspace: work,
        })
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn rubric_check(
    run: Res<RunHandle>,
    idea: Out<CanonicalIdea>,
    evaluation: Out<Evaluation>,
) -> Result<Assessed, SystemError> {
    let result: Result<Assessed, EvalError> = async move {
        let targets = idea.bundle["target_rubrics"]
            .as_array()
            .ok_or_else(|| EvalError("missing idea rubrics".into()))?;
        let report = match &evaluation.result {
            Ok(result) => {
                let runtime: task::runtime::RuntimeRecord =
                    serde_json::from_value(read_json(&run.root.join("task/runtime.json"))?)?;
                rubrics::mechanical(
                    &runtime.python,
                    &evaluation.program,
                    targets,
                    &result.metrics,
                    result.score().is_some(),
                )
                .await?
            }
            Err(reason) => rubrics::RubricReport {
                rubrics: targets
                    .iter()
                    .map(|r| rubrics::RubricAssessment {
                        id: r["id"].as_str().unwrap_or("").into(),
                        status: "not_assessed".into(),
                        evidence: reason.clone(),
                    })
                    .collect(),
                overall_pass: false,
            },
        };
        if evaluation.program.exists() {
            files::write_json(
                &evaluation.program.join("results/rubric_status.json"),
                &report,
            )?;
        }
        Ok(Assessed {
            required_failure: evaluation.required_failure,
            correct: evaluation
                .result
                .as_ref()
                .is_ok_and(|r| r.score().is_some()),
            report,
            author_finished: evaluation.author_finished,
            workspace: evaluation.workspace.clone(),
        })
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn rubric_judge_call(
    run: Res<RunHandle>,
    roles: Res<RoleBook>,
    active: Out<CycleLease>,
    idea: Out<CanonicalIdea>,
    evaluation: Out<Evaluation>,
    count: Res<CandidateCounter>,
) -> Result<AgentCall, SystemError> {
    let result:Result<AgentCall,EvalError>=async move {
        let mut vars=calls::task_vars(&run.root);
        vars.extend(BTreeMap::from([("RESULTS_DIR".into(),"inputs".into()),("ATTEMPT_ID".into(),idea.attempt_id.clone()),("IDEA_ID".into(),idea.bundle["idea_id"].as_str().unwrap_or("").into()),("MAX_PROGRAM_EVALS_PER_ATTEMPT".into(),"1".into())]));
        let template=idea.directory.join(format!("rubric_template_{}.json",count.index));
        files::write_json(&template,&json!({"attempt_id":idea.attempt_id,"idea_id":idea.bundle["idea_id"],"rubrics":idea.bundle["target_rubrics"].as_array().unwrap_or(&Vec::new()).iter().map(|r|json!({"id":r["id"],"pass":false,"evidence":""})).collect::<Vec<_>>()}))?;
        calls::call(&run,&roles,"investigation",cycle_key(&active)?.child(&format!("a{}/rubric.judge",count.index))?,"rubric_verifier",vec![(template,"rubric_status.json".into()),(evaluation.program.clone(),"program_1".into()),(idea.directory.join("target_rubrics.json"),"target_rubrics.json".into())],vars,vec!["outputs/rubric_status.json".into()],"Judge the unassessed semantic rubrics using only the host metrics and candidate source. Copy inputs/rubric_status.json to outputs/rubric_status.json and fill id, boolean pass, and evidence for each rubric. Do not edit inputs.")
    }.await;
    result.map_err(Into::into)
}
#[system]
async fn rubric_judge_check(
    run: Res<RunHandle>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    evaluation: Out<Evaluation>,
    assessed: Out<Assessed>,
) -> Result<Assessed, SystemError> {
    let result: Result<Assessed, EvalError> = async move {
        let mut result = (*assessed).clone();
        if calls::finished(&outcome) {
            let judgement =
                calls::read_json(&workspace(&run, &call.step).join("outputs/rubric_status.json"));
            if let Ok(judgement) = judgement {
                let mut updated = result.report.clone();
                if rubrics::merge_judgement(&mut updated, &judgement).is_ok() {
                    result.report = updated;
                }
            }
        }
        files::write_json(
            &evaluation.program.join("results/rubric_status.json"),
            &result.report,
        )?;
        Ok(result)
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn rubric_verdict(
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    count: Res<CandidateCounter>,
    assessed: Out<Assessed>,
    events: Res<flow::EventLog>,
) -> Result<Verdict, SystemError> {
    if !assessed.required_failure
        && !assessed.report.overall_pass
        && u64::from(count.index)
            >= task::number(
                &cfg,
                "investigation.execution.max_program_evals_per_attempt",
                1,
            )
    {
        events.emit("cap_hit",Some("investigation"),json!({"cap":"max_program_evals_per_attempt","limit":task::number(&cfg,"investigation.execution.max_program_evals_per_attempt",1),"used":count.index,"step":cycle_key(&active)?.child("rubric.check")?})).map_err(EvalError::from)?;
    }
    Ok(
        if assessed.required_failure || assessed.report.overall_pass {
            Verdict::Pass
        } else {
            Verdict::Fail("rubrics not all passed".into())
        },
    )
}
#[system]
async fn best_promote(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    idea: Out<CanonicalIdea>,
    mut best: ResMut<LaneBest>,
    evaluations: Res<Evaluations>,
) -> Result<Promotion, SystemError> {
    let result: Result<Promotion, EvalError> = async move {
        let mut updated = false;
        for candidate in &evaluations.candidates {
            updated |= selection::promote(&mut best.best, candidate.clone());
        }
        if let Some(best) = &best.best {
            let lane = lane_root(&run, &cfg, active.worker);
            files::write_json(&lane.join("best_program_manifest.json"), best)?;
            write_outer(
                &lane.join("outer_state.json"),
                best,
                lease(&active)?.global,
                idea.bundle["idea_id"].as_str().unwrap_or(""),
            )?;
        }
        Ok(Promotion { updated })
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn attempt_record(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    idea: Out<CanonicalIdea>,
    assessed: Out<Assessed>,
    promotion: Out<Promotion>,
    evaluation: Out<Evaluation>,
    evaluations: Res<Evaluations>,
    control: Option<Res<flow::PhaseControl>>,
) -> Result<(), SystemError> {
    let result:Result<(),EvalError> = async move {
    if control.is_some_and(|c|c.halt_reason()==Some(HaltReason::Interrupted)){return Err(EvalError("interrupted".into()));}
    let lane = lane_root(&run, &cfg, active.worker);
    let receipt = idea.directory.join("attempt_record.json");
    if receipt.exists() {
        return Ok(());
    }
    let best_attempt=evaluations.candidates.iter().max_by(|a,b|a.combined_score.total_cmp(&b.combined_score));
    let metrics=best_attempt.map(|c|json!({"combined_score":c.combined_score,"correct":true})).unwrap_or_else(||match &evaluation.result{Ok(result)=>json!({"combined_score":result.metrics.get("combined_score"),"correct":result.correctness.get("correct")}),Err(_)=>json!({"combined_score":null,"correct":false})});
    let previous=read_json(&lane.join("outer_state.json")).unwrap_or(Value::Null);
    let passed=assessed.author_finished&&assessed.report.overall_pass;
    let failure_streak=if passed{0}else{previous.get("failure_streak").and_then(Value::as_u64).unwrap_or(0)+1};
    let record = json!({"agent":"polaris","agent_model":task::text(&cfg,"runner.model","aws.bedrock/global.anthropic.claude-sonnet-5-5"),"attempt_id":idea.attempt_id,"attempt_number":1,"attempt_status":if assessed.author_finished&&assessed.report.overall_pass{"pass"}else{"fail"},"best_updated":promotion.updated,"cycle_index":lease(&active)?.global,"experiment_dir":idea.directory,"failure_streak_after":failure_streak,"idea_dir_name":dir_name(&idea.directory),"idea_id":idea.bundle["idea_id"],"metrics":metrics,"output_tail":""});
    files::append_json(&lane.join("attempts_history.jsonl"), &record)?;
    files::write_json(&receipt, &record)?;
    let mut state=previous;state["failure_streak"]=json!(failure_streak);state["updated_at"]=json!(stamp());files::write_json(&lane.join("outer_state.json"),&state)?;
    if assessed.required_failure{return Err(EvalError("required program author reservation refused".into()));}
    Ok(())

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn memory_check(assessed: Out<Assessed>) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let path = assessed.workspace.join("outputs/conclusions.md");
        if path.exists()
            && let Err(error) = memory::validate_conclusions(&fs::read_to_string(&path)?)
        {
            files::write_json(
                &assessed.workspace.join("conclusions_rejected.json"),
                &json!({"reason":error.to_string()}),
            )?;
        }
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn memory_push(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    assessed: Out<Assessed>,
    outcomes: Res<LaneOutcomes>,
) -> Result<Verdict, SystemError> {
    let result: Result<Verdict, EvalError> = async move {
        let _memory_lock = outcomes.2.lock().map_err(|e| EvalError(e.to_string()))?;
        let lane = lane_root(&run, &cfg, active.worker);
        let shared = run.root.join("investigation/_shared_memory");
        for name in ["memory.md", "memory.jsonl"] {
            let source = assessed.workspace.join("outputs").join(name);
            memory::append_new(&source, &lane.join(name))?;
            memory::append_new(&source, &shared.join(name))?;
        }
        let source = assessed.workspace.join("outputs/conclusions.md");
        if memory::publish(&source, &lane.join("conclusions.md"), true).is_ok() {
            memory::publish(
                &lane.join("conclusions.md"),
                &shared.join(format!("conclusions/worker_{:02}.md", active.worker)),
                true,
            )?;
        }
        Ok(Verdict::Pass)
    }
    .await;
    result.map_err(Into::into)
}
#[derive(Clone, Serialize, Deserialize)]
struct JoinedInvestigation {
    best: selection::Incumbent,
    workers: Value,
    failures: Value,
}
#[system]
async fn inv_join(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    outcomes: Res<LaneOutcomes>,
) -> Result<JoinedInvestigation, SystemError> {
    let result: Result<JoinedInvestigation, EvalError> = async move {
        let lanes = outcomes.0.lock().map_err(|e| EvalError(e.to_string()))?;
        let pending = outcomes.1.lock().map_err(|e| EvalError(e.to_string()))?;
        if !pending.is_empty()
            && (task::flag(&cfg, "investigation.execution.parallel_fail_fast", false)
                || !lanes.keys().any(|worker| !pending.contains_key(worker)))
        {
            files::write_json(
                &run.root.join("investigation/parallel_summary.json"),
                &json!({"worker_outcomes":&*lanes,"worker_failures":&*pending,"status":"failed"}),
            )?;
            return Err(EvalError(
                pending
                    .values()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "investigation failed".into()),
            ));
        }
        let winners = lanes
            .iter()
            .filter_map(|(index, result)| result.as_ref().ok().map(|best| (*index, best.clone())))
            .collect();
        let best = selection::join(winners)
            .or_else(|| {
                serde_json::from_value(
                    read_json(&run.root.join("investigation/best_program_manifest.json")).ok()?,
                )
                .ok()
            })
            .ok_or_else(|| EvalError("investigation has no valid incumbent".into()))?;
        if task::number(&cfg, "investigation.execution.workers", 1) > 1 {
            for name in ["ideas_history.jsonl", "attempts_history.jsonl"] {
                let mut lines = String::new();
                for worker in 0..task::number(&cfg, "investigation.execution.workers", 1) {
                    lines.push_str(&context(&lane_root(&run, &cfg, worker as usize).join(name)));
                }
                files::atomic_write(&run.root.join("investigation").join(name), lines.as_bytes())?;
            }
        }
        Ok(JoinedInvestigation {
            best,
            workers: serde_json::to_value(&*lanes)?,
            failures: serde_json::to_value(&*pending)?,
        })
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn inv_close(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    joined: Out<JoinedInvestigation>,
    leases: Res<flow::LeaseBook>,
) -> Result<PhaseStatus, SystemError> {
    let result:Result<PhaseStatus,EvalError>=async move {
        let best=&joined.best;
        files::write_json(&run.root.join("investigation/best_program_manifest.json"),best)?;
        let idea_path=best.program_dir.parent().map(|p|p.join("idea_bundle.json"));
        let idea_id=idea_path.and_then(|p|read_json(&p).ok()).and_then(|v|v["idea_id"].as_str().map(str::to_owned)).unwrap_or_default();
        write_outer(&run.root.join("investigation/outer_state.json"),best,leases.allocated(),&idea_id)?;
        files::write_json(&run.root.join("investigation/parallel_summary.json"),&json!({"workers":task::number(&cfg,"investigation.execution.workers",1),"worker_outcomes":joined.workers,"worker_failures":joined.failures,"completed_no_winner":best.source_attempt_id=="baseline","best_program_dir":best.program_dir}))?;
        Ok(PhaseStatus::Completed)
    }.await;
    result.map_err(Into::into)
}
#[system]
async fn lane_failed(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    error: polaris::system::param::ErrOut<polaris::graph::CaughtError>,
    outcomes: Res<LaneOutcomes>,
) -> Verdict {
    let reason = error.to_string();
    if let Ok(mut pending) = outcomes.1.lock() {
        pending.insert(active.worker, reason.clone());
    }
    let _ = files::write_json(
        &lane_root(&run, &cfg, active.worker).join("lane_failure.json"),
        &json!({"reason":reason,"worker":active.worker,"lease":active.lease}),
    );
    Verdict::Fail(reason)
}
/// Register independent per-lane state and the shared lane outcome ledger.
pub fn register(reg: &mut PhaseRegistry) -> Result<(), RegisterError> {
    reg.local(IdeaState::default);
    reg.local(CandidateCounter::default);
    reg.local(Evaluations::default);
    reg.local(LaneBest::default);
    reg.global(LaneOutcomes::default());
    Ok(())
}
#[derive(Clone)]
struct IdeaAccepted(bool);
#[system]
async fn idea_ready(state: Res<IdeaState>) -> IdeaAccepted {
    IdeaAccepted(!state.bundle.is_null())
}
#[system]
async fn idea_exhausted(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    state: Res<IdeaState>,
    events: Res<flow::EventLog>,
    outcomes: Res<LaneOutcomes>,
) -> Result<Verdict, SystemError> {
    let result:Result<Verdict,EvalError>=async move {
        let reason=if state.terminal{state.feedback.clone()}else{"IdeationExhausted".into()};
        if !state.terminal{events.emit("cap_hit",Some("investigation"),json!({"cap":"max_idea_generation_attempts","limit":task::number(&cfg,"investigation.execution.max_idea_generation_attempts",2),"used":state.attempt,"step":cycle_key(&active)?.child("idea.check")?}))?;}
        outcomes.1.lock().map_err(|e|EvalError(e.to_string()))?.insert(active.worker,reason.clone());
        files::write_json(&lane_root(&run,&cfg,active.worker).join("lane_failure.json"),&json!({"reason":reason,"worker":active.worker,"lease":active.lease}))?;
        Ok(Verdict::Fail(reason))
    }.await;
    result.map_err(Into::into)
}
fn cycle_memo<T>(body: Graph, suffix: &str, program: bool) -> Graph
where
    T: Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static,
{
    let mut graph = Graph::new();
    let suffix = suffix.to_owned();
    graph.add_system(flow::bind::<
        (Out<CycleLease>, Res<CandidateCounter>),
        StepKey,
        _,
    >("cycle.step_key", move |(active, count), _| {
        let suffix = suffix.clone();
        Box::pin(async move {
            let prefix = cycle_key(&active)?;
            Ok(if program {
                prefix.child(&format!("a{}/{suffix}", count.index))?
            } else {
                prefix.child(&suffix)?
            })
        })
    }));
    graph
        .append(flow::memo_current::<T>(body))
        .expect("memoized research step");
    graph
}
#[system]
async fn lane_outcome(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    active: Out<CycleLease>,
    best: Res<LaneBest>,
    outcomes: Res<LaneOutcomes>,
) -> Result<Verdict, SystemError> {
    let result: Result<Verdict, EvalError> = async move {
        if let Some(best) = &best.best {
            outcomes
                .0
                .lock()
                .map_err(|e| EvalError(e.to_string()))?
                .insert(active.worker, Ok(best.clone()));
        }
        outcomes
            .1
            .lock()
            .map_err(|e| EvalError(e.to_string()))?
            .remove(&active.worker);
        let failure = lane_root(&run, &cfg, active.worker).join("lane_failure.json");
        if failure.exists() {
            fs::remove_file(failure)?;
        }
        Ok(Verdict::Pass)
    }
    .await;
    result.map_err(Into::into)
}
/// Compose investigation as shared leases around idea and candidate repair loops.
pub fn build(mut graph: Graph, ctx: &PhaseCtx) -> Result<Graph, BuildError> {
    graph.system(inv_open).named("inv.open").done();
    let mut idea_check_graph = Graph::new();
    idea_check_graph
        .system(idea_call)
        .named("idea.write.call")
        .done();
    idea_check_graph.append(flow::agent("idea.write", "ideation"))?;
    idea_check_graph
        .system(idea_check)
        .named("idea.check")
        .done();
    let mut candidate_graph = Graph::new();
    candidate_graph
        .system(program_call)
        .named("prog.edit.call")
        .done();
    candidate_graph.append(flow::agent("prog.edit", "program_author"))?;
    candidate_graph
        .system(program_eval)
        .named("prog.eval")
        .done();
    let mut rubric = Graph::new();
    rubric.system(rubric_check).named("rubric.check").done();
    candidate_graph.append(cycle_memo::<Assessed>(rubric, "rubric.check", true))?;
    if task::flag(&ctx.config, "investigation.execution.rubric_judge", false) {
        candidate_graph.add_conditional_branch::<Assessed, _, _, _>(
            "rubric.semantic",
            |a| a.correct && a.report.rubrics.iter().any(|r| r.status == "not_assessed"),
            |g| {
                g.system(rubric_judge_call)
                    .named("rubric.judge.call")
                    .done();
                g.append(flow::agent("rubric.judge", "rubric_verifier"))
                    .expect("rubric judge block");
                g.system(rubric_judge_check)
                    .named("rubric.judge.check")
                    .done();
            },
            |g| {
                g.add_system(|| async {});
            },
        );
    }
    candidate_graph
        .system(rubric_verdict)
        .named("rubric.decision")
        .done();
    graph.append(flow::lease_loop(
        "investigation.lanes",
        task::number(&ctx.config, "investigation.execution.workers", 1) as usize,
        move |worker| {
            let ideas = flow::repair_loop(
                "idea.retry",
                &format!("investigation.w{worker}.idea"),
                idea_check_graph.duplicate(),
                Graph::new(),
            );
            let programs = flow::repair_loop(
                "attempt.retry",
                &format!("investigation.w{worker}.program"),
                candidate_graph.duplicate(),
                Graph::new(),
            );
            let mut lane = Graph::new();
            lane.add_system(flow::bind::<Res<flow::LeaseBook>, CycleLease, _>(
                "cycle.lease",
                move |book, _| {
                    Box::pin(async move {
                        Ok(CycleLease {
                            worker,
                            lease: book.take(worker)?,
                        })
                    })
                },
            ));
            lane.system(cycle_prune).named("cycle.prune").done();
            lane.system(memory_pull).named("memory.pull").done();
            lane.append(ideas).expect("idea subgraph");
            lane.system(idea_ready).named("idea.ready").done();
            lane.add_conditional_branch::<IdeaAccepted, _, _, _>(
                "idea.accepted",
                |accepted| accepted.0,
                move |accepted| {
                    let mut canon = Graph::new();
                    canon.system(idea_canon).named("idea.canon").done();
                    accepted
                        .append(cycle_memo::<CanonicalIdea>(canon, "idea.canon", false))
                        .expect("canonical idea memo");
                    accepted.append(programs).expect("candidate subgraph");
                    let mut promotion = Graph::new();
                    promotion.system(best_promote).named("best.promote").done();
                    accepted
                        .append(cycle_memo::<Promotion>(promotion, "best.promote", false))
                        .expect("promotion memo");
                    accepted.add_conditional_branch::<Assessed, _, _, _>(
                        "attempt.required",
                        |assessed| !assessed.required_failure,
                        |completed| {
                            let mut record = Graph::new();
                            record.system(attempt_record).named("attempt.record").done();
                            completed
                                .append(cycle_memo::<()>(record, "attempt.record", false))
                                .expect("attempt record memo");
                            let mut check = Graph::new();
                            check.system(memory_check).named("memory.check").done();
                            completed
                                .append(cycle_memo::<()>(check, "memory.check", false))
                                .expect("memory check memo");
                            let mut publish = Graph::new();
                            publish.system(memory_push).named("memory.push").done();
                            completed
                                .append(cycle_memo::<Verdict>(publish, "memory.push", false))
                                .expect("memory publication memo");
                            completed.system(lane_outcome).named("lane.outcome").done();
                        },
                        |failed| {
                            failed.system(attempt_record).named("attempt.record").done();
                        },
                    );
                },
                |failed| {
                    failed
                        .system(idea_exhausted)
                        .named("IdeationExhausted")
                        .done();
                },
            );
            lane.add_error_handler(|g| {
                g.system(lane_failed).named("lane.failed").done();
            });
            let mut cycle = Graph::new();
            cycle.add_scope(
                "investigation.cycle",
                lane,
                polaris::graph::ContextPolicy::new()
                    .share_rest()
                    .forward_fresh::<IdeaState>()
                    .forward_fresh::<CandidateCounter>()
                    .forward_fresh::<Evaluations>()
                    .forward_fresh::<LaneBest>(),
            );
            cycle
        },
    ))?;
    graph.system(inv_join).named("inv.join").done();
    graph.system(inv_close).named("inv.close").done();
    Ok(graph)
}
