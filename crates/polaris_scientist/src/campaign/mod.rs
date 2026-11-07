//! Campaign graph construction, execution, conservative restart and phase reset.
pub use crate::records::Outcome;
mod graph;
use crate::{
    config::{ConfigFlags, PHASES, ScientistConfig},
    durable::{read_json, relative, timestamp, write_json},
    phases::{self, BuildError, PhaseCtx, PhaseRegistry},
    records::RunRecord,
};
pub use graph::{build, build_with_phases, generated_winner, unused_roles};
use polaris::{
    agent::Agent,
    graph::{Graph, GraphExecutor},
    sessions::{AgentTypeId, InMemoryStore, SessionsAPI, SessionsPlugin},
    system::{param::Res, resource::GlobalResource, server::Server, system, system::SystemError},
};
use polaris_cosmos::flow::{
    self, AgentRunner, EventLog, FlowSettings, HaltReason, LeaseBook, LedgerSettings, Micros,
    PhaseControl, PhaseStatus, RepairCounters, RoleBook, RunHandle, RunMode, SpendBook,
    StepJournal, StepKey, TaskRunner, Worklist, digest,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
/// The event log of a run, under the run root.
const EVENTS_FILE: &str = "pipeline_events.jsonl";
/// Campaign lifecycle error.
pub type CampaignError = Box<dyn std::error::Error + Send + Sync>;
#[derive(Clone)]
struct RunClock {
    start: Instant,
    previous_seconds: f64,
}
impl GlobalResource for RunClock {}
struct CampaignAgent {
    graph: Graph,
}
impl Agent for CampaignAgent {
    fn name(&self) -> &'static str {
        "ResearchCampaign"
    }
    fn build(&self, graph: &mut Graph) {
        graph
            .append(self.graph.duplicate())
            .expect("fresh campaign graph");
    }
}
/// Generates a sortable UTC run identifier.
pub fn new_run_id() -> String {
    chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string()
}
/// Computes the run root before constructing the in-process agent runner.
pub fn run_root(results_root: &Path, task_dir: &Path, run_id: &str) -> PathBuf {
    results_root
        .join(task_dir.file_name().unwrap_or_default())
        .join("polaris")
        .join(run_id)
}
/// Resolves assets for an installed binary or a workspace checkout.
pub fn assets_root() -> PathBuf {
    if let Some(path) = std::env::var_os("POLARIS_SCIENTIST_ASSETS") {
        return path.into();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
        && parent.join("assets").is_dir()
    {
        return parent.join("assets");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}
/// Opens and executes a new campaign.
pub async fn run(
    task_dir: &Path,
    cfg: ScientistConfig,
    runner: Arc<dyn TaskRunner>,
    run_id: Option<String>,
) -> Result<Outcome, CampaignError> {
    run_with_phases(task_dir, cfg, runner, run_id, None, assets_root()).await
}
/// Opens a campaign with optional phase substitutions and an explicit asset root.
/// Substitutions use the same graph lifecycle, spend book and restart machinery.
pub async fn run_with_phases(
    task_dir: &Path,
    mut cfg: ScientistConfig,
    runner: Arc<dyn TaskRunner>,
    run_id: Option<String>,
    bodies: Option<BTreeMap<String, Graph>>,
    assets: PathBuf,
) -> Result<Outcome, CampaignError> {
    let task = task_dir.canonicalize()?;
    let id = run_id
        .or_else(|| {
            let id = cfg.string("run_id");
            (!id.is_empty()).then(|| id.to_owned())
        })
        .unwrap_or_else(new_run_id);
    valid_id(&id)?;
    cfg.set("task.dir", toml::Value::String(task.display().to_string()))?;
    cfg.set("run_id", toml::Value::String(id.clone()))?;
    let root = run_root(&cfg.path("paths.results_root"), &task, &id);
    fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    if root.join("run.json").exists() {
        return Err("run already exists; use resume".into());
    }
    execute_run(&root, cfg, runner, id, RunMode::Run, bodies, assets).await
}
/// Continues a campaign while refusing graph-shape changes.
pub async fn resume(
    root: &Path,
    sets: &[String],
    max_usd: Option<Micros>,
    runner: Arc<dyn TaskRunner>,
) -> Result<Outcome, CampaignError> {
    resume_with_phases(root, sets, max_usd, runner, None, assets_root()).await
}
/// Continues a campaign with the same optional phase substitutions as its assembly.
pub async fn resume_with_phases(
    root: &Path,
    sets: &[String],
    max_usd: Option<Micros>,
    runner: Arc<dyn TaskRunner>,
    bodies: Option<BTreeMap<String, Graph>>,
    assets: PathBuf,
) -> Result<Outcome, CampaignError> {
    let root = root.canonicalize()?;
    let path = root.join("config.resolved.toml");
    let old = ScientistConfig::resolve(std::slice::from_ref(&path), &[], ConfigFlags::default())?;
    let cfg = ScientistConfig::resolve(
        &[path],
        sets,
        ConfigFlags {
            max_usd,
            ..ConfigFlags::default()
        },
    )?;
    old.compatible_resume(&cfg)?;
    let record: RunRecord = read_json(&root.join("run.json"))?;
    execute_run(
        &root,
        cfg,
        runner,
        record.run_id,
        RunMode::Resume,
        bodies,
        assets,
    )
    .await
}
/// The mode text kept in `run.json`.
fn mode_text(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Resume => "resume",
        _ => "run",
    }
}
fn valid_id(id: &str) -> Result<(), CampaignError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err("run id must contain only letters, digits, underscores and hyphens".into());
    }
    Ok(())
}
fn register_all(reg: &mut PhaseRegistry) -> Result<(), CampaignError> {
    for register in [
        phases::task::register,
        phases::investigation::register,
        phases::ablations::register,
        phases::final_eval::register,
        phases::posthoc::register,
        phases::drafting::register,
        phases::paper::register,
        phases::review::register,
    ] {
        register(reg)?;
    }
    Ok(())
}
async fn execute_run(
    root: &Path,
    cfg: ScientistConfig,
    runner: Arc<dyn TaskRunner>,
    id: String,
    mode: RunMode,
    bodies: Option<BTreeMap<String, Graph>>,
    assets: PathBuf,
) -> Result<Outcome, CampaignError> {
    let resuming = mode == RunMode::Resume;
    let roles = crate::roles::load(&assets.join("roles"), &cfg)?;
    let graph = if let Some(bodies) = bodies {
        build_with_phases(&cfg, &roles, bodies)?
    } else {
        build(&cfg, &roles)?
    };
    let shares = PHASES
        .iter()
        .map(|name| {
            Ok((
                name.to_string(),
                cfg.fraction(&format!("budget.cumulative_share.{name}"))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, CampaignError>>()?;
    let spend = SpendBook::open(
        root,
        &id,
        cfg.micros("budget.max_usd")?,
        shares,
        cfg.fraction("budget.min_grant_fraction")?,
    )?
    .with_ledger(LedgerSettings {
        schema_version: 3,
        agent: "polaris".into(),
        provider: "amazon-bedrock".into(),
        cost_basis: "list-price-estimate".into(),
    });
    let journal = StepJournal::open(root)?;
    if resuming {
        for reservation in spend.recover_abandoned_with_model(cfg.string("runner.model"))? {
            let path = root.join(relative(&reservation.artifact)?);
            journal.abandon(&path)?;
        }
        recover_incomplete_workspaces(root, &journal)?;
    }
    let mut limits = BTreeMap::from([(
        "paper.repair_round".into(),
        cfg.u64("paper.max_repair_rounds") as u32,
    )]);
    for key in ["design", "contract", "smoke", "alignment", "qualification"] {
        limits.insert(
            format!("draft.{key}"),
            cfg.u64(&format!("task_drafting.{key}_max_repairs")) as u32,
        );
    }
    let counters = RepairCounters::open(root, limits)?;
    let leases = LeaseBook::open(
        root,
        cfg.u64("investigation.execution.max_idea_cycles") as u32,
    )?;
    let work = Worklist::open(&root.join(".state/worklist.json"), vec![])?;
    // The configured list holds the main phases. The review phase always runs
    // and records its own skip when the review is off, so it is always enabled.
    let mut enabled: Vec<String> = cfg.get("campaign.phases")?;
    enabled.push("review".into());
    let phases = PhaseControl::open(
        root,
        enabled,
        BTreeMap::from([(
            "task_drafting".into(),
            std::time::Duration::from_secs(cfg.u64("task_drafting.timeout_seconds")),
        )]),
    )?;
    let previous_seconds = if root.join("outcome.json").exists() {
        read_json::<Outcome>(&root.join("outcome.json"))?.wall_seconds
    } else if resuming {
        let record: RunRecord = read_json(&root.join("run.json"))?;
        let started = chrono::DateTime::parse_from_rfc3339(&record.started)?;
        (chrono::Utc::now()
            .signed_duration_since(started)
            .num_milliseconds()
            .max(0) as f64)
            / 1000.0
    } else {
        0.0
    };
    if !previous_seconds.is_finite() || previous_seconds < 0.0 {
        return Err("invalid persisted wall time".into());
    }
    let mut server = Server::new();
    server.add_plugins(polaris::plugins::PersistencePlugin);
    server.add_plugins(SessionsPlugin::new(Arc::new(InMemoryStore::new())));
    let mut registry = PhaseRegistry::default();
    register_all(&mut registry)?;
    registry.install(&mut server);
    server.insert_global(RunHandle {
        run_id: id.clone(),
        root: root.into(),
        mode,
    });
    server.insert_global(FlowSettings {
        assets,
        task_dir: root.join("task"),
        model_id: cfg.string("runner.model").into(),
        ..FlowSettings::default()
    });
    let wall = cfg.u64("budget.wall_seconds");
    server.insert_global(if wall == 0 {
        flow::CallDeadline::new(None)
    } else {
        flow::CallDeadline::with_elapsed(
            std::time::Duration::from_secs(wall),
            std::time::Duration::from_secs_f64(previous_seconds),
        )
    });
    server.insert_global(cfg);
    server.insert_global(roles);
    server.insert_global(AgentRunner::new(runner));
    server.insert_global(spend);
    server.insert_global(journal);
    server.insert_global(counters);
    server.insert_global(leases);
    server.insert_global(work);
    server.insert_global(phases);
    server.insert_global(EventLog::open(root, &id).with_file_name(EVENTS_FILE)?);
    server.insert_global(RunClock {
        start: Instant::now(),
        previous_seconds,
    });
    server.finish().await?;
    let sessions = server
        .api::<SessionsAPI>()
        .ok_or("sessions capability missing")?
        .clone();
    sessions.register_agent(CampaignAgent { graph })?;
    let outcome = sessions
        .run_oneshot_with_executor::<Outcome>(
            &AgentTypeId::from_name("ResearchCampaign"),
            GraphExecutor::new().with_default_max_iterations(100_000),
            |_| {},
        )
        .await?;
    Ok(outcome)
}
#[system]
async fn open_node(
    handle: Res<RunHandle>,
    cfg: Res<ScientistConfig>,
    roles: Res<RoleBook>,
    events: Res<EventLog>,
) -> Result<(), SystemError> {
    let result = (|| -> Result<(), CampaignError> {
        for path in [
            "metrics",
            ".state/steps",
            "work",
            "task_drafting",
            "investigation",
            "ablations",
            "final_evaluation",
            "posthoc",
            "paper",
        ] {
            fs::create_dir_all(handle.root.join(path))?;
        }
        if !handle.root.join("run.json").exists() {
            let task = cfg.path("task.dir");
            let evaluator = fs::read(task.join("evaluate.py")).ok().map(|b| digest(&b));
            write_json(
                &handle.root.join("run.json"),
                &RunRecord {
                    run_id: handle.run_id.clone(),
                    mode: mode_text(handle.mode).into(),
                    started: timestamp(),
                    binary_version: env!("CARGO_PKG_VERSION").into(),
                    task_path: task,
                    evaluator_sha256: evaluator,
                },
            )?;
        }
        cfg.write(&handle.root.join("config.resolved.toml"))?;
        roles.write(&handle.root.join("roles.resolved.toml"))?;
        events.emit("pipeline_start",None,json!({"task":cfg.string("task.dir"),"run_id":handle.run_id,"phases":cfg.get::<Vec<String>>("campaign.phases")?,"agent":{"model":cfg.string("runner.model"),"name":"polaris"},"execution":cfg.values,"paths":{"results_dir":handle.root}}))?;
        Ok(())
    })();
    result.map_err(|e| SystemError::ExecutionError(e.to_string()))
}
#[system]
async fn close_node(
    handle: Res<RunHandle>,
    control: Res<PhaseControl>,
    spend: Res<SpendBook>,
    events: Res<EventLog>,
    clock: Res<RunClock>,
) -> Result<Outcome, SystemError> {
    let result = (|| -> Result<Outcome, CampaignError> {
        let summary = spend.summary()?;
        write_json(
            &handle.root.join("metrics/cost_summary.json"),
            &serde_json::to_value(&summary)?,
        )?;
        let statuses = control.statuses();
        let outcome = match control.halt_reason() {
            Some(HaltReason::Interrupted) => "interrupted",
            Some(_) => "failed",
            None if control.incomplete() => "paper_incomplete",
            None if !generated_winner(&handle.root) => "completed_no_winner",
            None => "completed",
        };
        let lines = read_event_lines(&handle.root.join(EVENTS_FILE))?;
        let caps_hit = lines
            .iter()
            .filter(|e| e["event"] == "cap_hit")
            .map(|e| e["fields"].clone())
            .collect();
        let skipped_steps = lines
            .iter()
            .filter(|e| e["event"] == "step_skipped")
            .map(|e| e["fields"].clone())
            .collect();
        let outcome = Outcome {
            outcome: outcome.into(),
            phases: statuses
                .iter()
                .map(|(name, status)| (name.clone(), flow::phase_status(status).0.into()))
                .collect(),
            usd_settled: summary.total_cost_usd.to_string(),
            usd_reserved_unspent: summary.reserved_usd.to_string(),
            wall_seconds: clock.previous_seconds + clock.start.elapsed().as_secs_f64(),
            caps_hit,
            skipped_steps,
            models_called: summary.models_called,
            degraded_reasons: statuses
                .values()
                .filter_map(|status| {
                    if let PhaseStatus::Degraded(note) = status {
                        Some(note.clone())
                    } else {
                        None
                    }
                })
                .collect(),
        };
        events.emit("cost_summary",None,json!({"label":"campaign","ledger":"metrics/cost_events.jsonl","total_cost_usd":outcome.usd_settled,"message":"list-price estimate"}))?;
        events.emit("pipeline_end",None,json!({"status":outcome.outcome,"outcome":outcome.outcome,"return_code":match outcome.outcome.as_str(){"interrupted"=>4,"failed"|"paper_incomplete"=>1,_=>0},"elapsed_seconds":outcome.wall_seconds}))?;
        write_json(&handle.root.join("outcome.json"), &outcome)?;
        Ok(outcome)
    })();
    result.map_err(|e| SystemError::ExecutionError(e.to_string()))
}
fn read_event_lines(path: &Path) -> Result<Vec<Value>, CampaignError> {
    if !path.exists() {
        return Ok(vec![]);
    }
    fs::read_to_string(path)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}
fn recover_incomplete_workspaces(root: &Path, journal: &StepJournal) -> Result<(), CampaignError> {
    let work = root.join("work");
    if !work.exists() {
        return Ok(());
    }
    let markers = walkdir::WalkDir::new(&work)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| e.file_name() != "_abandoned")
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() && entry.file_name() == ".invocation.json" => {
                Some(Ok(entry.path().to_owned()))
            }
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    for marker in markers {
        let value: Value = read_json(&marker)?;
        let key = StepKey::new(value["step"].as_str().ok_or("invalid invocation marker")?)?;
        if journal.get(&key).is_none() {
            journal.abandon(marker.parent().unwrap())?;
        }
    }
    Ok(())
}
/// Drops a phase's and later completion records, archiving their work and artifacts.
pub fn reset(root: &Path, from: &str) -> Result<(), CampaignError> {
    let _: RunRecord = read_json(&root.join("run.json"))?;
    let index = PHASES
        .iter()
        .position(|phase| *phase == from)
        .ok_or_else(|| format!("unknown phase {from}"))?;
    let affected = &PHASES[index..];
    let spend_path = root.join("metrics/spend_state.json");
    if spend_path.exists() {
        let spend: Value = read_json(&spend_path)?;
        if spend["reserved"]
            .as_object()
            .is_some_and(|calls| !calls.is_empty())
        {
            return Err("cannot reset while agent calls remain reserved; resume first".into());
        }
    }
    let mut prefixes = affected
        .iter()
        .map(|phase| phase.to_string())
        .chain(["review".into(), "revision".into()])
        .collect::<Vec<_>>();
    if affected.contains(&"task_drafting") {
        prefixes.extend(["task".into(), "draft".into()]);
    }
    if affected.contains(&"investigation") {
        prefixes.push("inv".into());
    }
    if affected.contains(&"ablations") {
        prefixes.push("abl".into());
    }
    if affected.contains(&"posthoc") {
        prefixes.push("ph".into());
    }
    let archive_root = root.join("_reset");
    fs::create_dir_all(&archive_root)?;
    let mut ordinal = 0;
    let archive = loop {
        let path = archive_root.join(format!("{}-{}-{ordinal}", new_run_id(), std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => break path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ordinal += 1,
            Err(e) => return Err(e.into()),
        }
    };
    for prefix in &prefixes {
        for base in [
            PathBuf::new(),
            PathBuf::from("work"),
            PathBuf::from(".state/steps"),
        ] {
            let path = root.join(&base).join(prefix);
            if path.exists() {
                let dest = archive.join(&base).join(prefix);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(path, dest)?;
            }
        }
        let record = root.join(".state/steps").join(format!("{prefix}.json"));
        if record.exists() {
            fs::remove_file(record)?;
        }
    }
    let affected_key = |key: &str| {
        prefixes.iter().any(|prefix| {
            key == prefix
                || key.starts_with(&format!("{prefix}/"))
                || key.starts_with(&format!("{prefix}."))
        })
    };
    let phase_path = root.join("pipeline_state.json");
    if phase_path.exists() {
        let mut state: BTreeMap<String, PhaseStatus> = read_json(&phase_path)?;
        state.retain(|name, _| !affected_key(name));
        write_json(&phase_path, &state)?;
    }
    if affected.contains(&"investigation") {
        let path = root.join(".state/leases.json");
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    let counter_path = root.join(".state/counters.json");
    if counter_path.exists() {
        let mut state: Value = read_json(&counter_path)?;
        for field in ["used", "limits"] {
            if let Some(map) = state[field].as_object_mut() {
                map.retain(|name, _| !affected_key(name));
            }
        }
        write_json(&counter_path, &state)?;
    }
    let queue_path = root.join(".state/worklist.json");
    if queue_path.exists() {
        let mut items: Vec<flow::WorkItem> = read_json(&queue_path)?;
        items.retain(|item| !affected_key(item.key.as_str()));
        write_json(&queue_path, &items)?;
    }
    for name in ["outcome.json", "metrics/cost_summary.json"] {
        let path = root.join(name);
        if path.exists() {
            let dest = archive.join(name);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(path, dest)?;
        }
    }
    // Settled money and invocation numbering are preserved across resets.
    Ok(())
}
