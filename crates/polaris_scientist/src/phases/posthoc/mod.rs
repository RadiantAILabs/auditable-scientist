//! Artifact-only post-hoc analyses in isolated, bounded, sequential workspaces.
pub mod filter;
use crate::{
    config::ScientistConfig,
    eval::{self, EvalError},
    phases::{
        BuildError, PhaseCtx, PhaseRegistry, RegisterError,
        ablations::artifacts,
        investigation::calls,
        task::{self, files},
    },
};
use polaris::{
    graph::Graph,
    system::{
        param::{Out, Res, ResMut},
        resource::LocalResource,
        system,
    },
};
use polaris_cosmos::flow::{
    self, AgentCall, AgentOutcome, CurrentItem, PhaseStatus, RepairCounters, RoleBook, RunHandle,
    StepKey, Verdict, WorkItem, WorkItemStatus, Worklist,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::PathBuf};
#[derive(Default)]
struct AnalysisPlan {
    bundle: PathBuf,
    digest: String,
    candidates: Vec<Value>,
    accepted: Vec<Value>,
    rejected: Vec<Value>,
}
impl LocalResource for AnalysisPlan {}
#[derive(Default)]
struct AnalysisCursor {
    attempt: u32,
    valid: bool,
    terminal: bool,
    feedback: String,
    source: PathBuf,
    output: PathBuf,
}
impl LocalResource for AnalysisCursor {}
#[derive(Default)]
struct AnalysisResults(Vec<Value>);
impl LocalResource for AnalysisResults {}
fn item(current: &CurrentItem) -> Result<&WorkItem, EvalError> {
    current
        .item()
        .ok_or_else(|| EvalError("analysis body has no item".into()))
}
fn id(spec: &Value) -> Result<&str, EvalError> {
    spec.get("id")
        .or_else(|| spec.get("analysis_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| EvalError("analysis id missing".into()))
}
#[system]
async fn ph_extract(
    run: Res<RunHandle>,
    mut plan: ResMut<AnalysisPlan>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let extracted = artifacts::extract(&run.root, "posthoc")?;
        let ready = run.root.join("posthoc/_analysis_ready_bundle");
        if !ready.exists() {
            files::copy_tree(&extracted.join("program"), &ready.join("program"))?;
            files::copy_tree(&run.root.join("task/task.md"), &ready.join("task/task.md"))?;
        }
        plan.bundle = ready;
        plan.digest = eval::fingerprint(&plan.bundle)?;
        let receipt = run.root.join("posthoc/frozen_bundle.json");
        if receipt.exists() && calls::read_json(&receipt)?["sha256"].as_str() != Some(&plan.digest)
        {
            return Err(EvalError("frozen post-hoc bundle digest changed".into()));
        }
        files::write_json(
            &run.root.join("posthoc/frozen_bundle.json"),
            &json!({"sha256":plan.digest}),
        )?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn bootstrap_skip(run: Res<RunHandle>) -> Result<(), SystemError> {
    let result:Result<(),EvalError> = async move {files::write_json(&run.root.join("posthoc/bootstrap_report.json"),&json!({"status":"skipped","checkpoint_mode":"optional","retrain_if_checkpoint_missing":false,"bootstrap_ready":false,"reason":"analysis uses retained artifacts; checkpoint bootstrap is not built"}))?;Ok(())
 }.await;
    result.map_err(Into::into)
}
fn evidence_files(root: &std::path::Path) -> Result<Vec<String>, EvalError> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        out: &mut Vec<String>,
    ) -> Result<(), EvalError> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(EvalError("symlink in evidence".into()));
            }
            if kind.is_dir() {
                visit(root, &entry.path(), out)?;
            } else {
                out.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .map_err(|e| EvalError(e.to_string()))?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(root, root, &mut out)?;
    out.sort();
    Ok(out)
}
#[system]
async fn propose_call(
    run: Res<RunHandle>,
    roles: Res<RoleBook>,
    plan: Res<AnalysisPlan>,
) -> Result<AgentCall, SystemError> {
    let result: Result<AgentCall, EvalError> = async move {
        let mut vars = calls::task_vars(&run.root);
        vars.extend(BTreeMap::from([
            (
                "EXTRACTION_MANIFEST".into(),
                calls::context(
                    &run.root
                        .join("posthoc/_extracted_bundle/extraction_manifest.json"),
                ),
            ),
            (
                "EVIDENCE_MANIFEST".into(),
                serde_json::to_string(&evidence_files(&plan.bundle)?)?,
            ),
            (
                "BOOTSTRAP_REPORT".into(),
                calls::context(&run.root.join("posthoc/bootstrap_report.json")),
            ),
            (
                "RESULTS_JSON".into(),
                calls::context(&plan.bundle.join("program/results/metrics.json")),
            ),
            (
                "EVALUATOR_SOURCE".into(),
                calls::context(&run.root.join("task/evaluate.py")),
            ),
        ]));
        calls::call(
            &run,
            &roles,
            "posthoc",
            StepKey::new("posthoc/ph.propose")?,
            "posthoc_proposer",
            vec![(plan.bundle.clone(), "bundle".into())],
            vars,
            vec!["outputs/posthoc_bundle.json".into()],
            "Propose analyses using existing artifacts only; write outputs/posthoc_bundle.json.",
        )
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn propose_check(
    run: Res<RunHandle>,
    call: Out<AgentCall>,
    mut plan: ResMut<AnalysisPlan>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let source = calls::workspace(&run, &call.step).join("outputs/posthoc_bundle.json");
        plan.candidates = artifacts::proposals(&source, "analyses")?;
        if source.exists() {
            files::copy_tree(
                &source,
                &run.root.join("posthoc/generation/posthoc_bundle.json"),
            )?;
        }
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn select_call(
    run: Res<RunHandle>,
    roles: Res<RoleBook>,
    plan: Res<AnalysisPlan>,
) -> Result<AgentCall, SystemError> {
    let result: Result<AgentCall, EvalError> = async move {
        let mut vars = calls::task_vars(&run.root);
        vars.extend(BTreeMap::from([
            (
                "BOOTSTRAP_REPORT".into(),
                calls::context(&run.root.join("posthoc/bootstrap_report.json")),
            ),
            (
                "CANDIDATES_JSON".into(),
                serde_json::to_string(&plan.candidates)?,
            ),
        ]));
        calls::call(
            &run,
            &roles,
            "posthoc",
            StepKey::new("posthoc/ph.select")?,
            "posthoc_selector",
            vec![(plan.bundle.clone(), "bundle".into())],
            vars,
            vec!["outputs/posthoc_selection.json".into()],
            "Select feasible artifact-only analyses; write outputs/posthoc_selection.json.",
        )
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn select_filter(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    mut plan: ResMut<AnalysisPlan>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let selection = if calls::finished(&outcome) {
            calls::read_json(
                &calls::workspace(&run, &call.step).join("outputs/posthoc_selection.json"),
            )
            .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let (accepted, rejected) = filter::select(
            &plan.candidates,
            &selection,
            &plan.bundle,
            task::number(&cfg, "posthoc.execution.max_selected_analyses", 3) as usize,
        );
        plan.accepted = accepted;
        plan.rejected = rejected;
        files::write_json(
            &run.root.join("posthoc/generation/posthoc_selection.json"),
            &json!({"accepted":plan.accepted,"rejected":plan.rejected}),
        )?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn materialize(
    run: Res<RunHandle>,
    plan: Res<AnalysisPlan>,
    worklist: Res<Worklist>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let mut items = Vec::new();
        for spec in &plan.accepted {
            let identifier = id(spec)?;
            let directory = run.root.join("posthoc/analyses").join(identifier);
            let contract = filter::contract(spec);
            files::write_json(&directory.join("analysis_spec.json"), spec)?;
            files::write_json(&directory.join("rubric_contract.json"), &contract)?;
            files::write_json(&directory.join("target_rubrics.json"), &contract["rubrics"])?;
            items.push(WorkItem {
                key: StepKey::new(format!("posthoc/analyses/{identifier}"))?,
                value: spec.clone(),
                status: WorkItemStatus::Pending,
                note: String::new(),
            });
        }
        worklist.replace(items)?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn analysis_open(
    cfg: Res<ScientistConfig>,
    mut cursor: ResMut<AnalysisCursor>,
    counters: Res<RepairCounters>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        *cursor = AnalysisCursor::default();
        counters
            .reset(
                "posthoc.attempts",
                task::number(&cfg, "posthoc.execution.max_attempts_per_analysis", 3)
                    .saturating_sub(1) as u32,
            )
            .map_err(|e| EvalError(e.to_string()))?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn analysis_fresh(
    run: Res<RunHandle>,
    current: Out<CurrentItem>,
    plan: Res<AnalysisPlan>,
    mut cursor: ResMut<AnalysisCursor>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        cursor.attempt += 1;
        let source = run
            .root
            .join(item(&current)?.key.as_str())
            .join(format!("attempt_{}/source", cursor.attempt));
        filter::fresh(&plan.bundle, &plan.digest, &source)?;
        cursor.source = source;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn analyst_call(
    run: Res<RunHandle>,
    roles: Res<RoleBook>,
    current: Out<CurrentItem>,
    cursor: Res<AnalysisCursor>,
) -> Result<AgentCall, SystemError> {
    let result:Result<AgentCall,EvalError>=async move {
        let item=item(&current)?;
        let mut vars=calls::task_vars(&run.root);
        vars.extend(BTreeMap::from([
            ("ANALYSIS_SPEC".into(),serde_json::to_string_pretty(&item.value)?),
            ("RUBRIC_CONTRACT".into(),serde_json::to_string_pretty(&filter::contract(&item.value))?),
            ("ATTEMPT_NUMBER".into(),cursor.attempt.to_string()),
            ("ERROR_CONTEXT".into(),cursor.feedback.clone())]));
        let mut expected=vec!["outputs/analysis_summary.md".into(),"outputs/interestingness_rubric.json".into(),"outputs/analysis_manifest.json".into(),"outputs/rubric_status.json".into()];
        if let Some(outputs)=item.value.get("expected_outputs").and_then(Value::as_array){expected.extend(outputs.iter().filter_map(Value::as_str).map(|p|format!("outputs/{p}")));}
        let mut inputs=vec![(cursor.source.clone(),"bundle".into())];
        for name in ["analysis_spec.json","rubric_contract.json","target_rubrics.json"] {
            inputs.push((run.root.join(item.key.as_str()).join(name),name.into()));
        }
        // The role prompt holds the analysis spec and the rubric contract, so the
        // agent goes to the evidence without a reading pass over the inputs.
        calls::call_briefed(&run,&roles,"posthoc",item.key.child(&format!("attempt_{}/analysis.write",cursor.attempt))?,"posthoc_analyst",inputs,vars,expected,"Use inputs/bundle as the frozen evidence. Write the analysis and every declared output under outputs/. Do not retrain, run the evaluator, or create variants.",calls::Briefing::Inline)
    }.await;
    result.map_err(Into::into)
}
#[system]
async fn analysis_check(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    current: Out<CurrentItem>,
    plan: Res<AnalysisPlan>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    mut cursor: ResMut<AnalysisCursor>,
) -> Result<Verdict, SystemError> {
    let result: Result<Verdict, EvalError> = async move {
        let work = calls::workspace(&run, &call.step);
        cursor.output = work.join("outputs");
        if calls::timed_out(&outcome) || matches!(&*outcome,AgentOutcome::Ran{report,..}if report.stop==flow::StopCause::Interrupted) {
            cursor.terminal = true;
            cursor.feedback = "analysis timed out".into();
            return Ok(Verdict::Pass);
        }
        if matches!(&*outcome, AgentOutcome::Skipped(_)) {
            cursor.terminal = true;
            cursor.feedback = "required analysis reservation refused".into();
            return Ok(Verdict::Pass);
        }
        let result = (|| {
            if eval::fingerprint(&plan.bundle)? != plan.digest {
                return Err(EvalError("frozen bundle changed".into()));
            }
            filter::validate(
                &work.join("inputs/bundle"),
                &plan.digest,
                &cursor.output,
                &item(&current)?.value,
                cursor.attempt,
                task::config_value(&cfg, "posthoc.execution.primary_metric_tolerance")
                    .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|n| n as f64)))
                    .unwrap_or(0.01),
            )?;
            for file in evidence_files(&cursor.output)? {
                if file.split('/').any(|p| {
                    p.starts_with("program_") && p[8..].chars().all(|c| c.is_ascii_digit())
                }) {
                    return Err(EvalError("analysis created a new program variant".into()));
                }
            }
            Ok::<(), EvalError>(())
        })();
        match result {
            Ok(()) => {
                cursor.valid = true;
                Ok(Verdict::Pass)
            }
            Err(error) => {
                cursor.feedback = error.to_string();
                Ok(Verdict::Fail(cursor.feedback.clone()))
            }
        }
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn analysis_finish(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    events: Res<flow::EventLog>,
    current: Out<CurrentItem>,
    cursor: Res<AnalysisCursor>,
    mut results: ResMut<AnalysisResults>,
) -> Result<Verdict, SystemError> {
    let result:Result<Verdict,EvalError> = async move {let item=item(&current)?;if !cursor.valid&&!cursor.terminal&&u64::from(cursor.attempt)>=task::number(&cfg,"posthoc.execution.max_attempts_per_analysis",3){events.emit("cap_hit",Some("posthoc"),json!({"cap":"max_attempts_per_analysis","limit":task::number(&cfg,"posthoc.execution.max_attempts_per_analysis",3),"used":cursor.attempt,"step":item.key}))?;}let result=json!({"id":id(&item.value)?,"status":if cursor.valid{"valid"}else{"rejected"},"attempts":cursor.attempt,"reason":cursor.feedback,"rubric_status":calls::read_json(&cursor.output.join("rubric_status.json")).unwrap_or(Value::Null),"outputs":if cursor.valid{evidence_files(&cursor.output)?}else{Vec::<String>::new()},"output_dir":cursor.output});files::write_json(&run.root.join(item.key.as_str()).join("analysis_result.json"),&result)?;results.0.push(result);Ok(if cursor.valid{Verdict::Pass}else{Verdict::Fail(cursor.feedback.clone())})
 }.await;
    result.map_err(Into::into)
}
#[system]
async fn ph_publish(
    run: Res<RunHandle>,
    plan: Res<AnalysisPlan>,
    results: Res<AnalysisResults>,
    worklist: Res<Worklist>,
) -> Result<PhaseStatus, SystemError> {
    let result:Result<PhaseStatus,EvalError> = async move {
    let mut analyses=results.0.clone();for item in worklist.items(){if !analyses.iter().any(|a|a["id"]==id(&item.value).unwrap_or("")){let previous=calls::read_json(&run.root.join(item.key.as_str()).join("analysis_result.json")).unwrap_or_else(|_|json!({"id":id(&item.value).unwrap_or(""),"status":"rejected","reason":item.note,"outputs":[]}));analyses.push(previous);}}
    let valid=analyses.iter().filter(|a|a["status"]=="valid").count();let latest=json!({"analyses":analyses,"rejected":plan.rejected,"selected_analysis_id":null,"delegated_to_paper_agent":true});
    files::write_json(&run.root.join("posthoc/latest_manifest.json"),&latest)?;files::write_json(&run.root.join("posthoc/ranking.json"),&json!({"selected_analysis_id":null,"delegated_to_paper_agent":true,"analyses":analyses.iter().filter(|a|a["status"]=="valid").map(|a|&a["id"]).collect::<Vec<_>>()}))?;files::write_json(&run.root.join("posthoc/integration_status.json"),&json!({"status":if valid>0{"completed"}else{"degraded"},"valid_analyses":valid}))?;
    Ok(if valid>0{PhaseStatus::Completed}else{PhaseStatus::Degraded("no valid post-hoc analysis".into())})

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn analysis_failed(
    run: Res<RunHandle>,
    current: Out<CurrentItem>,
    cursor: Res<AnalysisCursor>,
    error: polaris::system::param::ErrOut<polaris::graph::CaughtError>,
    mut results: ResMut<AnalysisResults>,
) -> Verdict {
    let reason = error.to_string();
    if let Ok(item) = item(&current) {
        let result = json!({"id":id(&item.value).unwrap_or("unknown"),"status":"rejected","attempts":cursor.attempt,"reason":reason,"outputs":[]});
        let _ = files::write_json(
            &run.root
                .join(item.key.as_str())
                .join("analysis_result.json"),
            &result,
        );
        results.0.push(result);
    }
    Verdict::Fail(reason)
}
/// Register separate analysis plans, attempt state and result inventory.
pub fn register(reg: &mut PhaseRegistry) -> Result<(), RegisterError> {
    reg.local(AnalysisPlan::default);
    reg.local(AnalysisCursor::default);
    reg.local(AnalysisResults::default);
    Ok(())
}
/// Compose proposal, filtering and sequential pristine analysis attempts.
pub fn build(mut graph: Graph, _ctx: &PhaseCtx) -> Result<Graph, BuildError> {
    graph.system(ph_extract).named("ph.extract").done();
    graph
        .system(bootstrap_skip)
        .named("ph.bootstrap.skip")
        .done();
    graph.system(propose_call).named("ph.propose.call").done();
    graph.append(flow::agent("ph.propose", "posthoc_proposer"))?;
    graph.system(propose_check).named("ph.propose.check").done();
    graph.system(select_call).named("ph.select.call").done();
    graph.append(flow::agent("ph.select", "posthoc_selector"))?;
    graph.system(select_filter).named("ph.select.filter").done();
    graph.system(materialize).named("ph.materialize").done();
    let mut attempt = Graph::new();
    attempt
        .system(analysis_fresh)
        .named("analysis.fresh")
        .done();
    attempt
        .system(analyst_call)
        .named("analysis.write.call")
        .done();
    attempt.append(flow::agent("analysis.write", "posthoc_analyst"))?;
    attempt
        .system(analysis_check)
        .named("analysis.check")
        .done();
    let attempts = flow::repair_loop(
        "analysis.attempts",
        "posthoc.attempts",
        attempt,
        Graph::new(),
    );
    let mut body = Graph::new();
    body.system(analysis_open).named("analysis.open").done();
    body.append(attempts)?;
    body.system(analysis_finish).named("analysis.finish").done();
    body.add_error_handler(|g| {
        g.system(analysis_failed).named("analysis.failed").done();
    });
    let mut item_graph = Graph::new();
    item_graph.add_scope(
        "analysis.body",
        body,
        polaris::graph::ContextPolicy::shared(),
    );
    graph.append(flow::worklist("posthoc.analyses", item_graph))?;
    graph.system(ph_publish).named("ph.publish").done();
    Ok(graph)
}
