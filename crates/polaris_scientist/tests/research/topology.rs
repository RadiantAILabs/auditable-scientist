use polaris::{
    graph::{Graph, GraphExecutor},
    system::{param::SystemContext, server::Server},
};
use polaris_cosmos::flow::{
    self, AgentRunner, EventLog, FlowSettings, HaltReason, LeaseBook, PhaseControl, PhasePolicy,
    PhaseStatus, RepairCounters, RunHandle, RunMode, Scenario, ScriptedRunner, SpendBook,
    StepJournal, StepKey, Worklist,
};
use polaris_scientist::{
    PhaseCtx, PhaseRegistry,
    config::ScientistConfig,
    phases::{ablations, investigation, posthoc, task},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}
fn scenario(phase: &str) -> Scenario {
    Scenario::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/scenarios")
            .join(phase),
    )
    .unwrap()
}
fn cfg(cycles: u32, workers: u32) -> ScientistConfig {
    ScientistConfig::resolve(
        &[],
        &[
            format!("investigation.execution.max_idea_cycles={cycles}"),
            format!("investigation.execution.workers={workers}"),
            "ablations.execution.max_evals_per_variant=2".into(),
            "ablations.execution.num_replications=1".into(),
        ],
        Default::default(),
    )
    .unwrap()
}
fn harness(
    root: &Path,
    cfg: &ScientistConfig,
    scenario: Scenario,
    budget: u64,
) -> (SystemContext<'static>, PhaseCtx, PhaseControl) {
    fs::create_dir_all(root).unwrap();
    let roles = polaris_scientist::roles::load(&assets().join("roles"), cfg).unwrap();
    let mut server = Server::new();
    server.insert_global(RunHandle {
        run_id: "offline".into(),
        root: root.into(),
        mode: RunMode::Run,
    });
    server.insert_global(cfg.clone());
    server.insert_global(roles.clone());
    server.insert_global(AgentRunner::new(Arc::new(
        ScriptedRunner::new(scenario, &root.join("scripted")).unwrap(),
    )));
    let shares = ["investigation", "ablations", "posthoc"]
        .into_iter()
        .map(|phase| (phase.to_string(), 1_000_000))
        .collect();
    server.insert_global(SpendBook::open(root, "offline", budget, shares, 250_000).unwrap());
    server.insert_global(StepJournal::open(root).unwrap());
    server.insert_global(RepairCounters::open(root, BTreeMap::new()).unwrap());
    server.insert_global(
        LeaseBook::open(
            root,
            cfg.u64("investigation.execution.max_idea_cycles") as u32,
        )
        .unwrap(),
    );
    server.insert_global(Worklist::open(&root.join(".state/worklist.json"), Vec::new()).unwrap());
    let control = PhaseControl::open(
        root,
        vec!["investigation".into(), "ablations".into(), "posthoc".into()],
        BTreeMap::new(),
    )
    .unwrap();
    server.insert_global(control.clone());
    server.insert_global(EventLog::open(root, "offline"));
    server.insert_global(FlowSettings {
        assets: assets(),
        task_dir: root.join("task"),
        model_id: cfg.string("runner.model").into(),
        ..FlowSettings::default()
    });
    let mut registry = PhaseRegistry::default();
    investigation::register(&mut registry).unwrap();
    ablations::register(&mut registry).unwrap();
    posthoc::register(&mut registry).unwrap();
    registry.install(&mut server);
    let ctx = server.create_context();
    (
        ctx,
        PhaseCtx {
            config: Arc::new(cfg.clone()),
            roles: Arc::new(roles),
            prefix: StepKey::new("test").unwrap(),
        },
        control,
    )
}
async fn baseline(root: &Path, cfg: &ScientistConfig) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/circpack/task");
    let task = task::snapshot(&source, root).unwrap();
    task::runtime::provision(
        &task,
        Path::new("python3"),
        Path::new("/nonexistent-wheelhouse"),
    )
    .await
    .unwrap();
    let request =
        task::evaluator_request(root, cfg, task.join("program_0"), task.join("baseline")).unwrap();
    polaris_scientist::eval::run_evaluator(request)
        .await
        .unwrap();
    let best = investigation::selection::candidate(
        &task.join("program_0"),
        &task.join("baseline"),
        "baseline",
    )
    .unwrap()
    .unwrap();
    task::files::write_json(&task.join("baseline/incumbent.json"), &best).unwrap();
    task::files::write_json(
        &root.join("investigation/best_program_manifest.json"),
        &best,
    )
    .unwrap();
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn calls(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("scripted/calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
async fn execute(graph: Graph, ctx: &mut SystemContext<'_>) {
    use polaris::graph::hooks::{HooksAPI, events::GraphEvent, schedule::OnSystemError};
    let hooks = HooksAPI::new();
    let errors = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    hooks
        .register_observer::<OnSystemError, _>("errors", move |event| {
            if let GraphEvent::SystemError {
                node_name, error, ..
            } = event
            {
                let message = format!("{node_name}: {error}");
                if errors.lock().unwrap().insert(message.clone()) {
                    eprintln!("{message}");
                }
            }
        })
        .unwrap();
    GraphExecutor::new()
        .execute(&graph, ctx, Some(&hooks), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn investigation_runs_one_leased_cycle_with_recorded_grants_and_staged_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(1, 1);
    baseline(root, &cfg).await;
    let (mut context, phase, control) = harness(root, &cfg, scenario("investigation"), 100_000_000);
    let graph = flow::phase(
        "investigation",
        PhasePolicy::Fatal,
        investigation::build(Graph::new(), &phase).unwrap(),
    );
    execute(graph, &mut context).await;
    assert_eq!(
        control.status("investigation"),
        Some(PhaseStatus::Completed)
    );
    let calls = calls(root);
    assert_eq!(
        calls
            .iter()
            .filter_map(|c| c["role"].as_str())
            .collect::<Vec<_>>(),
        vec!["ideation", "program_author"]
    );
    assert_eq!(calls[0]["grants"]["commands"], serde_json::json!([]));
    assert_eq!(calls[0]["limits"]["max_turns"], 30);
    assert_eq!(calls[1]["grants"]["edit"], true);
    assert!(
        !calls[1]["grants"]["commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for call in &calls {
        let work = Path::new(call["workspace"].as_str().unwrap());
        assert!(!work.join("inputs/task/evaluate.py").exists());
        assert!(
            !fs::read_to_string(work.join("role_prompt.md"))
                .unwrap()
                .contains("{{")
        );
    }
    let best = read(&root.join("investigation/best_program_manifest.json"));
    assert_eq!(best["combined_score"], 2.0);
    assert!(Path::new(best["program_dir"].as_str().unwrap()).is_absolute());
    let idea_line = fs::read_to_string(root.join("investigation/ideas_history.jsonl")).unwrap();
    let idea: Value = serde_json::from_str(idea_line.lines().next().unwrap()).unwrap();
    for key in [
        "agent",
        "agent_model",
        "cycle_index",
        "idea_dir_name",
        "idea_id",
        "reasoning_effort",
        "target_rubrics",
        "timestamp",
        "title",
        "hypothesis",
    ] {
        assert!(idea.get(key).is_some(), "missing {key}");
    }
    // The folder name is text, not the byte array that a raw file name would serialize to.
    let folder = idea["idea_dir_name"]
        .as_str()
        .expect("idea_dir_name is text");
    assert!(
        root.join("investigation/ideas").join(folder).is_dir(),
        "no idea folder named {folder}"
    );
    let attempt_line =
        fs::read_to_string(root.join("investigation/attempts_history.jsonl")).unwrap();
    let attempt: Value = serde_json::from_str(attempt_line.lines().next().unwrap()).unwrap();
    assert_eq!(attempt["idea_dir_name"].as_str(), Some(folder));
    assert_eq!(
        read(&root.join("investigation/parallel_summary.json"))["completed_no_winner"],
        false
    );
}

#[tokio::test]
async fn invalid_ideas_exhaust_the_idea_cap_and_fail_the_phase() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(1, 1);
    baseline(root, &cfg).await;
    fs::remove_file(root.join("investigation/best_program_manifest.json")).unwrap();
    let mut scenario = scenario("investigation");
    for entry in scenario.roles.get_mut("ideation").unwrap() {
        entry.files = None;
    }
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "investigation",
            PhasePolicy::Fatal,
            investigation::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert!(
        matches!(control.status("investigation"),Some(PhaseStatus::Failed(reason)) if reason.contains("IdeationExhausted"))
    );
    assert_eq!(calls(root).len(), 2);
    assert!(calls(root).iter().all(|c| c["role"] == "ideation"));
    assert_eq!(
        read(&root.join("investigation/best_program_manifest.json"))["combined_score"],
        1.0
    );
    let (mut resumed, phase, control) =
        harness(root, &cfg, self::scenario("investigation"), 100_000_000);
    resumed.insert_resource(RunHandle {
        run_id: "offline".into(),
        root: root.into(),
        mode: RunMode::Resume,
    });
    execute(
        flow::phase(
            "investigation",
            PhasePolicy::Fatal,
            investigation::build(Graph::new(), &phase).unwrap(),
        ),
        &mut resumed,
    )
    .await;
    assert!(
        matches!(control.status("investigation"),Some(PhaseStatus::Failed(reason)) if reason.contains("IdeationExhausted"))
    );
    assert_eq!(calls(root).len(), 2);
}

#[tokio::test]
async fn two_lanes_share_one_cycle_cap_and_keep_the_first_tied_winner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(2, 2);
    baseline(root, &cfg).await;
    let (mut context, phase, control) = harness(root, &cfg, scenario("investigation"), 100_000_000);
    execute(
        flow::phase(
            "investigation",
            PhasePolicy::Fatal,
            investigation::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(
        control.status("investigation"),
        Some(PhaseStatus::Completed)
    );
    let calls = calls(root);
    assert_eq!(calls.iter().filter(|c| c["role"] == "ideation").count(), 2);
    let leases = read(&root.join(".state/leases.json"));
    assert_eq!(leases["next"], 2);
    let best = read(&root.join("investigation/best_program_manifest.json"));
    assert!(best["program_dir"].as_str().unwrap().contains("worker_00"));
}

#[tokio::test]
async fn ablations_run_sequential_controls_and_a_verified_variant() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(0, 1);
    baseline(root, &cfg).await;
    let (mut context, phase, control) = harness(root, &cfg, scenario("ablations"), 100_000_000);
    execute(
        flow::phase(
            "ablations",
            PhasePolicy::Degrade,
            ablations::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(control.status("ablations"), Some(PhaseStatus::Completed));
    let summary = read(&root.join("ablations/pipeline_summary.json"));
    assert_eq!(summary["variants"].as_array().unwrap().len(), 3);
    assert_eq!(summary["replications_requested"], 1);
    assert_eq!(summary["replications_observed"], 1);
    assert_eq!(summary["cross_task"], "not_applicable");
    assert_eq!(summary["variants"][2]["status"], "pass");
    let canonical = read(&root.join(
        "ablations/variants/ablation_constant/task_bundle/program_1/results/rubric_status.json",
    ));
    assert_eq!(canonical["variant_id"], "ablation_constant");
    assert_eq!(canonical["attempt_id"], "ablation_constant_e1");
    assert_eq!(canonical["metrics"]["combined_score"], 2.0);
    assert_eq!(canonical["overall_pass"], true);
    let roles = calls(root)
        .iter()
        .map(|c| c["role"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        vec![
            "variant_retrofit",
            "variant_propose",
            "variant_select",
            "variant_author",
            "variant_verifier",
            "variant_summarizer"
        ]
    );
    assert!(root.join("ablations/README.md").exists());
    assert!(
        root.join("ablations/aggregated/circpack/analysis.csv")
            .exists()
    );
}

#[tokio::test]
async fn posthoc_publishes_valid_analysis_without_selecting_a_winner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(0, 1);
    baseline(root, &cfg).await;
    let (mut context, phase, control) = harness(root, &cfg, scenario("posthoc"), 100_000_000);
    execute(
        flow::phase(
            "posthoc",
            PhasePolicy::Degrade,
            posthoc::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(control.status("posthoc"), Some(PhaseStatus::Completed));
    let latest = read(&root.join("posthoc/latest_manifest.json"));
    assert!(latest["selected_analysis_id"].is_null());
    assert_eq!(latest["analyses"][0]["status"], "valid");
    assert_eq!(
        read(&root.join("posthoc/bootstrap_report.json"))["status"],
        "skipped"
    );
    assert_eq!(
        calls(root)
            .iter()
            .map(|c| c["role"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec!["posthoc_proposer", "posthoc_selector", "posthoc_analyst"]
    );
}

#[tokio::test]
async fn shared_lease_loop_finishes_after_one_unit() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = cfg(1, 1);
    let (mut context, _, _) = harness(temp.path(), &cfg, Scenario::default(), 100_000_000);
    let graph = flow::lease_loop("probe.leases", 1, |_| {
        let mut body = Graph::new();
        body.add_system(|| async { flow::Verdict::Pass });
        body
    });
    execute(graph, &mut context).await;
}

fn response(root: &Path, name: &str, source: &Path) -> PathBuf {
    let directory = root.join(name);
    task::files::copy_tree(source, &directory).unwrap();
    directory
}

#[tokio::test]
async fn a_failed_lane_is_tolerated_when_another_lane_completes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(2, 2);
    baseline(root, &cfg).await;
    let mut scenario = scenario("investigation");
    let valid = scenario.roles["ideation"][0].clone();
    let mut failed = valid.clone();
    failed.files = None;
    failed.when_step = Some("w00".into());
    let mut valid = valid;
    valid.when_step = Some("w01".into());
    scenario
        .roles
        .insert("ideation".into(), vec![failed, valid]);
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "investigation",
            PhasePolicy::Fatal,
            investigation::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(
        control.status("investigation"),
        Some(PhaseStatus::Completed)
    );
    assert_eq!(
        read(&root.join("investigation/best_program_manifest.json"))["combined_score"],
        2.0
    );
    assert!(
        root.join("investigation/worker_00/lane_failure.json")
            .exists()
    );
}

#[tokio::test]
async fn promotion_is_independent_of_attempt_success_and_ties_do_not_improve() {
    for (normal, score, expected_updated) in [(false, 2.0, true), (true, 1.0, false)] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let cfg = cfg(1, 1);
        baseline(root, &cfg).await;
        let mut scenario = scenario("investigation");
        let entry = &mut scenario.roles.get_mut("program_author").unwrap()[0];
        let directory = response(root, "response", entry.files.as_ref().unwrap());
        fs::write(
            directory.join("outputs/program_1/src/program.py"),
            format!("def solution():\n    score = {score}\n    return score\n"),
        )
        .unwrap();
        entry.files = Some(directory);
        if !normal {
            entry.stop = "turn_cap".into();
        }
        let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
        execute(
            flow::phase(
                "investigation",
                PhasePolicy::Fatal,
                investigation::build(Graph::new(), &phase).unwrap(),
            ),
            &mut context,
        )
        .await;
        assert_eq!(
            control.status("investigation"),
            Some(PhaseStatus::Completed)
        );
        let line = fs::read_to_string(root.join("investigation/attempts_history.jsonl")).unwrap();
        let attempt: Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
        assert_eq!(attempt["attempt_status"], "fail");
        assert_eq!(attempt["best_updated"], expected_updated);
        assert_eq!(attempt["metrics"]["combined_score"], score);
        assert_eq!(
            read(&root.join("investigation/parallel_summary.json"))["completed_no_winner"],
            !expected_updated
        );
    }
}

#[tokio::test]
async fn semantic_rubric_judge_runs_only_for_unassessed_correct_results() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut cfg = cfg(1, 1);
    cfg.values
        .get_mut("investigation")
        .unwrap()
        .get_mut("execution")
        .unwrap()
        .as_table_mut()
        .unwrap()
        .insert("rubric_judge".into(), toml::Value::Boolean(true));
    baseline(root, &cfg).await;
    let mut scenario = scenario("investigation");
    let entry = &mut scenario.roles.get_mut("ideation").unwrap()[0];
    let directory = response(root, "idea_response", entry.files.as_ref().unwrap());
    let mut idea = read(&directory.join("outputs/idea_bundle.json"));
    idea["target_rubrics"].as_array_mut().unwrap().push(serde_json::json!({"id":"semantic","type":"implementation","description":"The implementation follows the stated method."}));
    task::files::write_json(
        &directory.join("outputs/target_rubrics.json"),
        &idea["target_rubrics"],
    )
    .unwrap();
    task::files::write_json(&directory.join("outputs/idea_bundle.json"), &idea).unwrap();
    entry.files = Some(directory);
    let entry = &mut scenario.roles.get_mut("rubric_verifier").unwrap()[0];
    let directory = response(root, "judge_response", entry.files.as_ref().unwrap());
    let mut judgement = read(&directory.join("outputs/rubric_status.json"));
    judgement["rubrics"].as_array_mut().unwrap().push(serde_json::json!({"id":"semantic","pass":true,"evidence":"Source implements the declared fixture method."}));
    task::files::write_json(&directory.join("outputs/rubric_status.json"), &judgement).unwrap();
    entry.files = Some(directory);
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "investigation",
            PhasePolicy::Fatal,
            investigation::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(
        control.status("investigation"),
        Some(PhaseStatus::Completed)
    );
    assert_eq!(
        calls(root)
            .iter()
            .map(|c| c["role"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec!["ideation", "program_author", "rubric_verifier"]
    );
}

#[tokio::test]
async fn budget_refusals_fail_required_investigation_and_skip_optional_retrospective_roles() {
    for phase_name in ["investigation", "ablations", "posthoc"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let cfg = cfg(1, 1);
        baseline(root, &cfg).await;
        let (mut context, phase, control) = harness(root, &cfg, scenario(phase_name), 0);
        let (body, policy) = match phase_name {
            "investigation" => (
                investigation::build(Graph::new(), &phase).unwrap(),
                PhasePolicy::Fatal,
            ),
            "ablations" => (
                ablations::build(Graph::new(), &phase).unwrap(),
                PhasePolicy::Degrade,
            ),
            _ => (
                posthoc::build(Graph::new(), &phase).unwrap(),
                PhasePolicy::Degrade,
            ),
        };
        execute(flow::phase(phase_name, policy, body), &mut context).await;
        assert!(calls(root).is_empty());
        match phase_name {
            "investigation" => assert!(matches!(
                control.status(phase_name),
                Some(PhaseStatus::Failed(_))
            )),
            "ablations" => {
                assert_eq!(control.status(phase_name), Some(PhaseStatus::Completed));
                assert_eq!(
                    read(&root.join("ablations/retrofit_manifest.json"))["mode"],
                    "copied"
                );
            }
            _ => assert!(matches!(
                control.status(phase_name),
                Some(PhaseStatus::Degraded(_))
            )),
        }
    }
}

#[tokio::test]
async fn one_failed_variant_does_not_block_the_next_variant() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(0, 1);
    baseline(root, &cfg).await;
    let mut scenario = scenario("ablations");
    let entry = &mut scenario.roles.get_mut("variant_propose").unwrap()[0];
    let directory = response(root, "proposal_response", entry.files.as_ref().unwrap());
    let mut bundle = read(&directory.join("outputs/variant_bundle.json"));
    let mut bad = bundle["candidates"][0].clone();
    bad["variant_id"] = serde_json::json!("bad_variant");
    bundle["candidates"].as_array_mut().unwrap().insert(0, bad);
    task::files::write_json(&directory.join("outputs/variant_bundle.json"), &bundle).unwrap();
    entry.files = Some(directory);
    let good = scenario.roles["variant_author"][0].clone();
    let mut bad = good.clone();
    bad.files = None;
    bad.when_step = Some("bad_variant".into());
    let mut good = good;
    good.when_step = Some("ablation_constant".into());
    scenario
        .roles
        .insert("variant_author".into(), vec![bad, good]);
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "ablations",
            PhasePolicy::Degrade,
            ablations::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(control.status("ablations"), Some(PhaseStatus::Completed));
    let summary = read(&root.join("ablations/pipeline_summary.json"));
    assert!(
        summary["variants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "bad_variant" && v["status"] == "fail")
    );
    assert!(
        summary["variants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "ablation_constant" && v["status"] == "pass")
    );
}

#[tokio::test]
async fn analysis_timeout_stops_retries_and_no_valid_analysis_degrades() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(0, 1);
    baseline(root, &cfg).await;
    let mut scenario = scenario("posthoc");
    scenario.roles.get_mut("posthoc_analyst").unwrap()[0].stop = "timeout".into();
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "posthoc",
            PhasePolicy::Degrade,
            posthoc::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert!(matches!(
        control.status("posthoc"),
        Some(PhaseStatus::Degraded(_))
    ));
    assert_eq!(
        calls(root)
            .iter()
            .filter(|c| c["role"] == "posthoc_analyst")
            .count(),
        1
    );
    assert!(read(&root.join("posthoc/latest_manifest.json"))["selected_analysis_id"].is_null());
}

#[tokio::test]
async fn investigation_honors_two_cycles_and_two_evaluations_per_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut cfg = cfg(2, 1);
    cfg.set(
        "investigation.execution.max_program_evals_per_attempt",
        toml::Value::Integer(2),
    )
    .unwrap();
    baseline(root, &cfg).await;
    let mut scenario = scenario("investigation");
    let entry = &mut scenario.roles.get_mut("ideation").unwrap()[0];
    let directory = response(root, "high_target", entry.files.as_ref().unwrap());
    let mut idea = read(&directory.join("outputs/idea_bundle.json"));
    idea["target_rubrics"][0]["description"] = serde_json::json!("combined_score > 3.0");
    task::files::write_json(&directory.join("outputs/idea_bundle.json"), &idea).unwrap();
    task::files::write_json(
        &directory.join("outputs/target_rubrics.json"),
        &idea["target_rubrics"],
    )
    .unwrap();
    entry.files = Some(directory);
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "investigation",
            PhasePolicy::Fatal,
            investigation::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(
        control.status("investigation"),
        Some(PhaseStatus::Completed)
    );
    let calls = calls(root);
    assert_eq!(calls.iter().filter(|c| c["role"] == "ideation").count(), 2);
    assert_eq!(
        calls
            .iter()
            .filter(|c| c["role"] == "program_author")
            .count(),
        4
    );
    let attempts = fs::read_to_string(root.join("investigation/attempts_history.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["failure_streak_after"], 1);
    assert_eq!(attempts[1]["failure_streak_after"], 2);
    assert_eq!(attempts[0]["best_updated"], true);
    assert_eq!(attempts[1]["best_updated"], false);
    assert_eq!(
        read(&root.join("investigation/outer_state.json"))["current_cycle"],
        2
    );
}

#[tokio::test]
async fn variants_stop_at_the_evaluation_cap_and_report_requested_replications_separately() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut cfg = cfg(0, 1);
    cfg.set(
        "ablations.execution.num_replications",
        toml::Value::Integer(3),
    )
    .unwrap();
    baseline(root, &cfg).await;
    let mut scenario = scenario("ablations");
    let first = scenario.roles["variant_author"][0].clone();
    let mut second = first.clone();
    let directory = response(root, "second_candidate", first.files.as_ref().unwrap());
    fs::rename(
        directory.join("outputs/program_1"),
        directory.join("outputs/program_2"),
    )
    .unwrap();
    second.files = Some(directory);
    scenario
        .roles
        .insert("variant_author".into(), vec![first, second]);
    let entry = &mut scenario.roles.get_mut("variant_verifier").unwrap()[0];
    let directory = response(root, "failed_verification", entry.files.as_ref().unwrap());
    let mut report = read(&directory.join("outputs/rubric_status.json"));
    report["rubrics"][6]["pass"] = serde_json::json!(false);
    task::files::write_json(&directory.join("outputs/rubric_status.json"), &report).unwrap();
    entry.files = Some(directory);
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "ablations",
            PhasePolicy::Degrade,
            ablations::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(control.status("ablations"), Some(PhaseStatus::Completed));
    let summary = read(&root.join("ablations/pipeline_summary.json"));
    let variant = summary["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "ablation_constant")
        .unwrap();
    assert_eq!(variant["evals"], 2);
    assert_eq!(variant["status"], "fail");
    assert_eq!(summary["replications_requested"], 3);
    assert_eq!(summary["replications_observed"], 1);
    assert_eq!(
        calls(root)
            .iter()
            .filter(|c| c["role"] == "variant_author")
            .count(),
        2
    );
}

#[tokio::test]
async fn a_repair_that_carries_the_previous_candidates_results_is_still_scored() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(0, 1);
    baseline(root, &cfg).await;
    let mut scenario = scenario("ablations");
    let first = scenario.roles["variant_author"][0].clone();
    let mut second = first.clone();
    let directory = response(root, "repair_with_results", first.files.as_ref().unwrap());
    fs::rename(
        directory.join("outputs/program_1"),
        directory.join("outputs/program_2"),
    )
    .unwrap();
    // The author copied the evaluated first candidate, including the evaluator's files for it.
    let stale = directory.join("outputs/program_2/results");
    fs::create_dir_all(&stale).unwrap();
    task::files::write_json(
        &stale.join("evaluation_manifest.json"),
        &serde_json::json!({
            "program_sha256": "0".repeat(64),
            "evaluator_sha256": "0".repeat(64),
            "evaluator_fingerprint_after": "0".repeat(64),
            "integrity_valid": true,
            "started": 1,
            "ended": 2,
            "exit_code": 0,
            "timed_out": false,
            "candidate_fingerprint_before": "0".repeat(64),
            "candidate_fingerprint_after": "0".repeat(64),
        }),
    )
    .unwrap();
    task::files::write_json(
        &stale.join("metrics.json"),
        &serde_json::json!({"combined_score": 99.0}),
    )
    .unwrap();
    task::files::write_json(
        &stale.join("correct.json"),
        &serde_json::json!({"correct": true, "message": "from the previous candidate"}),
    )
    .unwrap();
    second.files = Some(directory);
    scenario
        .roles
        .insert("variant_author".into(), vec![first, second]);
    let entry = &mut scenario.roles.get_mut("variant_verifier").unwrap()[0];
    let directory = response(root, "failed_verification", entry.files.as_ref().unwrap());
    let mut report = read(&directory.join("outputs/rubric_status.json"));
    report["rubrics"][6]["pass"] = serde_json::json!(false);
    task::files::write_json(&directory.join("outputs/rubric_status.json"), &report).unwrap();
    entry.files = Some(directory);
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "ablations",
            PhasePolicy::Degrade,
            ablations::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert_eq!(control.status("ablations"), Some(PhaseStatus::Completed));
    let manifest = read(&root.join("ablations/variants/ablation_constant/run_manifest.json"));
    assert_eq!(manifest["evals"], 2);
    assert!(
        !manifest["reason"]
            .as_str()
            .unwrap()
            .contains("existing evaluation"),
        "the repair was refused: {manifest}"
    );
    let score = manifest["score"].as_f64().expect("the repair was scored");
    assert_ne!(score, 99.0, "the previous candidate's metrics were reused");
}

#[tokio::test]
async fn invalid_analyses_use_only_the_configured_attempts_and_each_source_copy_is_pristine() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(0, 1);
    baseline(root, &cfg).await;
    let mut scenario = scenario("posthoc");
    scenario.roles.get_mut("posthoc_analyst").unwrap()[0].files = None;
    let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "posthoc",
            PhasePolicy::Degrade,
            posthoc::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    assert!(matches!(
        control.status("posthoc"),
        Some(PhaseStatus::Degraded(_))
    ));
    assert_eq!(
        calls(root)
            .iter()
            .filter(|c| c["role"] == "posthoc_analyst")
            .count(),
        3
    );
    let digest = read(&root.join("posthoc/frozen_bundle.json"))["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    for attempt in 1..=3 {
        let source = root.join(format!(
            "posthoc/analyses/score_forensics/attempt_{attempt}/source"
        ));
        assert_eq!(
            polaris_scientist::eval::fingerprint(&source).unwrap(),
            digest
        );
    }
}

#[tokio::test]
async fn interrupted_author_resumes_the_same_cycle_and_uses_only_the_new_invocation_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(1, 1);
    baseline(root, &cfg).await;
    let mut interrupted = scenario("investigation");
    let entry = &mut interrupted.roles.get_mut("program_author").unwrap()[0];
    let directory = response(root, "interrupted_response", entry.files.as_ref().unwrap());
    fs::write(
        directory.join("outputs/program_1/src/program.py"),
        "def solution():\n    score = 9.0\n    return score\n",
    )
    .unwrap();
    entry.files = Some(directory);
    entry.stop = "interrupted".into();
    let (mut first, phase, control) = harness(root, &cfg, interrupted, 100_000_000);
    execute(
        flow::phase(
            "investigation",
            PhasePolicy::Fatal,
            investigation::build(Graph::new(), &phase).unwrap(),
        ),
        &mut first,
    )
    .await;
    assert_eq!(control.halt_reason(), Some(HaltReason::Interrupted));
    assert_eq!(
        read(&root.join(".state/leases.json"))["in_flight"]["0"]["global"],
        1
    );
    let (mut resumed, phase, control) = harness(root, &cfg, scenario("investigation"), 100_000_000);
    resumed.insert_resource(RunHandle {
        run_id: "offline".into(),
        root: root.into(),
        mode: RunMode::Resume,
    });
    execute(
        flow::phase(
            "investigation",
            PhasePolicy::Fatal,
            investigation::build(Graph::new(), &phase).unwrap(),
        ),
        &mut resumed,
    )
    .await;
    assert_eq!(
        control.status("investigation"),
        Some(PhaseStatus::Completed)
    );
    assert_eq!(
        read(&root.join("investigation/best_program_manifest.json"))["combined_score"],
        2.0
    );
    assert_eq!(
        calls(root)
            .iter()
            .filter(|c| c["role"] == "ideation")
            .count(),
        1
    );
    assert_eq!(
        calls(root)
            .iter()
            .filter(|c| c["role"] == "program_author")
            .count(),
        2
    );
    assert!(
        read(&root.join(".state/leases.json"))["in_flight"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let line = fs::read_to_string(root.join("investigation/attempts_history.jsonl")).unwrap();
    let attempts = line
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["attempt_status"], "pass");
}

#[tokio::test]
async fn unchanged_or_unsuccessful_authors_exhaust_edit_retries_without_scoring() {
    for stop in ["finished", "turn_cap", "timeout", "model_error"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let cfg = cfg(0, 1);
        baseline(root, &cfg).await;
        let mut scenario = scenario("ablations");
        let reply = root.join("reply");
        // A candidate directory and specification are present even on unsuccessful stops.
        // Finished authors copy the control unchanged, reproducing a failed edit.
        let source = if stop == "finished" {
            assets()
                .parent()
                .unwrap()
                .join("tests/scenarios/ablations/roles/variant_retrofit/01/outputs/program_0")
        } else {
            assets()
                .parent()
                .unwrap()
                .join("tests/scenarios/ablations/roles/variant_author/01/outputs/program_1")
        };
        task::files::copy_tree(&source, &reply.join("outputs/program_1")).unwrap();
        let entry = &mut scenario.roles.get_mut("variant_author").unwrap()[0];
        entry.files = Some(reply);
        entry.stop = stop.into();
        let (mut context, phase, control) = harness(root, &cfg, scenario, 100_000_000);
        execute(
            flow::phase(
                "ablations",
                PhasePolicy::Degrade,
                ablations::build(Graph::new(), &phase).unwrap(),
            ),
            &mut context,
        )
        .await;
        assert_eq!(control.status("ablations"), Some(PhaseStatus::Completed));
        let summary = read(&root.join("ablations/pipeline_summary.json"));
        let variant = &summary["variants"][2];
        assert_eq!(variant["status"], "fail", "{stop}: {variant}");
        assert_eq!(variant["evals"], 0);
        assert!(variant["score"].is_null());
        assert!(
            variant["reason"]
                .as_str()
                .unwrap()
                .contains(if stop == "finished" {
                    "unchanged"
                } else {
                    "did not finish"
                })
        );
        assert!(!root.join("ablations/variants/ablation_constant/task_bundle/program_1/results/metrics.json").exists());
        let calls = calls(root);
        let authors = calls
            .iter()
            .filter(|c| c["role"] == "variant_author")
            .collect::<Vec<_>>();
        assert_eq!(authors.len(), 3);
        assert!(!calls.iter().any(|c| c["role"] == "variant_verifier"));
        let second = Path::new(authors[1]["workspace"].as_str().unwrap());
        assert!(
            fs::read_to_string(second.join("role_prompt.md"))
                .unwrap()
                .contains(if stop == "finished" {
                    "unchanged"
                } else {
                    "did not finish"
                })
        );
    }
}

#[tokio::test]
async fn cloned_scorer_outputs_are_removed_before_a_variant_is_scored() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(0, 1);
    baseline(root, &cfg).await;
    let mut scenario = scenario("ablations");
    let author = &mut scenario.roles.get_mut("variant_author").unwrap()[0];
    let reply = response(root, "cloned_results", author.files.as_ref().unwrap());
    let stale = reply.join("outputs/program_1/results");
    fs::create_dir_all(&stale).unwrap();
    task::files::write_json(
        &stale.join("evaluation_manifest.json"),
        &serde_json::json!({"program_sha256":"old candidate"}),
    )
    .unwrap();
    task::files::write_json(
        &stale.join("metrics.json"),
        &serde_json::json!({"combined_score":999}),
    )
    .unwrap();
    author.files = Some(reply);
    let (mut context, phase, _) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "ablations",
            PhasePolicy::Degrade,
            ablations::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    let summary = read(&root.join("ablations/pipeline_summary.json"));
    assert_eq!(summary["variants"][2]["status"], "pass");
    assert_eq!(summary["variants"][2]["evals"], 1);
    assert_ne!(summary["variants"][2]["score"], 999);
    let canonical = root.join("ablations/variants/ablation_constant/task_bundle/program_1/results");
    assert_eq!(
        read(&canonical.join("evaluation_manifest.json"))["integrity_valid"],
        true
    );
}

#[tokio::test]
async fn unchanged_edit_can_be_repaired_before_the_first_evaluation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg = cfg(0, 1);
    baseline(root, &cfg).await;
    let mut scenario = scenario("ablations");
    let reply = root.join("unchanged_reply");
    let source = assets()
        .parent()
        .unwrap()
        .join("tests/scenarios/ablations/roles/variant_retrofit/01/outputs/program_0");
    task::files::copy_tree(&source, &reply.join("outputs/program_1")).unwrap();
    let good = scenario.roles["variant_author"][0].clone();
    let mut unchanged = good.clone();
    unchanged.files = Some(reply);
    scenario
        .roles
        .insert("variant_author".into(), vec![unchanged, good]);
    let (mut context, phase, _) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "ablations",
            PhasePolicy::Degrade,
            ablations::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    let summary = read(&root.join("ablations/pipeline_summary.json"));
    assert_eq!(summary["variants"][2]["status"], "pass");
    assert_eq!(summary["variants"][2]["evals"], 1);
    assert_eq!(
        calls(root)
            .iter()
            .filter(|c| c["role"] == "variant_author")
            .count(),
        2
    );
}

#[tokio::test]
async fn a_primary_metric_the_evaluator_does_not_name_stands_for_its_score() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut cfg = cfg(0, 1);
    cfg.set(
        "ablations.execution.max_evals_per_variant",
        toml::Value::Integer(1),
    )
    .unwrap();
    baseline(root, &cfg).await;
    let mut scenario = scenario("ablations");
    let entry = &mut scenario.roles.get_mut("variant_author").unwrap()[0];
    let directory = response(root, "named_quantity", entry.files.as_ref().unwrap());
    let source = directory.join("outputs/program_1/src/program.py");
    let text = fs::read_to_string(&source).unwrap();
    assert!(text.contains("True,'combined_score',score"));
    fs::write(
        &source,
        text.replace("True,'combined_score',score", "True,'sum_of_radii',score"),
    )
    .unwrap();
    entry.files = Some(directory);
    let (mut context, phase, _) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "ablations",
            PhasePolicy::Degrade,
            ablations::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    let variant = root.join("ablations/variants/ablation_constant");
    let status = read(&variant.join("task_bundle/program_1/results/rubric_status.json"));
    let check = status["rubrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "metrics_consistent_with_metrics_json")
        .unwrap();
    assert_eq!(check["pass"], true, "{check}");
    assert_eq!(read(&variant.join("run_manifest.json"))["status"], "pass");
}

#[tokio::test]
async fn model_passes_cannot_override_host_analysis_failures() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut cfg = cfg(0, 1);
    cfg.set(
        "ablations.execution.max_evals_per_variant",
        toml::Value::Integer(1),
    )
    .unwrap();
    baseline(root, &cfg).await;
    let mut scenario = scenario("ablations");
    let entry = &mut scenario.roles.get_mut("variant_author").unwrap()[0];
    let directory = response(root, "wrong_csv", entry.files.as_ref().unwrap());
    let source = directory.join("outputs/program_1/src/program.py");
    fs::write(
        &source,
        fs::read_to_string(&source)
            .unwrap()
            .replace("True,score,True", "True,9.0,True"),
    )
    .unwrap();
    entry.files = Some(directory);
    let (mut context, phase, _) = harness(root, &cfg, scenario, 100_000_000);
    execute(
        flow::phase(
            "ablations",
            PhasePolicy::Degrade,
            ablations::build(Graph::new(), &phase).unwrap(),
        ),
        &mut context,
    )
    .await;
    let variant = root.join("ablations/variants/ablation_constant");
    let manifest = read(&variant.join("run_manifest.json"));
    assert_eq!(manifest["status"], "fail");
    assert_eq!(manifest["evals"], 1);
    assert!(
        manifest["reason"]
            .as_str()
            .unwrap()
            .contains("metrics_consistent_with_metrics_json")
    );
    let status = read(&variant.join("task_bundle/program_1/results/rubric_status.json"));
    assert_eq!(status["overall_pass"], false);
    assert_eq!(
        status["rubrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "metrics_consistent_with_metrics_json")
            .unwrap()["pass"],
        false
    );
}
