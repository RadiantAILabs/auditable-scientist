use polaris::{
    graph::{Graph, GraphExecutor},
    system::param::SystemContext,
};
use polaris_cosmos::flow::{RoleBook, RunHandle, RunMode, StepKey};
use polaris_scientist::{
    PhaseCtx,
    config::ScientistConfig,
    phases::{drafting, final_eval, task},
};
use serde_json::Value;
use std::{fs, path::Path, sync::Arc};

fn config(task: &Path) -> ScientistConfig {
    let mut cfg = ScientistConfig::default();
    cfg.values.insert(
        "task".into(),
        toml::Value::Table(toml::Table::from_iter([(
            "dir".into(),
            toml::Value::String(task.to_string_lossy().into_owned()),
        )])),
    );
    cfg
}
fn phase_ctx(cfg: &ScientistConfig, name: &str) -> PhaseCtx {
    PhaseCtx {
        config: Arc::new(cfg.clone()),
        roles: Arc::new(RoleBook::default()),
        prefix: StepKey::new(name).unwrap(),
    }
}
fn system_ctx(root: &Path, cfg: ScientistConfig) -> SystemContext<'static> {
    let mut ctx = SystemContext::new();
    ctx.insert_resource(RunHandle {
        run_id: "test".into(),
        root: root.into(),
        mode: RunMode::Run,
    });
    ctx.insert_resource(polaris_cosmos::flow::StepJournal::open(root).unwrap());
    ctx.insert_resource(cfg);
    ctx
}

#[tokio::test]
async fn task_graph_records_baseline_and_reuses_immutable_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("run");
    task::files::copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/circpack/task"),
        &source,
    )
    .unwrap();
    let cfg = config(&source);
    let graph = task::build(Graph::new(), &phase_ctx(&cfg, "task")).unwrap();
    let mut ctx = system_ctx(&root, cfg.clone());
    GraphExecutor::new()
        .execute(&graph, &mut ctx, None, None)
        .await
        .unwrap();
    let inspection: Value = serde_json::from_slice(
        &fs::read(root.join("task_drafting/scaffold_inspection.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(inspection["task_contract_present"], false);
    let best: Value = serde_json::from_slice(
        &fs::read(root.join("investigation/best_program_manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(best["combined_score"], 1.0);
    assert_eq!(best["source_attempt_id"], "baseline");
    fs::write(
        source.join("program_0/src/program.py"),
        "def solution(): return 9.0\n",
    )
    .unwrap();
    let mut ctx = system_ctx(&root, cfg);
    GraphExecutor::new()
        .execute(&graph, &mut ctx, None, None)
        .await
        .unwrap();
    assert!(
        fs::read_to_string(root.join("task/program_0/src/program.py"))
            .unwrap()
            .contains("return 1.0")
    );
}

#[tokio::test]
async fn task_graph_names_missing_scaffold_and_optional_drafting_is_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path());
    let graph = task::build(Graph::new(), &phase_ctx(&cfg, "task")).unwrap();
    let mut ctx = system_ctx(&temp.path().join("run"), cfg.clone());
    let error = GraphExecutor::new()
        .execute(&graph, &mut ctx, None, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("evaluate.py"));
    let graph = drafting::build(Graph::new(), &phase_ctx(&cfg, "drafting")).unwrap();
    let error = GraphExecutor::new()
        .execute(&graph, &mut ctx, None, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("NotBuilt"));
}

#[tokio::test]
async fn final_graph_records_missing_and_unsupported_contracts() {
    for present in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("task")).unwrap();
        if present {
            fs::write(temp.path().join("task/task_contract.json"), "{}").unwrap();
        }
        let cfg = ScientistConfig::default();
        let graph = final_eval::build(Graph::new(), &phase_ctx(&cfg, "final_evaluation")).unwrap();
        let mut ctx = system_ctx(temp.path(), cfg);
        GraphExecutor::new()
            .execute(&graph, &mut ctx, None, None)
            .await
            .unwrap();
        let result: Value = serde_json::from_slice(
            &fs::read(
                temp.path()
                    .join("final_evaluation/final_launch_failure.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            result["status"],
            if present { "unsupported" } else { "degraded" }
        );
        assert_eq!(result["task_contract_present"], present);
    }
}

#[tokio::test]
async fn task_graph_fails_a_managed_runtime_with_incorrect_installed_pins() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("run");
    task::files::copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/circpack/task"),
        &source,
    )
    .unwrap();
    fs::write(source.join("requirements.txt"), "fixture-package==1.0\n").unwrap();
    let snapshot = task::snapshot(&source, &root).unwrap();
    fs::create_dir_all(snapshot.join(".venv/bin")).unwrap();
    let python = snapshot.join(".venv/bin/python");
    fs::write(&python,"#!/bin/sh\nprintf '%s\\n' '{\"version\":\"fixture\",\"installed\":{\"fixture-package\":\"2.0\"}}'\n").unwrap();
    fs::set_permissions(python, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        snapshot.join(".venv/.scientist-managed"),
        "managed fixture\n",
    )
    .unwrap();
    let cfg = config(&source);
    let graph = task::build(Graph::new(), &phase_ctx(&cfg, "task")).unwrap();
    let mut ctx = system_ctx(&root, cfg);
    let error = GraphExecutor::new()
        .execute(&graph, &mut ctx, None, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("pin mismatch"), "{error}");
}
