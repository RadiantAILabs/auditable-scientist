//! What each `polaris-scientist` command does.
//!
//! Everything a command needs from outside — the stop flag, the way an agent
//! session is started, the claims checker, whether the process is offline —
//! comes in through [`Host`], so a test can supply each one and see what the
//! command does with it. Output goes to the two writers in [`Io`]; the
//! commands never print on their own.

use std::fmt::Display;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use polaris_cosmos::flow::{
    AgentLimits, AgentRequest, Micros, ScriptedRunner, StepKey, StopFlag, TaskRunner, ToolGrant,
    parse_micros,
};
use polaris_scientist::campaign;
use polaris_scientist::config::{ConfigFlags, PHASES, ScientistConfig};

use crate::claims_check::{self, ClaimsChecker};
use crate::cli::{
    AgentCallArgs, CheckClaimsArgs, Command, ConfigArgs, ResetArgs, ResumeArgs, RunArgs,
};
use crate::exit::Exit;
use crate::runner::{RunnerConfig, RunnerError, SessionRunner};

/// The environment variable that puts the process offline: no model is ever
/// called, and a campaign may run on a scripted runner.
pub const OFFLINE_VAR: &str = "POLARIS_SCIENTIST_OFFLINE";

/// The future that starts an agent session runner.
pub type StartFuture = Pin<Box<dyn Future<Output = Result<SessionRunner, RunnerError>>>>;

/// Starts the runner that makes real model calls.
pub type SessionStarter = Box<dyn Fn(RunnerConfig) -> StartFuture + Send + Sync>;

/// The two output streams of a command.
pub struct Io<'a> {
    /// Standard output: what the command produces (a report, a configuration).
    pub out: &'a mut dyn Write,
    /// Standard error: progress and problems.
    pub err: &'a mut dyn Write,
}

impl Io<'_> {
    fn note(&mut self, text: impl Display) {
        let _ = writeln!(self.err, "{text}");
    }

    fn usage(&mut self, text: impl Display) -> Exit {
        self.note(format_args!("polaris-scientist: {text}"));
        Exit::Usage
    }

    fn failure(&mut self, text: impl Display) -> Exit {
        self.note(format_args!("polaris-scientist: {text}"));
        Exit::Failed
    }
}

/// What the commands run against.
pub struct Host {
    /// Whether the process is offline. When it is, no session runner is ever
    /// built and every command that needs a model refuses.
    pub offline: bool,
    /// Raised to stop every call in flight and every call after.
    pub stop: StopFlag,
    /// Builds the runner that calls the model. Never called when offline.
    pub start_session: SessionStarter,
    /// The claims checker, if this build carries one.
    pub claims: Option<Arc<dyn ClaimsChecker>>,
}

impl Host {
    /// The host for the real process: the model runner on Bedrock, the
    /// checker this build links, and the offline setting from the
    /// environment.
    #[must_use]
    pub fn for_process(stop: StopFlag) -> Self {
        Self {
            offline: offline_requested(),
            stop,
            start_session: Box::new(|config| {
                Box::pin(async move { SessionRunner::start(&config).await })
            }),
            claims: claims_check::linked(),
        }
    }
}

/// Whether [`OFFLINE_VAR`] is set to `1` in the process environment.
#[must_use]
pub fn offline_requested() -> bool {
    std::env::var(OFFLINE_VAR).as_deref() == Ok("1")
}

/// Run `command`.
pub async fn execute(host: &Host, command: Command, io: &mut Io<'_>) -> Exit {
    match command {
        Command::Run(args) => run(host, args, io).await,
        Command::Resume(args) => resume(host, args, io).await,
        Command::Reset(args) => reset(&args, io),
        Command::CheckClaims(args) => check_claims(host, &args, io),
        Command::Config(args) => show_config(&args, io),
        Command::AgentCall(args) => agent_call(host, args, io).await,
    }
}

/// Read a spend ceiling in dollars as exact micro-dollars.
///
/// At most six decimals are accepted, so no figure is ever rounded: `13.5` is
/// `13_500_000`.
///
/// # Errors
///
/// Plain words when `text` is not a plain non-negative dollar amount.
pub fn parse_ceiling(text: &str) -> Result<Micros, String> {
    parse_micros(text.trim()).map_err(|_| {
        format!("{text:?} is not a dollar amount; use digits with at most six decimals, like 13.5")
    })
}

/// The ceiling a command that spends money was given.
///
/// Required, and above zero, unless the process is offline.
fn ceiling(host: &Host, given: Option<&str>) -> Result<Option<Micros>, String> {
    match given {
        None if host.offline => Ok(None),
        None => Err("--max-usd is required; say how much the run may spend".to_string()),
        Some(text) => {
            let micros = parse_ceiling(text)?;
            if micros == 0 && !host.offline {
                return Err("--max-usd must be above zero".to_string());
            }
            Ok(Some(micros))
        }
    }
}

fn load_config(
    files: &[PathBuf],
    sets: &[String],
    flags: ConfigFlags,
) -> Result<ScientistConfig, String> {
    ScientistConfig::resolve(files, sets, flags).map_err(|error| error.to_string())
}

/// A run name the campaign accepts.
fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// What a campaign calls its role calls on, and the session runner to clean
/// up afterwards.
struct Built {
    runner: Arc<dyn TaskRunner>,
    session: Option<Arc<SessionRunner>>,
}

impl Built {
    async fn release(self) {
        if let Some(session) = self.session {
            session.shutdown().await;
        }
    }
}

/// Pick and build the runner the configuration asks for.
///
/// Offline, only a scripted runner is allowed and the session runner is never
/// built; online, only the session runner is.
async fn build_runner(
    host: &Host,
    cfg: &ScientistConfig,
    results_root: &Path,
    run_root: &Path,
    io: &mut Io<'_>,
) -> Result<Built, Exit> {
    let scripted = cfg.string("runner.kind") == "scripted";
    if host.offline != scripted {
        return Err(io.usage(if host.offline {
            format!(
                "{OFFLINE_VAR}=1 allows only a scripted runner; set runner.kind to \"scripted\" \
                 and runner.scenario to a scenario directory"
            )
        } else {
            format!("runner.kind=\"scripted\" needs {OFFLINE_VAR}=1")
        }));
    }
    if scripted {
        let scenario = cfg.string("runner.scenario");
        if scenario.is_empty() {
            return Err(io.usage("runner.scenario must name a scenario directory"));
        }
        let runner = ScriptedRunner::load(Path::new(scenario), &run_root.join("scripted"))
            .map_err(|error| io.usage(format!("scenario {scenario} cannot be used: {error}")))?
            .with_stop(host.stop.clone());
        return Ok(Built {
            runner: Arc::new(runner),
            session: None,
        });
    }
    let config =
        RunnerConfig::from_config(cfg, results_root, run_root).map_err(|error| io.usage(error))?;
    let session = (host.start_session)(config)
        .await
        .map_err(|error| io.usage(error))?
        .with_stop(host.stop.clone());
    let session = Arc::new(session);
    Ok(Built {
        runner: session.clone(),
        session: Some(session),
    })
}

/// Report how a campaign ended and pick the exit.
fn finish(result: Result<campaign::Outcome, campaign::CampaignError>, io: &mut Io<'_>) -> Exit {
    match result {
        Ok(outcome) => {
            io.note(format_args!(
                "outcome: {}; settled ${}",
                outcome.outcome, outcome.usd_settled
            ));
            Exit::of_outcome(&outcome.outcome)
        }
        Err(error) => io.failure(error),
    }
}

async fn run(host: &Host, args: RunArgs, io: &mut Io<'_>) -> Exit {
    let max_usd = match ceiling(host, args.max_usd.as_deref()) {
        Ok(max_usd) => max_usd,
        Err(message) => return io.usage(message),
    };
    let task = match args.task_dir.canonicalize() {
        Ok(task) if task.is_dir() => task,
        _ => {
            return io.usage(format_args!(
                "the task directory {} does not exist",
                args.task_dir.display()
            ));
        }
    };
    let results_root = match std::fs::create_dir_all(&args.results_root)
        .and_then(|()| args.results_root.canonicalize())
    {
        Ok(path) => path,
        Err(error) => {
            return io.usage(format_args!(
                "the results root {} cannot be used: {error}",
                args.results_root.display()
            ));
        }
    };
    let flags = ConfigFlags {
        max_usd,
        results_root: Some(results_root.clone()),
        run_id: args.run_id.clone(),
        task_dir: Some(task.clone()),
    };
    let mut cfg = match load_config(&args.config, &args.set, flags) {
        Ok(cfg) => cfg,
        Err(message) => return io.usage(message),
    };
    let id = args
        .run_id
        .or_else(|| Some(cfg.string("run_id").to_string()).filter(|id| !id.is_empty()))
        .unwrap_or_else(campaign::new_run_id);
    if !valid_run_id(&id) {
        return io.usage("the run id may hold only letters, digits, '_' and '-'");
    }
    if let Err(error) = cfg.set("run_id", toml::Value::String(id.clone())) {
        return io.usage(error);
    }
    let run_root = campaign::run_root(&results_root, &task, &id);
    let built = match build_runner(host, &cfg, &results_root, &run_root, io).await {
        Ok(built) => built,
        Err(exit) => return exit,
    };
    let result = campaign::run(&task, cfg, built.runner.clone(), Some(id)).await;
    built.release().await;
    finish(result, io)
}

async fn resume(host: &Host, args: ResumeArgs, io: &mut Io<'_>) -> Exit {
    let max_usd = match ceiling(host, args.max_usd.as_deref()) {
        Ok(max_usd) => max_usd,
        Err(message) => return io.usage(message),
    };
    let saved = args.root.join("config.resolved.toml");
    let root = match args.root.canonicalize() {
        Ok(root) if saved.is_file() => root,
        _ => {
            return io.usage(format_args!(
                "{} is not a run directory: it holds no config.resolved.toml",
                args.root.display()
            ));
        }
    };
    let saved = root.join("config.resolved.toml");
    let files = [saved];
    let (old, new) = match (
        load_config(&files, &[], ConfigFlags::default()),
        load_config(
            &files,
            &args.set,
            ConfigFlags {
                max_usd,
                ..ConfigFlags::default()
            },
        ),
    ) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(message), _) | (_, Err(message)) => return io.usage(message),
    };
    if let Err(error) = old.compatible_resume(&new) {
        return io.usage(error);
    }
    let results_root = new.path("paths.results_root");
    let built = match build_runner(host, &new, &results_root, &root, io).await {
        Ok(built) => built,
        Err(exit) => return exit,
    };
    let result = campaign::resume(&root, &args.set, max_usd, built.runner.clone()).await;
    built.release().await;
    finish(result, io)
}

fn reset(args: &ResetArgs, io: &mut Io<'_>) -> Exit {
    let root = match args.root.canonicalize() {
        Ok(root) if root.is_dir() => root,
        _ => {
            return io.usage(format_args!(
                "the run directory {} does not exist",
                args.root.display()
            ));
        }
    };
    if !PHASES.contains(&args.from.as_str()) {
        return io.usage(format_args!(
            "unknown phase {:?}; the phases are {}",
            args.from,
            PHASES.join(", ")
        ));
    }
    match campaign::reset(&root, &args.from) {
        Ok(()) => {
            io.note(format_args!("reset from {}", args.from));
            Exit::Done
        }
        Err(error) => io.failure(error),
    }
}

fn check_claims(host: &Host, args: &CheckClaimsArgs, io: &mut Io<'_>) -> Exit {
    if !args.paper_dir.is_dir() {
        return io.usage(format_args!(
            "the paper directory {} does not exist",
            args.paper_dir.display()
        ));
    }
    let Some(checker) = host.claims.as_ref() else {
        return io.failure("this build carries no claims checker");
    };
    let report = match checker.check(&args.paper_dir) {
        Ok(report) => report,
        Err(message) => return io.failure(message),
    };
    let mut text = match serde_json::to_string_pretty(&report) {
        Ok(text) => text,
        Err(error) => return io.failure(error),
    };
    text.push('\n');
    if args.out.as_os_str() == "-" {
        return match io.out.write_all(text.as_bytes()) {
            Ok(()) => Exit::Done,
            Err(error) => io.failure(format_args!("the report cannot be written: {error}")),
        };
    }
    let written = match args
        .out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => std::fs::create_dir_all(parent),
        None => Ok(()),
    }
    .and_then(|()| std::fs::write(&args.out, text));
    match written {
        Ok(()) => Exit::Done,
        Err(error) => io.failure(format_args!(
            "the report {} cannot be written: {error}",
            args.out.display()
        )),
    }
}

fn show_config(args: &ConfigArgs, io: &mut Io<'_>) -> Exit {
    let cfg = match load_config(&args.config, &args.set, ConfigFlags::default()) {
        Ok(cfg) => cfg,
        Err(message) => return io.usage(message),
    };
    match toml::to_string_pretty(&cfg.values) {
        Ok(text) => match io.out.write_all(text.as_bytes()) {
            Ok(()) => Exit::Done,
            Err(error) => io.failure(error),
        },
        Err(error) => io.failure(error),
    }
}

fn read_file(path: &Path, what: &str, io: &mut Io<'_>) -> Result<String, Exit> {
    std::fs::read_to_string(path).map_err(|error| {
        io.usage(format_args!(
            "the {what} {} cannot be read: {error}",
            path.display()
        ))
    })
}

async fn agent_call(host: &Host, args: AgentCallArgs, io: &mut Io<'_>) -> Exit {
    if host.offline {
        return io.usage(format_args!(
            "agent-call makes a real model call and is refused while {OFFLINE_VAR}=1"
        ));
    }
    let max_micros = match ceiling(host, args.max_usd.as_deref()) {
        Ok(Some(micros)) => micros,
        Ok(None) => return io.usage("--max-usd is required"),
        Err(message) => return io.usage(message),
    };
    let system_prompt = match read_file(&args.role_prompt, "role prompt", io) {
        Ok(text) => text,
        Err(exit) => return exit,
    };
    let turn_prompt = match read_file(&args.task, "task file", io) {
        Ok(text) => text,
        Err(exit) => return exit,
    };
    let results_root = match args.results_root.canonicalize() {
        Ok(path) if path.is_dir() => path,
        _ => {
            return io.usage(format_args!(
                "the results root {} does not exist",
                args.results_root.display()
            ));
        }
    };
    let workspace = match args.workspace.canonicalize() {
        Ok(path) if path.is_dir() => path,
        _ => {
            return io.usage(format_args!(
                "the workspace {} does not exist",
                args.workspace.display()
            ));
        }
    };
    let mut config = match RunnerConfig::from_config(
        &ScientistConfig::default(),
        &results_root,
        &results_root,
    ) {
        Ok(config) => config,
        Err(error) => return io.usage(error),
    };
    config.venv = results_root.join(".venv");
    let session = match (host.start_session)(config).await {
        Ok(session) => session.with_stop(host.stop.clone()),
        Err(error) => return io.usage(error),
    };
    let request = AgentRequest {
        step: StepKey::new("agent_call").expect("`agent_call` is a valid step key"),
        role: "agent_call".to_string(),
        workspace,
        system_prompt,
        turn_prompt,
        limits: AgentLimits {
            max_turns: args.max_turns,
            max_micros,
            timeout: Duration::from_secs(args.timeout_seconds),
            max_output_tokens: None,
        },
        grants: ToolGrant {
            edit: args.edit,
            commands: args.command,
        },
    };
    let result = session.run(request).await;
    session.shutdown().await;
    match result {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(text) => match writeln!(io.out, "{text}") {
                Ok(()) => Exit::Done,
                Err(error) => io.failure(error),
            },
            Err(error) => io.failure(error),
        },
        Err(failure) => io.failure(format_args!(
            "the call could not be measured or run: {failure:?}"
        )),
    }
}
