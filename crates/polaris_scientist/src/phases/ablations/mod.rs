//! Sequential ablation worklists with host-owned scoring and canonical verification.
pub mod artifacts;
pub mod contracts;
use crate::{
    config::ScientistConfig,
    eval::{self, EvalError},
    phases::{
        BuildError, PhaseCtx, PhaseRegistry, RegisterError,
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
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Default)]
struct VariantPlan {
    bundle: PathBuf,
    program: PathBuf,
    evaluator_digest: String,
    candidates: Vec<Value>,
    selection: Value,
}
impl LocalResource for VariantPlan {}
#[derive(Default)]
struct VariantCursor {
    index: u32,
    edit_attempt: u32,
    evals: u32,
    passed: bool,
    terminal: bool,
    reason: String,
    verify_attempt: u32,
}
impl LocalResource for VariantCursor {}
#[derive(Default)]
struct VariantResults(Vec<Value>);
impl LocalResource for VariantResults {}
#[derive(Clone)]
struct AuthoredCandidate {
    result: Result<PathBuf, String>,
}
#[derive(Clone)]
struct VariantEvaluation {
    result: Result<eval::EvalResult, String>,
    program: PathBuf,
}
fn item(current: &CurrentItem) -> Result<&WorkItem, EvalError> {
    current
        .item()
        .ok_or_else(|| EvalError("variant body has no work item".into()))
}
fn id(spec: &Value) -> Result<&str, EvalError> {
    spec.get("variant_id")
        .or_else(|| spec.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| EvalError("variant id missing".into()))
}
fn control(spec: &Value) -> bool {
    matches!(id(spec), Ok("control_best" | "baseline_reference"))
}
fn directory(run: &RunHandle, spec: &Value) -> Result<PathBuf, EvalError> {
    Ok(run.root.join("ablations/variants").join(id(spec)?))
}

#[system]
async fn abl_extract(
    run: Res<RunHandle>,
    mut plan: ResMut<VariantPlan>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        plan.bundle = artifacts::extract(&run.root, "ablations")?;
        plan.program = plan.bundle.join("program");
        plan.evaluator_digest = eval::file_digest(&run.root.join("task/evaluate.py"))?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn retrofit_call(
    run: Res<RunHandle>,
    roles: Res<RoleBook>,
    plan: Res<VariantPlan>,
) -> Result<AgentCall, SystemError> {
    let result:Result<AgentCall,EvalError> = async move {
    let mut vars = calls::task_vars(&run.root);
    vars.extend(BTreeMap::from([
        (
            "EXTRACTION_MANIFEST".into(),
            calls::context(&plan.bundle.join("extraction_manifest.json")),
        ),
        (
            "EVALUATOR_SOURCE".into(),
            calls::context(&run.root.join("task/evaluate.py")),
        ),
        (
            "PROGRAM_SOURCE".into(),
            calls::context(&eval::entrypoint(&plan.program)?),
        ),
        (
            "REFERENCE_EVALUATOR_SOURCE".into(),
            calls::context(&run.root.join("task/evaluate.py")),
        ),
        (
            "CSV_SCHEMAS".into(),
            contracts::csv_schemas(),
        ),
    ]));
    calls::call(
        &run,
        &roles,
        "ablations",
        StepKey::new("ablations/abl.retrofit")?,
        "variant_retrofit",
        vec![(plan.bundle.clone(), "bundle".into())],
        vars,
        vec!["outputs/program_0/program_spec.toml".into()],
        "Copy inputs/bundle/program to outputs/program_0 and add analysis-only instrumentation. Do not modify the scorer or objective. For edit_file always supply path, old_string, new_string and replace_all=false. For read_span supply integer start and end line numbers. You may choose test_candidate on outputs/program_0 to check the instrumentation. Inspect its analysis_checks and correct any missing CSV output before finishing. The host evaluates control_best after this step.",
    )

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn retrofit_check(
    run: Res<RunHandle>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    mut plan: ResMut<VariantPlan>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let workspace = calls::workspace(&run, &call.step);
        let candidate = workspace.join("outputs/program_0");
        let destination = run.root.join("ablations/_retrofitted_bundle");
        let copied = matches!(&*outcome, AgentOutcome::Skipped(_));
        if !copied {
            calls::required(&outcome)?;
        }
        let source = if copied { &plan.program } else { &candidate };
        let mut manifest = artifacts::check_retrofit(
            &run.root.join("task/evaluate.py"),
            &plan.evaluator_digest,
            &workspace.join("inputs/bundle"),
            source,
        )?;
        manifest["mode"] = json!(if copied { "copied" } else { "agent" });
        manifest["original_program_sha256"] = json!(eval::fingerprint(&plan.program)?);
        manifest["note"] = json!(if copied {
            "role reservation unavailable; winner copied unchanged"
        } else {
            "analysis instrumentation; control evaluation checks execution"
        });
        if !destination.exists() {
            files::copy_editable_tree(source, &destination)?;
            artifacts::remove_results(&destination)?;
        }
        plan.program = destination;
        files::write_json(
            &run.root.join("ablations/retrofit_manifest.json"),
            &manifest,
        )?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn propose_call(
    run: Res<RunHandle>,
    roles: Res<RoleBook>,
    plan: Res<VariantPlan>,
) -> Result<AgentCall, SystemError> {
    let result: Result<AgentCall, EvalError> = async move {
        let mut vars = calls::task_vars(&run.root);
        vars.extend(BTreeMap::from([
            (
                "EXTRACTION_MANIFEST".into(),
                calls::context(&plan.bundle.join("extraction_manifest.json")),
            ),
            (
                "PROGRAM_DIFF".into(),
                calls::context(&plan.bundle.join("program.diff")),
            ),
            (
                "EVALUATOR_SUMMARY".into(),
                calls::context(&run.root.join("task/evaluate.py")),
            ),
        ]));
        calls::call(
            &run,
            &roles,
            "ablations",
            StepKey::new("ablations/abl.propose")?,
            "variant_propose",
            vec![(plan.bundle.clone(), "bundle".into())],
            vars,
            vec!["outputs/variant_bundle.json".into()],
            "Propose controlled variants; write outputs/variant_bundle.json.",
        )
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn propose_check(
    run: Res<RunHandle>,
    call: Out<AgentCall>,
    mut plan: ResMut<VariantPlan>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let path = calls::workspace(&run, &call.step).join("outputs/variant_bundle.json");
        plan.candidates = artifacts::proposals(&path, "candidates")?;
        if path.exists() {
            files::copy_tree(
                &path,
                &run.root.join("ablations/generation/variant_bundle.json"),
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
    plan: Res<VariantPlan>,
) -> Result<AgentCall, SystemError> {
    let result: Result<AgentCall, EvalError> = async move {
        let mut vars = calls::task_vars(&run.root);
        vars.insert(
            "CANDIDATES_JSON".into(),
            serde_json::to_string(&plan.candidates)?,
        );
        vars.insert(
            "PROGRAM_DIFF".into(),
            calls::context(&plan.bundle.join("program.diff")),
        );
        calls::call(
            &run,
            &roles,
            "ablations",
            StepKey::new("ablations/abl.select")?,
            "variant_select",
            vec![(plan.bundle.join("program.diff"), "program.diff".into())],
            vars,
            vec!["outputs/variant_selection.json".into()],
            "Select feasible single-change variants; write outputs/variant_selection.json.",
        )
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn select_apply(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    mut plan: ResMut<VariantPlan>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        let path = calls::workspace(&run, &call.step).join("outputs/variant_selection.json");
        plan.selection = if calls::finished(&outcome) {
            calls::read_json(&path).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        plan.candidates = artifacts::selected(
            &plan.candidates,
            &plan.selection,
            task::number(&cfg, "ablations.execution.max_ablations", 3) as usize,
            task::number(&cfg, "ablations.execution.max_extra_baselines", 3) as usize,
        );
        files::write_json(
            &run.root.join("ablations/generation/variant_selection.json"),
            &plan.selection,
        )?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn materialize(
    run: Res<RunHandle>,
    plan: Res<VariantPlan>,
    worklist: Res<Worklist>,
) -> Result<(), SystemError> {
    let result:Result<(),EvalError> = async move {
    let mut candidates = vec![
        json!({"variant_id":"control_best","kind":"control","title":"Unchanged winner"}),
        json!({"variant_id":"baseline_reference","kind":"baseline","title":"Baseline (reference / seed program)"}),
    ];
    candidates.extend(plan.candidates.clone());
    let mut items = Vec::new();
    for spec in candidates {
        let source = if id(&spec)? == "baseline_reference" {
            plan.bundle.join("reference_program")
        } else {
            plan.program.clone()
        };
        artifacts::materialize(&run.root, &spec, &source)?;
        items.push(WorkItem {
            key: StepKey::new(format!("ablations/variants/{}", id(&spec)?))?,
            value: spec,
            status: WorkItemStatus::Pending,
            note: String::new(),
        });
    }
    worklist
        .replace(items)
        .map_err(|e| EvalError(e.to_string()))?;
    Ok(())

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn variant_open(
    cfg: Res<ScientistConfig>,
    mut cursor: ResMut<VariantCursor>,
    counters: Res<RepairCounters>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        *cursor = VariantCursor::default();
        counters
            .reset(
                "ablations.evals",
                task::number(&cfg, "ablations.execution.max_evals_per_variant", 5).saturating_sub(1)
                    as u32,
            )
            .map_err(|e| EvalError(e.to_string()))?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn author_call(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    roles: Res<RoleBook>,
    current: Out<CurrentItem>,
    mut cursor: ResMut<VariantCursor>,
) -> Result<AgentCall, SystemError> {
    let result: Result<AgentCall, EvalError> = async move {
        let item = item(&current)?;
        cursor.edit_attempt += 1;
        let dir = directory(&run, &item.value)?;
        let seed = dir.join(format!("task_bundle/program_{}", cursor.index - 1));
        let mut vars = calls::task_vars(&run.root);
        vars.extend(BTreeMap::from([
            (
                "VARIANT_SPEC".into(),
                calls::context(&dir.join("variant_spec.json")),
            ),
            (
                "MAX_PROGRAM_EVALS_PER_ATTEMPT".into(),
                task::number(&cfg, "ablations.execution.max_evals_per_variant", 5).to_string(),
            ),
            (
                "ATTEMPT_ID".into(),
                format!("{}_e{}", id(&item.value)?, cursor.index),
            ),
            ("IDEA_ID".into(), id(&item.value)?.into()),
            ("CYCLE_INDEX".into(), cursor.index.to_string()),
            ("WORKER_INDEX".into(), "0".into()),
            ("PROGRAM_INDEX".into(), cursor.index.to_string()),
            ("PREVIOUS_INDEX".into(), (cursor.index - 1).to_string()),
            ("EDIT_ATTEMPT".into(), cursor.edit_attempt.to_string()),
            ("PREVIOUS_STATUS".into(), cursor.reason.clone()),
            ("SEED_PROGRAM_DIR".into(), "inputs/program_0".into()),
        ]));
        calls::call(
            &run,
            &roles,
            "ablations",
            item.key.child(&format!(
                "e{}/edit{}/variant.edit",
                cursor.index, cursor.edit_attempt
            ))?,
            "variant_author",
            vec![
                (seed, "program_0".into()),
                (dir.join("variant_spec.json"), "variant_spec.json".into()),
                (
                    dir.join("target_rubrics.json"),
                    "target_rubrics.json".into(),
                ),
            ],
            vars,
            vec![format!(
                "outputs/program_{}/program_spec.toml",
                cursor.index
            )],
            &format!(
                "Author outputs/program_{} from inputs/program_0. You may choose test_candidate before finishing.",
                cursor.index
            ),
        )
    }
    .await;
    result.map_err(Into::into)
}
async fn score(
    run: &RunHandle,
    cfg: &ScientistConfig,
    spec: &Value,
    index: u32,
) -> Result<VariantEvaluation, EvalError> {
    let program = directory(run, spec)?.join(format!("task_bundle/program_{index}"));
    let mut request =
        task::evaluator_request(&run.root, cfg, program.clone(), program.join("results"))?;
    request.environment.insert(
        "NUM_REPLICATIONS".into(),
        task::number(cfg, "ablations.execution.num_replications", 5).to_string(),
    );
    request
        .environment
        .insert("VARIANT_ID".into(), id(spec)?.into());
    for (key, value) in [
        (
            "VARIANT_KIND",
            spec["kind"].as_str().unwrap_or("ablation").to_owned(),
        ),
        (
            "BASE_PROGRAM",
            spec["base_program"].as_str().unwrap_or("best").to_owned(),
        ),
        ("EVALUATION_ID", format!("{}_eval_{index}", id(spec)?)),
    ] {
        request.environment.insert(key.into(), value);
    }
    let result = eval::run_evaluator(request)
        .await
        .map_err(|e| e.to_string());
    Ok(VariantEvaluation { result, program })
}
#[system]
async fn control_eval(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    current: Out<CurrentItem>,
) -> Result<VariantEvaluation, SystemError> {
    let result: Result<VariantEvaluation, EvalError> =
        async move { score(&run, &cfg, &item(&current)?.value, 0).await }.await;
    result.map_err(Into::into)
}
#[system]
async fn author_open(
    mut cursor: ResMut<VariantCursor>,
    counters: Res<RepairCounters>,
) -> Result<(), SystemError> {
    cursor.index += 1;
    cursor.edit_attempt = 0;
    counters
        .reset("ablations.edit", 2)
        .map_err(|e| EvalError(e.to_string()))?;
    Ok(())
}
#[system]
async fn author_check(
    run: Res<RunHandle>,
    current: Out<CurrentItem>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    mut cursor: ResMut<VariantCursor>,
) -> AuthoredCandidate {
    let checked = (|| -> Result<PathBuf, EvalError> {
        calls::required(&outcome)?;
        if !calls::finished(&outcome) {
            return Err(EvalError(format!(
                "variant author did not finish successfully: {:?}",
                *outcome
            )));
        }
        let dir = directory(&run, &item(&current)?.value)?;
        let output =
            calls::workspace(&run, &call.step).join(format!("outputs/program_{}", cursor.index));
        // A clone may contain a previous candidate's scorer outputs. The host
        // must produce fresh records for this implementation, never adopt them.
        artifacts::remove_results(&output)?;
        contracts::require_source_change(&dir.join("task_bundle/program_0"), &output)?;
        Ok(output)
    })();
    let result = checked.map_err(|e| e.to_string());
    if let Err(reason) = &result {
        cursor.reason = reason.clone();
    }
    AuthoredCandidate { result }
}
#[system]
async fn author_verdict(authored: Out<AuthoredCandidate>) -> Verdict {
    match &authored.result {
        Ok(_) => Verdict::Pass,
        Err(reason) => Verdict::Fail(reason.clone()),
    }
}
#[system]
async fn variant_eval(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    current: Out<CurrentItem>,
    mut cursor: ResMut<VariantCursor>,
    authored: Out<AuthoredCandidate>,
) -> Result<VariantEvaluation, SystemError> {
    let result: Result<VariantEvaluation, EvalError> = async move {
        let program = directory(&run, &item(&current)?.value)?
            .join(format!("task_bundle/program_{}", cursor.index));
        let output = match &authored.result {
            Ok(output) => output,
            Err(reason) => {
                return Ok(VariantEvaluation {
                    result: Err(reason.clone()),
                    program,
                });
            }
        };
        if !program.exists() {
            files::copy_editable_tree(output, &program)?;
            // A repair starts from the evaluated previous candidate, so the copy can carry that
            // candidate's evaluator files. They describe other source and would be refused as a
            // fingerprint mismatch, leaving the repair unscored.
            artifacts::remove_results(&program)?;
        }
        // Validate the persisted candidate too, including on resume.
        contracts::require_source_change(
            &directory(&run, &item(&current)?.value)?.join("task_bundle/program_0"),
            &program,
        )?;
        cursor.evals += 1;
        score(&run, &cfg, &item(&current)?.value, cursor.index).await
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn verifier_open(
    mut cursor: ResMut<VariantCursor>,
    counters: Res<RepairCounters>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        cursor.verify_attempt = 0;
        counters
            .reset("ablations.verify", 2)
            .map_err(|e| EvalError(e.to_string()))?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn verify_call(
    run: Res<RunHandle>,
    roles: Res<RoleBook>,
    current: Out<CurrentItem>,
    evaluation: Out<VariantEvaluation>,
    mut cursor: ResMut<VariantCursor>,
) -> Result<AgentCall, SystemError> {
    let result:Result<AgentCall,EvalError> = async move {
    cursor.verify_attempt += 1;
    let item = item(&current)?;
    let targets = calls::read_json(&directory(&run, &item.value)?.join("target_rubrics.json"))?;
    let mut vars = calls::task_vars(&run.root);
    vars.extend(BTreeMap::from([
        ("RESULTS_DIR".into(), "inputs".into()),
        ("ATTEMPT_ID".into(), id(&item.value)?.into()),
        ("IDEA_ID".into(), id(&item.value)?.into()),
        (
            "MAX_PROGRAM_EVALS_PER_ATTEMPT".into(),
            cursor.index.to_string(),
        ),
    ]));
    let template=directory(&run,&item.value)?.join(format!("verifier_template_{}.json",cursor.index));
    let evaluated = evaluation.result.as_ref().map_err(|e|EvalError(e.clone()))?;
    let mechanical = contracts::mechanical(&evaluation.program, evaluated)?;
    files::write_json(&template,&json!({"variant_id":id(&item.value)?,"rubrics":targets.as_array().unwrap_or(&Vec::new()).iter().map(|r|mechanical.iter().find(|m|m["id"]==r["id"]).cloned().unwrap_or_else(||json!({"id":r["id"],"pass":false,"evidence":""}))).collect::<Vec<_>>()}))?;
    let mut inputs = vec![(
        evaluation.program.clone(),
        format!("program_{}", cursor.index),
    )];
    let targets_path = directory(&run, &item.value)?.join("target_rubrics.json");
    inputs.push((targets_path, "target_rubrics.json".into()));
    inputs.push((template,"rubric_status.json".into()));
    let dir = directory(&run, &item.value)?;
    inputs.push((dir.join("task_bundle/program_0"), "control_program".into()));
    inputs.push((dir.join("variant_spec.json"), "variant_spec.json".into()));
    inputs.push((dir.join("rubric_contract.json"), "rubric_contract.json".into()));
    calls::call(
        &run,
        &roles,
        "ablations",
        item.key.child(&format!(
            "e{}/verify_{}/variant.verify",
            cursor.index, cursor.verify_attempt
        ))?,
        "variant_verifier",
        inputs,
        vars,
        vec!["outputs/rubric_status.json".into()],
        &format!(
            "Inspect inputs/variant_spec.json, the full candidate source and inputs/control_program. Compare the source against the declared intent and comparison axis: variant_intent_preserved requires the declared intervention to be implemented; single_change_scope requires no unrelated changes. Cite source locations and the actual differences in evidence; identical scores cannot establish intent. Inspect host-produced metrics and analysis evidence. Copy inputs/rubric_status.json to outputs/rubric_status.json and preserve the six host-assessed file/schema/metrics/correctness checks. Judge only the remaining semantic rubrics, filling boolean pass and non-empty evidence per rubric. Required rubrics: {targets}. Previous host validation error: {}. Do not edit inputs.", cursor.reason
        ),
    )

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn verify_check(
    run: Res<RunHandle>,
    current: Out<CurrentItem>,
    evaluation: Out<VariantEvaluation>,
    call: Out<AgentCall>,
    outcome: Out<AgentOutcome>,
    mut cursor: ResMut<VariantCursor>,
) -> Result<Verdict, SystemError> {
    let result:Result<Verdict,EvalError> = async move {
    let item = item(&current)?;
    let targets = calls::read_json(&directory(&run, &item.value)?.join("target_rubrics.json"))?;
    let correct = evaluation
        .result
        .as_ref()
        .ok()
        .is_some_and(|r| r.score().is_some());
    let checked = (|| {
        calls::required(&outcome)?;
        if !calls::finished(&outcome) { return Err(EvalError("variant verifier did not finish successfully".into())); }
        let output_path = calls::workspace(&run, &call.step).join("outputs/rubric_status.json");
        if std::fs::metadata(&output_path)?.len() > 1024 * 1024 { return Err(EvalError("variant verifier output exceeds size limit".into())); }
        let output = calls::read_json(&output_path)?;
        let trusted = contracts::targets(&item.value);
        if targets != json!(trusted) { return Err(EvalError("variant contract differs from trusted rubric IDs".into())); }
        artifacts::verifier_pass(&trusted, &output, correct)?;
        let mut canonical = output["rubrics"].as_array().unwrap().iter().map(|r|json!({"id":r["id"],"pass":r["pass"],"evidence":r["evidence"].as_str().unwrap().trim()})).collect::<Vec<_>>();
        let evaluated = evaluation.result.as_ref().map_err(|e|EvalError(e.clone()))?;
        for check in contracts::mechanical(&evaluation.program, evaluated)? {
            let entry = canonical.iter_mut().find(|r|r["id"]==check["id"]).ok_or_else(||EvalError("mechanical rubric missing".into()))?;
            *entry = check;
        }
        canonical.sort_by_key(|r|trusted.iter().position(|t|t["id"]==r["id"]).unwrap_or(usize::MAX));
        let passed = artifacts::verifier_pass(&trusted, &json!({"rubrics":canonical}), correct)?;
        cursor.reason = canonical.iter().filter(|r|r["pass"]!=true).map(|r|format!("{}: {}",r["id"],r["evidence"])).collect::<Vec<_>>().join("\n");
        files::write_json(
            &evaluation.program.join("results/rubric_status.json"),
            &json!({"variant_id":id(&item.value)?,"attempt_id":format!("{}_e{}",id(&item.value)?,cursor.index),"idea_id":id(&item.value)?,"rubrics":canonical,"overall_pass":passed,"metrics":evaluation.result.as_ref().ok().map(|r|&r.metrics),"next_recommendation":output.get("next_recommendation").and_then(Value::as_str).unwrap_or("")}),
        )?;
        Ok::<bool, EvalError>(passed)
    })();
    match checked {
        Ok(passed) => {
            cursor.passed = passed;
            Ok(Verdict::Pass)
        }
        Err(error) => {
            cursor.reason = error.to_string();
            if calls::timed_out(&outcome) {
                cursor.terminal = true;
                Ok(Verdict::Pass)
            } else {
                Ok(Verdict::Fail(cursor.reason.clone()))
            }
        }
    }

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn variant_canon(
    cfg: Res<ScientistConfig>,
    current: Out<CurrentItem>,
    evaluation: Out<VariantEvaluation>,
    mut cursor: ResMut<VariantCursor>,
    events: Res<flow::EventLog>,
) -> Verdict {
    if control(&item(&current).expect("work item exists").value) {
        cursor.passed = evaluation
            .result
            .as_ref()
            .ok()
            .is_some_and(|r| r.score().is_some());
        cursor.terminal = true;
    }
    if let Err(error) = &evaluation.result {
        cursor.reason = error.clone();
        cursor.terminal = true;
    }
    if !cursor.passed
        && !cursor.terminal
        && u64::from(cursor.index)
            >= task::number(&cfg, "ablations.execution.max_evals_per_variant", 5)
    {
        let _=events.emit("cap_hit",Some("ablations"),json!({"cap":"max_evals_per_variant","limit":task::number(&cfg,"ablations.execution.max_evals_per_variant",5),"used":cursor.index,"step":current.item().map(|i|&i.key)}));
    }
    if cursor.passed
        || cursor.terminal
        || cursor.index >= task::number(&cfg, "ablations.execution.max_evals_per_variant", 5) as u32
    {
        Verdict::Pass
    } else {
        Verdict::Fail("variant rubrics failed; another candidate allowed".into())
    }
}
#[system]
async fn variant_finish(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    current: Out<CurrentItem>,
    evaluation: Out<VariantEvaluation>,
    cursor: Res<VariantCursor>,
    mut results: ResMut<VariantResults>,
) -> Result<Verdict, SystemError> {
    let result:Result<Verdict,EvalError> = async move {
    let item = item(&current)?;
    let result = json!({"id":id(&item.value)?,"kind":item.value["kind"],"status":if cursor.passed{"pass"}else{"fail"},"score":evaluation.result.as_ref().ok().and_then(|r|r.score()),"correct":evaluation.result.as_ref().ok().is_some_and(|r|r.score().is_some()),"evals":if control(&item.value){1}else{cursor.evals},"edit_attempts_last_candidate":cursor.edit_attempt,"reason":cursor.reason,"cap_hit":!cursor.passed&&!cursor.terminal&&u64::from(cursor.index)>=task::number(&cfg,"ablations.execution.max_evals_per_variant",5)});
    files::write_json(
        &directory(&run, &item.value)?.join("run_manifest.json"),
        &result,
    )?;
    results.0.push(result);
    Ok(if cursor.passed {
        Verdict::Pass
    } else {
        Verdict::Fail(cursor.reason.clone())
    })

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn abl_join(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    mut results: ResMut<VariantResults>,
    worklist: Res<Worklist>,
) -> Result<(), SystemError> {
    let result:Result<(),EvalError> = async move {
    for item in worklist.items(){
        let identifier=id(&item.value)?;
        if !results.0.iter().any(|r|r["id"]==identifier){
            let saved=calls::read_json(&directory(&run,&item.value)?.join("run_manifest.json")).unwrap_or_else(|_|json!({"id":identifier,"kind":item.value["kind"],"status":"fail","score":null,"correct":false,"evals":0,"reason":item.note}));
            results.0.push(saved);
        }
    }
    let controls_ok = ["control_best", "baseline_reference"].iter().all(|id| {
        results
            .0
            .iter()
            .any(|r| r["id"] == *id && r["status"] == "pass")
    });
    let summary = json!({"variants":results.0,"controls":results.0.iter().filter(|r|r["id"]=="control_best"||r["id"]=="baseline_reference").collect::<Vec<_>>(),"replications_requested":task::number(&cfg,"ablations.execution.num_replications",5),"replications_observed":1,"cross_task":"not_applicable","caps_hit":results.0.iter().filter(|v|v["cap_hit"]==true).map(|v|json!({"cap":"max_evals_per_variant","variant":v["id"],"used":v["evals"]})).collect::<Vec<_>>()});
    files::write_json(&run.root.join("ablations/pipeline_summary.json"), &summary)?;
    if !controls_ok || !results.0.iter().any(|r| r["status"] == "pass") {
        return Err(EvalError("required control failed or no variant passed".into()));
    }
    Ok(())

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn abl_aggregate(
    run: Res<RunHandle>,
    results: Res<VariantResults>,
) -> Result<(), SystemError> {
    let result: Result<(), EvalError> = async move {
        artifacts::aggregate(&run.root, &results.0)?;
        Ok(())
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn summarize_call(
    run: Res<RunHandle>,
    roles: Res<RoleBook>,
    results: Res<VariantResults>,
) -> Result<AgentCall, SystemError> {
    let result: Result<AgentCall, EvalError> = async move {
        let mut vars = calls::task_vars(&run.root);
        vars.insert("VARIANTS_JSON".into(), serde_json::to_string(&results.0)?);
        calls::call(
            &run,
            &roles,
            "ablations",
            StepKey::new("ablations/abl.summarize")?,
            "variant_summarizer",
            vec![(
                run.root.join("ablations/pipeline_summary.json"),
                "pipeline_summary.json".into(),
            )],
            vars,
            vec!["outputs/variant_summaries.json".into()],
            "Write concise evidence-grounded descriptions to outputs/variant_summaries.json.",
        )
    }
    .await;
    result.map_err(Into::into)
}
#[system]
async fn abl_readme(
    run: Res<RunHandle>,
    call: Out<AgentCall>,
    results: Res<VariantResults>,
) -> Result<PhaseStatus, SystemError> {
    let result:Result<PhaseStatus,EvalError> = async move {
    let descriptions = calls::read_json(
        &calls::workspace(&run, &call.step).join("outputs/variant_summaries.json"),
    )
    .unwrap_or_else(|_| json!({}));
    files::write_json(
        &run.root.join("ablations/variant_summaries.json"),
        &descriptions,
    )?;
    let mut readme = String::from(
        "# Controlled comparisons\n\nRequested replications are configuration; this task records one observation per evaluation.\n\n",
    );
    for result in &results.0 {
        readme.push_str(&format!(
            "- {}: {}; score {}.\n",
            result["id"], result["status"], result["score"]
        ));
    }
    files::atomic_write(&run.root.join("ablations/README.md"), readme.as_bytes())?;
    Ok(PhaseStatus::Completed)

 }.await;
    result.map_err(Into::into)
}
#[system]
async fn variant_failed(
    run: Res<RunHandle>,
    current: Out<CurrentItem>,
    cursor: Res<VariantCursor>,
    error: polaris::system::param::ErrOut<polaris::graph::CaughtError>,
    mut results: ResMut<VariantResults>,
) -> Verdict {
    let reason = error.to_string();
    if let Ok(item) = item(&current) {
        let result = json!({"id":id(&item.value).unwrap_or("unknown"),"kind":item.value["kind"],"status":"fail","score":null,"correct":false,"evals":cursor.evals,"reason":reason});
        if let Ok(directory) = directory(&run, &item.value) {
            let _ = files::write_json(&directory.join("run_manifest.json"), &result);
        }
        results.0.push(result);
    }
    Verdict::Fail(reason)
}
/// Register separate plan, cursor and result resources.
pub fn register(reg: &mut PhaseRegistry) -> Result<(), RegisterError> {
    reg.local(VariantPlan::default);
    reg.local(VariantCursor::default);
    reg.local(VariantResults::default);
    Ok(())
}
/// Compose instrumentation, proposals, selection, sequential evaluation and publication.
pub fn build(mut graph: Graph, _ctx: &PhaseCtx) -> Result<Graph, BuildError> {
    graph.system(abl_extract).named("abl.extract").done();
    graph
        .system(retrofit_call)
        .named("abl.retrofit.call")
        .done();
    graph.append(flow::agent("abl.retrofit", "variant_retrofit"))?;
    graph
        .system(retrofit_check)
        .named("abl.retrofit.check")
        .done();
    graph.system(propose_call).named("abl.propose.call").done();
    graph.append(flow::agent("abl.propose", "variant_propose"))?;
    graph
        .system(propose_check)
        .named("abl.propose.check")
        .done();
    graph.system(select_call).named("abl.select.call").done();
    graph.append(flow::agent("abl.select", "variant_select"))?;
    graph.system(select_apply).named("abl.select.apply").done();
    graph.system(materialize).named("abl.materialize").done();
    let mut verify = Graph::new();
    verify
        .system(verify_call)
        .named("variant.verify.call")
        .done();
    verify.append(flow::agent("variant.verify", "variant_verifier"))?;
    verify
        .system(verify_check)
        .named("variant.verify.check")
        .done();
    let verify = flow::repair_loop(
        "variant.verify.retry",
        "ablations.verify",
        verify,
        Graph::new(),
    );
    let mut candidate = Graph::new();
    candidate.add_conditional_branch::<CurrentItem, _, _, _>(
        "variant.control",
        |i| i.item().is_some_and(|i| control(&i.value)),
        |g| {
            g.system(control_eval).named("variant.control.eval").done();
        },
        move |g| {
            g.system(author_open).named("variant.edit.open").done();
            let mut edit = Graph::new();
            edit.system(author_call).named("variant.edit.call").done();
            edit.append(flow::agent("variant.edit", "variant_author"))
                .expect("agent block");
            edit.system(author_check).named("variant.edit.check").done();
            edit.system(author_verdict)
                .named("variant.edit.verdict")
                .done();
            g.append(flow::repair_loop(
                "variant.edit.retry",
                "ablations.edit",
                edit,
                Graph::new(),
            ))
            .expect("edit retry loop");
            g.system(variant_eval).named("variant.eval").done();
            g.add_conditional_branch::<VariantEvaluation, _, _, _>(
                "variant.scored",
                |e| e.result.is_ok(),
                move |g| {
                    g.system(verifier_open).named("variant.verify.open").done();
                    g.append(verify).expect("verifier loop");
                },
                |g| {
                    g.add_system(|| async {});
                },
            );
        },
    );
    candidate
        .system(variant_canon)
        .named("variant.canon")
        .done();
    let candidates = flow::repair_loop(
        "variant.evaluations",
        "ablations.evals",
        candidate,
        Graph::new(),
    );
    let mut body = Graph::new();
    body.system(variant_open).named("variant.open").done();
    body.append(candidates)?;
    body.system(variant_finish).named("variant.finish").done();
    body.add_error_handler(|g| {
        g.system(variant_failed).named("variant.failed").done();
    });
    let mut item_graph = Graph::new();
    item_graph.add_scope(
        "variant.body",
        body,
        polaris::graph::ContextPolicy::shared(),
    );
    graph.append(flow::worklist("ablations.variants", item_graph))?;
    graph.system(abl_join).named("abl.join").done();
    graph.system(abl_aggregate).named("abl.aggregate").done();
    graph
        .system(summarize_call)
        .named("abl.summarize.call")
        .done();
    graph.append(flow::agent("abl.summarize", "variant_summarizer"))?;
    graph.system(abl_readme).named("abl.readme").done();
    Ok(graph)
}
