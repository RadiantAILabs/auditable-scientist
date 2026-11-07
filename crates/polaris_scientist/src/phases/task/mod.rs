//! Task inspection, immutable snapshot, pinned runtime and first incumbent.

pub mod files;
pub mod runtime;
use crate::{
    config::ScientistConfig,
    eval::{self, EvalError, EvalRequest},
    phases::{BuildError, PhaseCtx, PhaseRegistry, RegisterError},
};
use polaris::{
    graph::Graph,
    system::{
        param::{Out, Res},
        system,
    },
};
use polaris_cosmos::flow::{PhaseStatus, RunHandle};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// Scaffold inventory saved before task preparation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskInspection {
    /// Source chosen for this run, or the existing immutable snapshot.
    pub source: PathBuf,
    /// Required scaffold paths that are absent.
    pub missing: Vec<String>,
    /// Whether a sealed task contract was supplied.
    pub task_contract_present: bool,
}
/// Prepared immutable task path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskSnapshot(pub PathBuf);
/// Verified task-local runtime passed to baseline scoring.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskRuntime(pub runtime::RuntimeRecord);

/// Look up a configuration key using its dotted path.
pub fn config_value<'a>(cfg: &'a ScientistConfig, key: &str) -> Option<&'a toml::Value> {
    let mut parts = key.split('.');
    let mut value = cfg.values.get(parts.next()?)?;
    for part in parts {
        value = value.get(part)?;
    }
    Some(value)
}
/// Read a nonnegative integer setting with its public default.
pub fn number(cfg: &ScientistConfig, key: &str, default: u64) -> u64 {
    config_value(cfg, key)
        .and_then(toml::Value::as_integer)
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(default)
}
/// Read a boolean setting with its public default.
pub fn flag(cfg: &ScientistConfig, key: &str, default: bool) -> bool {
    config_value(cfg, key)
        .and_then(toml::Value::as_bool)
        .unwrap_or(default)
}
/// Read a string setting with its public default.
pub fn text(cfg: &ScientistConfig, key: &str, default: &str) -> String {
    config_value(cfg, key)
        .and_then(toml::Value::as_str)
        .unwrap_or(default)
        .into()
}

/// Read a list of strings; items that are not strings are ignored.
pub fn texts(cfg: &ScientistConfig, key: &str) -> Vec<String> {
    config_value(cfg, key)
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Inspect required scaffold entries without executing task code.
pub fn inspect(source: &Path) -> TaskInspection {
    let mut missing = Vec::new();
    for file in [
        "task.md",
        "evaluate.py",
        "requirements.txt",
        "program_0/program_spec.toml",
    ] {
        if !source.join(file).is_file() {
            missing.push(file.into());
        }
    }
    if !source.join("program_0").is_dir() {
        missing.push("program_0/".into());
    }
    TaskInspection {
        source: source.into(),
        missing,
        task_contract_present: source.join("task_contract.json").is_file(),
    }
}

/// Copy a complete task once; subsequent calls reuse exactly the first snapshot.
pub fn snapshot(source: &Path, root: &Path) -> Result<PathBuf, EvalError> {
    let task = root.join("task");
    let receipt = task.join("snapshot.json");
    if receipt.exists() {
        return Ok(task.canonicalize()?);
    }
    let check = inspect(source);
    if !check.missing.is_empty() {
        return Err(EvalError(format!(
            "missing task scaffold: {}",
            check.missing.join(", ")
        )));
    }
    fs::create_dir_all(root)?;
    if task.exists() {
        return Err(EvalError(
            "task snapshot exists without a completed snapshot receipt".into(),
        ));
    }
    let staging = root.join(".task-snapshot");
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    files::copy_tree(source, &staging)?;
    files::write_json(
        &staging.join("snapshot.json"),
        &serde_json::json!({"source":source,"task_contract_present":check.task_contract_present,"evaluator_sha256":eval::file_digest(&staging.join("evaluate.py"))?,"requirements_sha256":eval::file_digest(&staging.join("requirements.txt"))?,"task_md_sha256":eval::file_digest(&staging.join("task.md"))?,"program_0_sha256":eval::fingerprint(&staging.join("program_0"))?,"capture_sha256":if staging.join("capture.py").exists(){Some(eval::file_digest(&staging.join("capture.py"))?)}else{None}}),
    )?;
    fs::rename(staging, &task)?;
    Ok(task.canonicalize()?)
}

#[system]
async fn task_inspect(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
) -> Result<TaskInspection, polaris::system::system::SystemError> {
    let existing = run.root.join("task");
    let source = if existing.join("snapshot.json").exists() {
        existing
    } else {
        PathBuf::from(text(&cfg, "task.dir", ""))
    };
    let inspection = inspect(&source);
    files::write_json(
        &run.root.join("task_drafting/scaffold_inspection.json"),
        &inspection,
    )?;
    Ok(inspection)
}
#[system]
async fn missing_scaffold(
    inspection: Out<TaskInspection>,
) -> Result<(), polaris::system::system::SystemError> {
    Err(EvalError(format!(
        "missing task scaffold: {}",
        inspection.missing.join(", ")
    ))
    .into())
}
#[system]
async fn task_snapshot(
    run: Res<RunHandle>,
    inspection: Out<TaskInspection>,
) -> Result<TaskSnapshot, polaris::system::system::SystemError> {
    Ok(TaskSnapshot(snapshot(&inspection.source, &run.root)?))
}
#[system]
async fn task_runtime(
    cfg: Res<ScientistConfig>,
    snapshot: Out<TaskSnapshot>,
) -> Result<TaskRuntime, polaris::system::system::SystemError> {
    Ok(TaskRuntime(
        runtime::provision_with_extras(
            &snapshot.0,
            Path::new(&text(&cfg, "evaluation.python", "python3")),
            Path::new(&text(&cfg, "evaluation.wheelhouse", "/opt/wheelhouse")),
            &texts(&cfg, "evaluation.extra_packages"),
        )
        .await?,
    ))
}
/// Build an evaluator request from the verified runtime and campaign settings.
pub fn evaluator_request(
    root: &Path,
    cfg: &ScientistConfig,
    program: PathBuf,
    results: PathBuf,
) -> Result<EvalRequest, EvalError> {
    let runtime: runtime::RuntimeRecord =
        serde_json::from_slice(&fs::read(root.join("task/runtime.json"))?)?;
    if eval::file_digest(&root.join("task/evaluate.py"))? != runtime.evaluator_sha256 {
        return Err(EvalError(
            "task evaluator differs from verified runtime".into(),
        ));
    }
    Ok(EvalRequest {
        python: runtime.python,
        evaluator: root.join("task/evaluate.py"),
        program_dir: program,
        results_dir: results,
        timeout: Duration::from_secs(number(cfg, "evaluation.timeout_seconds", 300)),
        grace: Duration::from_secs(number(cfg, "evaluation.grace_seconds", 5)),
        environment: BTreeMap::new(),
    })
}
#[system]
async fn task_baseline(
    run: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    runtime: Out<TaskRuntime>,
) -> Result<PhaseStatus, polaris::system::system::SystemError> {
    let _ = &runtime.0;
    let task = run.root.join("task");
    let program = task.join("program_0");
    let results = task.join("baseline");
    let scored = eval::run_evaluator(evaluator_request(
        &run.root,
        &cfg,
        program.clone(),
        results.clone(),
    )?)
    .await?;
    if scored.score().is_none() {
        return Err(EvalError("baseline is not correct with a finite numeric score".into()).into());
    }
    let candidate =
        crate::phases::investigation::selection::candidate(&program, &results, "baseline")?
            .ok_or_else(|| EvalError("baseline selection failed".into()))?;
    files::write_json(&results.join("incumbent.json"), &candidate)?;
    let best = run.root.join("investigation/best_program_manifest.json");
    if !best.exists() {
        files::write_json(&best, &candidate)?;
    }
    Ok(PhaseStatus::Completed)
}
/// Register task capability resources.
pub fn register(_reg: &mut PhaseRegistry) -> Result<(), RegisterError> {
    Ok(())
}
/// Compose task systems; missing scaffolds enter the optional drafting capability.
pub fn build(mut graph: Graph, ctx: &PhaseCtx) -> Result<Graph, BuildError> {
    let mut inspection = Graph::new();
    inspection.system(task_inspect).named("task.inspect").done();
    graph.append(polaris_cosmos::flow::memo::<TaskInspection>(
        ctx.prefix.child("task.inspect")?,
        inspection,
    ))?;
    let drafting = flag(&ctx.config, "task.drafting", false);
    let draft_ctx = ctx.clone();
    let mut absent = Graph::new();
    if drafting {
        absent = super::drafting::build(absent, &draft_ctx)?;
    } else {
        absent.system(missing_scaffold).named("task.missing").done();
    }
    graph.add_conditional_branch::<TaskInspection, _, _, _>(
        "task.scaffold",
        |i| i.missing.is_empty(),
        |g| {
            g.system(task_snapshot).named("task.snapshot").done();
        },
        move |g| {
            g.append(absent).expect("independent drafting graph");
        },
    );
    let mut runtime = Graph::new();
    runtime.system(task_runtime).named("task.runtime").done();
    graph.append(polaris_cosmos::flow::memo::<TaskRuntime>(
        ctx.prefix.child("task.runtime")?,
        runtime,
    ))?;
    let mut baseline = Graph::new();
    baseline.system(task_baseline).named("task.baseline").done();
    graph.append(polaris_cosmos::flow::memo::<PhaseStatus>(
        ctx.prefix.child("task.baseline")?,
        baseline,
    ))?;
    Ok(graph)
}
