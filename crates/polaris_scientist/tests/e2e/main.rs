//! Whole campaign proofs over small replaceable phase graphs, entirely offline.
use polaris::{
    graph::Graph,
    system::param::{Out, Res},
};
use polaris_cosmos::flow::*;
use polaris_scientist::{
    campaign,
    config::{ConfigFlags, PHASES, ScientistConfig},
    durable::{read_json, write_json},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
struct Fixture {
    dir: tempfile::TempDir,
    task: PathBuf,
    assets: PathBuf,
    cfg: ScientistConfig,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("example");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(task.join("evaluate.py"), "# frozen evaluator\n").unwrap();
        let assets = dir.path().join("assets");
        std::fs::create_dir_all(assets.join("roles")).unwrap();
        std::fs::create_dir_all(assets.join("prompts")).unwrap();
        std::fs::write(assets.join("roles/worker.toml"),"[roles.worker]\nprompt='worker'\nnode='write'\nmax_turns=4\nmax_usd=1.0\ntimeout_seconds=30\nshell=[]\nread_only=false\nskills=[]\n").unwrap();
        std::fs::write(
            assets.join("prompts/worker.md"),
            "Read {{TASK_DIR}} and produce outputs/result.json.",
        )
        .unwrap();
        let cfg = ScientistConfig::resolve(
            &[],
            &["investigation.execution.max_idea_cycles=2".into()],
            ConfigFlags {
                results_root: Some(dir.path().join("results")),
                max_usd: Some(10_000_000),
                ..ConfigFlags::default()
            },
        )
        .unwrap();
        Self {
            dir,
            task,
            assets,
            cfg,
        }
    }
    fn root(&self) -> PathBuf {
        campaign::run_root(&self.cfg.path("paths.results_root"), &self.task, "fixture")
    }
    fn calls(&self) -> PathBuf {
        self.dir.path().join("calls")
    }
    fn runner(&self, entry: ScriptEntry) -> Arc<ScriptedRunner> {
        Arc::new(
            ScriptedRunner::new(
                Scenario {
                    usage: Usage {
                        model_id: self.cfg.string("runner.model").into(),
                        input: 100,
                        output: 20,
                        requests: 1,
                        micros: 10_000,
                        ..Usage::default()
                    },
                    roles: BTreeMap::from([("worker".into(), vec![entry])]),
                },
                &self.calls(),
            )
            .unwrap(),
        )
    }
}
fn lines(path: &Path) -> Vec<Value> {
    if !path.exists() {
        return vec![];
    }
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn simple_status(status: PhaseStatus) -> Graph {
    let mut body = Graph::new();
    body.add_system(move || {
        let status = status.clone();
        async move { status }
    });
    body
}
fn bodies() -> BTreeMap<String, Graph> {
    let mut bodies = PHASES
        .iter()
        .map(|phase| (phase.to_string(), simple_status(PhaseStatus::Completed)))
        .collect::<BTreeMap<_, _>>();
    bodies.insert(
        "final_evaluation".into(),
        simple_status(PhaseStatus::Degraded("no sealed contract".into())),
    );
    bodies.insert(
        "review".into(),
        simple_status(PhaseStatus::Skipped("external review disabled".into())),
    );
    bodies
}
fn key(text: &str) -> StepKey {
    StepKey::new(text).unwrap()
}
fn single_call(phase: &str, name: &str) -> Graph {
    let call = AgentCall {
        step: key(phase).child(name).unwrap(),
        role: "worker".into(),
        phase: phase.into(),
        stage: StageSpec::default(),
        expect: vec![],
    };
    let mut graph = Graph::new();
    graph.add_system(move || {
        let call = call.clone();
        async move { call }
    });
    graph.append(agent(name, "worker")).unwrap();
    graph.add_system(|| async { PhaseStatus::Completed });
    graph
}
#[tokio::test]
async fn campaign_records_all_phases_in_order_and_both_no_winner_branches() {
    for policy in ["best_available", "skip"] {
        let fixture = Fixture::new();
        let mut cfg = fixture.cfg.clone();
        cfg.set(
            "campaign.no_winner_policy",
            toml::Value::String(policy.into()),
        )
        .unwrap();
        let outcome = campaign::run_with_phases(
            &fixture.task,
            cfg,
            fixture.runner(ScriptEntry::default()),
            Some("fixture".into()),
            Some(bodies()),
            fixture.assets.clone(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.outcome, "completed_no_winner");
        assert_eq!(
            outcome.phases["ablations"],
            if policy == "skip" {
                "skipped"
            } else {
                "completed"
            }
        );
        assert_eq!(outcome.phases["final_evaluation"], "degraded");
        assert_eq!(outcome.phases["review"], "skipped");
        let root = fixture.root();
        let events = lines(&root.join("pipeline_events.jsonl"));
        let started = events
            .iter()
            .filter(|e| e["event"] == "phase_start")
            .map(|e| e["phase"].as_str().unwrap())
            .collect::<Vec<_>>();
        let expected = PHASES
            .iter()
            .copied()
            .filter(|p| policy != "skip" || *p != "ablations")
            .chain(["review"])
            .collect::<Vec<_>>();
        assert_eq!(started, expected);
        assert_eq!(events.first().unwrap()["event"], "pipeline_start");
        assert_eq!(events.last().unwrap()["event"], "pipeline_end");
        let saved: campaign::Outcome = read_json(&root.join("outcome.json")).unwrap();
        assert_eq!(saved.outcome, outcome.outcome);
        let summary: Value = read_json(&root.join("metrics/cost_summary.json")).unwrap();
        for key in [
            "by_model",
            "by_phase",
            "models_called",
            "total_cost_usd",
            "cost_basis",
            "reserved_usd",
        ] {
            assert!(summary.get(key).is_some());
        }
        assert_eq!(summary["total_cost_usd"], "0.000000");
        assert!(root.join("config.resolved.toml").exists());
        assert!(root.join("roles.resolved.toml").exists());
    }
}
#[tokio::test]
async fn fatal_task_stops_later_phases_and_paper_failure_is_incomplete() {
    for phase in ["task_drafting", "paper"] {
        let fixture = Fixture::new();
        let mut graphs = bodies();
        graphs.insert(
            phase.into(),
            simple_status(PhaseStatus::Failed("fixture rejected".into())),
        );
        let outcome = campaign::run_with_phases(
            &fixture.task,
            fixture.cfg.clone(),
            fixture.runner(ScriptEntry::default()),
            Some("fixture".into()),
            Some(graphs),
            fixture.assets.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome.outcome,
            if phase == "paper" {
                "paper_incomplete"
            } else {
                "failed"
            }
        );
        if phase == "paper" {
            assert_eq!(outcome.phases["review"], "skipped");
        }
        let events = lines(&fixture.root().join("pipeline_events.jsonl"));
        if phase == "task_drafting" {
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e["event"] == "phase_start")
                    .count(),
                1
            );
        }
        assert_eq!(events.last().unwrap()["event"], "pipeline_end");
    }
}
#[tokio::test]
async fn generated_winner_takes_the_study_branch_even_under_skip_policy() {
    let fixture = Fixture::new();
    let mut cfg = fixture.cfg.clone();
    cfg.set(
        "campaign.no_winner_policy",
        toml::Value::String("skip".into()),
    )
    .unwrap();
    let mut graphs = bodies();
    let mut investigation = Graph::new();
    investigation.add_system(bind::<Res<RunHandle>, PhaseStatus, _>(
        "publish.best",
        |handle, _| {
            Box::pin(async move {
                write_json(
                    &handle.root.join("task/baseline/metrics.json"),
                    &json!({"combined_score":1.0}),
                )?;
                write_json(
                    &handle.root.join("investigation/best_program_manifest.json"),
                    &json!({"correct":true,"combined_score":2.0}),
                )?;
                Ok(PhaseStatus::Completed)
            })
        },
    ));
    graphs.insert("investigation".into(), investigation);
    let outcome = campaign::run_with_phases(
        &fixture.task,
        cfg,
        fixture.runner(ScriptEntry::default()),
        Some("fixture".into()),
        Some(graphs),
        fixture.assets.clone(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.outcome, "completed");
    assert_eq!(outcome.phases["ablations"], "completed");
}
#[tokio::test]
async fn two_lanes_finish_and_settle_before_the_phase_join() {
    let fixture = Fixture::new();
    let mut graphs = bodies();
    let mut investigation = lease_loop("workers", 2, |worker| {
        let mut lane = Graph::new();
        lane.add_system(bind::<Out<LaneLease>, AgentCall, _>(
            "make.call",
            move |current, _| {
                Box::pin(async move {
                    Ok(AgentCall {
                        step: key(&format!(
                            "investigation/w{worker}/c{}/write",
                            current.lease.as_ref().unwrap().global
                        )),
                        role: "worker".into(),
                        phase: "investigation".into(),
                        stage: StageSpec::default(),
                        expect: vec![],
                    })
                })
            },
        ));
        lane.append(agent("write", "worker")).unwrap();
        lane.add_system(|| async { Verdict::Pass });
        lane
    });
    investigation.add_system(|| async { PhaseStatus::Completed });
    graphs.insert("investigation".into(), investigation);
    let outcome = campaign::run_with_phases(
        &fixture.task,
        fixture.cfg.clone(),
        fixture.runner(ScriptEntry {
            delay_ms: 25,
            ..ScriptEntry::default()
        }),
        Some("fixture".into()),
        Some(graphs),
        fixture.assets.clone(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.models_called.values().sum::<u64>(), 2);
    assert_eq!(lines(&fixture.calls().join("calls.jsonl")).len(), 2);
    assert_eq!(
        lines(&fixture.root().join("metrics/cost_events.jsonl")).len(),
        2
    );
    assert!(outcome.wall_seconds < 60.0);
    let events = lines(&fixture.root().join("pipeline_events.jsonl"));
    let end = events
        .iter()
        .position(|e| e["event"] == "phase_end" && e["phase"] == "investigation")
        .unwrap();
    assert_eq!(
        events[..end]
            .iter()
            .filter(|e| e["event"] == "agent_end")
            .count(),
        2
    );
}
fn two_calls() -> BTreeMap<String, Graph> {
    let mut graphs = bodies();
    graphs.insert(
        "task_drafting".into(),
        single_call("task_drafting", "first"),
    );
    graphs.insert(
        "investigation".into(),
        single_call("investigation", "second"),
    );
    graphs
}
#[tokio::test]
async fn interruption_closes_run_and_resume_repeats_only_the_interrupted_step() {
    let fixture = Fixture::new();
    let flag = StopFlag::default();
    let mut scenario = Scenario {
        usage: Usage {
            model_id: fixture.cfg.string("runner.model").into(),
            requests: 1,
            micros: 10_000,
            ..Usage::default()
        },
        ..Scenario::default()
    };
    scenario.roles.insert(
        "worker".into(),
        vec![
            ScriptEntry {
                when_step: Some("first".into()),
                ..ScriptEntry::default()
            },
            ScriptEntry {
                when_step: Some("second".into()),
                delay_ms: 30_000,
                ..ScriptEntry::default()
            },
        ],
    );
    let runner = Arc::new(
        ScriptedRunner::new(scenario, &fixture.calls())
            .unwrap()
            .with_stop(flag.clone()),
    );
    let task = fixture.task.clone();
    let cfg = fixture.cfg.clone();
    let assets = fixture.assets.clone();
    let running = tokio::spawn(async move {
        campaign::run_with_phases(
            &task,
            cfg,
            runner,
            Some("fixture".into()),
            Some(two_calls()),
            assets,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while lines(&fixture.calls().join("calls.jsonl")).len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    flag.request();
    let interrupted = running.await.unwrap().unwrap();
    assert_eq!(interrupted.outcome, "interrupted");
    assert_eq!(interrupted.phases["investigation"], "failed");
    assert!(!interrupted.phases.contains_key("paper"));
    let root = fixture.root();
    let events = lines(&root.join("pipeline_events.jsonl"));
    assert_eq!(events.last().unwrap()["event"], "pipeline_end");
    assert_eq!(events.last().unwrap()["fields"]["outcome"], "interrupted");
    assert!(
        lines(&root.join("metrics/cost_events.jsonl"))
            .iter()
            .any(|line| line["status"] == "interrupted")
    );
    let resumed = campaign::resume_with_phases(
        &root,
        &[],
        None,
        fixture.runner(ScriptEntry::default()),
        Some(two_calls()),
        fixture.assets.clone(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.outcome, "completed_no_winner");
    let calls = lines(&fixture.calls().join("calls.jsonl"));
    assert_eq!(
        calls
            .iter()
            .filter(|c| c["step"] == "task_drafting/first")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c["step"] == "investigation/second")
            .count(),
        2
    );
    assert!(
        root.join("work/_abandoned/investigation/second/001")
            .exists()
    );
    assert!(root.join("work/investigation/second/002").exists());
    assert!(
        campaign::resume_with_phases(
            &root,
            &["investigation.execution.workers=2".into()],
            None,
            fixture.runner(ScriptEntry::default()),
            Some(two_calls()),
            fixture.assets.clone()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("shape key")
    );
}
#[tokio::test]
async fn reset_keeps_earlier_records_and_settled_spend() {
    let fixture = Fixture::new();
    campaign::run_with_phases(
        &fixture.task,
        fixture.cfg.clone(),
        fixture.runner(ScriptEntry::default()),
        Some("fixture".into()),
        Some(two_calls()),
        fixture.assets.clone(),
    )
    .await
    .unwrap();
    let root = fixture.root();
    let before = std::fs::read(root.join("metrics/spend_state.json")).unwrap();
    let journal = StepJournal::open(&root).unwrap();
    assert!(journal.get(&key("task_drafting/first")).is_some());
    assert!(journal.get(&key("investigation/second")).is_some());
    campaign::reset(&root, "investigation").unwrap();
    assert!(journal.get(&key("task_drafting/first")).is_some());
    assert!(journal.get(&key("investigation/second")).is_none());
    assert_eq!(
        std::fs::read(root.join("metrics/spend_state.json")).unwrap(),
        before
    );
    assert!(!root.join("outcome.json").exists());
    assert!(root.join("_reset").exists());
}

#[tokio::test]
async fn dropped_campaign_settles_abandoned_call_and_resumes_without_repeating_done_work() {
    let fixture = Fixture::new();
    let mut scenario = Scenario {
        usage: Usage {
            model_id: fixture.cfg.string("runner.model").into(),
            micros: 10_000,
            ..Usage::default()
        },
        ..Scenario::default()
    };
    scenario.roles.insert(
        "worker".into(),
        vec![
            ScriptEntry {
                when_step: Some("first".into()),
                ..ScriptEntry::default()
            },
            ScriptEntry {
                when_step: Some("second".into()),
                delay_ms: 30_000,
                ..ScriptEntry::default()
            },
        ],
    );
    let runner = Arc::new(ScriptedRunner::new(scenario, &fixture.calls()).unwrap());
    let task = fixture.task.clone();
    let cfg = fixture.cfg.clone();
    let assets = fixture.assets.clone();
    let running = tokio::spawn(async move {
        campaign::run_with_phases(
            &task,
            cfg,
            runner,
            Some("fixture".into()),
            Some(two_calls()),
            assets,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while lines(&fixture.calls().join("calls.jsonl")).len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    let root = fixture.root();
    let pending: Value = read_json(&root.join("metrics/spend_state.json")).unwrap();
    assert_eq!(pending["reserved"].as_object().unwrap().len(), 1);
    let resumed = campaign::resume_with_phases(
        &root,
        &[],
        None,
        fixture.runner(ScriptEntry::default()),
        Some(two_calls()),
        fixture.assets.clone(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.outcome, "completed_no_winner");
    assert_eq!(resumed.usd_settled, "1.020000");
    let ledger = lines(&root.join("metrics/cost_events.jsonl"));
    assert_eq!(
        ledger
            .iter()
            .filter(|line| line["status"] == "abandoned")
            .count(),
        1
    );
    assert_eq!(
        ledger
            .iter()
            .find(|line| line["status"] == "abandoned")
            .unwrap()["total_cost_usd"],
        "1.000000"
    );
    assert!(
        root.join("work/_abandoned/investigation/second/001")
            .exists()
    );
    let calls = lines(&fixture.calls().join("calls.jsonl"));
    assert_eq!(
        calls
            .iter()
            .filter(|c| c["step"] == "task_drafting/first")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c["step"] == "investigation/second")
            .count(),
        2
    );
    assert!(
        resumed
            .models_called
            .keys()
            .all(|model| model == fixture.cfg.string("runner.model"))
    );
}

fn same_pass_roles() -> BTreeMap<String, Graph> {
    let mut phases = bodies();
    let mut body = Graph::new();
    for (step, role) in [("first", "worker"), ("second", "follower")] {
        let call = AgentCall {
            step: key("investigation").child(step).unwrap(),
            role: role.into(),
            phase: "investigation".into(),
            stage: StageSpec::default(),
            expect: vec![],
        };
        body.add_system(move || {
            let call = call.clone();
            async move { call }
        });
        body.append(agent(step, role)).unwrap();
    }
    body.add_system(bind::<(Out<AgentOutcome>, Res<PhaseControl>), PhaseStatus, _>(
        "check.following.role",
        |(outcome, control), _| Box::pin(async move {
            if control.halt_reason() == Some(HaltReason::Interrupted) {
                assert!(matches!(&*outcome, AgentOutcome::Skipped(SkipReason::Disabled)));
                return Err(polaris::system::system::SystemError::ExecutionError("required following role was skipped after interruption".into()));
            }
            assert!(matches!(&*outcome, AgentOutcome::Ran { report, .. } if report.stop == StopCause::Finished));
            Ok(PhaseStatus::Completed)
        }),
    ));
    phases.insert("investigation".into(), body);
    phases
}

#[tokio::test]
async fn resume_runs_a_role_skipped_after_interruption_and_journals_each_role_once() {
    let fixture = Fixture::new();
    let roles_path = fixture.assets.join("roles/worker.toml");
    let roles = std::fs::read_to_string(&roles_path).unwrap();
    std::fs::write(
        &roles_path,
        format!(
            "{roles}\n{}",
            roles.replace("roles.worker", "roles.follower")
        ),
    )
    .unwrap();
    let flag = StopFlag::default();
    let runner = Arc::new(
        ScriptedRunner::new(
            Scenario {
                roles: BTreeMap::from([
                    (
                        "worker".into(),
                        vec![ScriptEntry {
                            delay_ms: 30_000,
                            ..ScriptEntry::default()
                        }],
                    ),
                    ("follower".into(), vec![ScriptEntry::default()]),
                ]),
                ..Scenario::default()
            },
            &fixture.calls(),
        )
        .unwrap()
        .with_stop(flag.clone()),
    );
    let task = fixture.task.clone();
    let cfg = fixture.cfg.clone();
    let assets = fixture.assets.clone();
    let running = tokio::spawn(async move {
        campaign::run_with_phases(
            &task,
            cfg,
            runner,
            Some("fixture".into()),
            Some(same_pass_roles()),
            assets,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while lines(&fixture.calls().join("calls.jsonl")).is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    flag.request();
    let interrupted = running.await.unwrap().unwrap();
    assert_eq!(interrupted.outcome, "interrupted");
    let root = fixture.root();
    let journal = StepJournal::open(&root).unwrap();
    for step in [
        "investigation/first",
        "investigation/second",
        "investigation/phase.complete",
    ] {
        assert!(
            journal.get(&key(step)).is_none(),
            "{step} must remain unfinished"
        );
    }
    let state: BTreeMap<String, PhaseStatus> =
        read_json(&root.join("pipeline_state.json")).unwrap();
    assert_eq!(
        state["investigation"],
        PhaseStatus::Failed("interrupted".into())
    );
    assert_eq!(lines(&fixture.calls().join("calls.jsonl")).len(), 1);
    let runner = Arc::new(
        ScriptedRunner::new(
            Scenario {
                roles: BTreeMap::from([
                    ("worker".into(), vec![ScriptEntry::default()]),
                    ("follower".into(), vec![ScriptEntry::default()]),
                ]),
                ..Scenario::default()
            },
            &fixture.calls(),
        )
        .unwrap(),
    );
    let resumed = campaign::resume_with_phases(
        &root,
        &[],
        None,
        runner.clone(),
        Some(same_pass_roles()),
        fixture.assets.clone(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.outcome, "completed_no_winner");
    let calls = lines(&fixture.calls().join("calls.jsonl"));
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["step"] == "investigation/first")
            .count(),
        2
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["step"] == "investigation/second")
            .count(),
        1
    );
    for step in ["investigation/first", "investigation/second"] {
        let record = journal.get(&key(step)).unwrap();
        let outcome: AgentOutcome = serde_json::from_value(record.output).unwrap();
        assert!(
            matches!(outcome, AgentOutcome::Ran { report, .. } if report.stop == StopCause::Finished)
        );
        assert_eq!(
            lines(&root.join("journal.jsonl"))
                .iter()
                .filter(|line| line["key"] == step && line["state"] == "done")
                .count(),
            1
        );
    }
    campaign::resume_with_phases(
        &root,
        &[],
        None,
        runner,
        Some(same_pass_roles()),
        fixture.assets.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        lines(&fixture.calls().join("calls.jsonl")).len(),
        calls.len()
    );
}
