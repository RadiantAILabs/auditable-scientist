//! The commands, run in process against a stand-in host: no model, no
//! network, no credential, no container.
//!
//! The environment is process-wide, so every test takes one lock and sets the
//! variables it needs.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use clap::Parser as _;
use polaris_cosmos::flow::StopFlag;
use polaris_scientist::config::{ConfigFlags, ScientistConfig};
use polaris_scientist_run::claims_check::ClaimsChecker;
use polaris_scientist_run::cli::{Cli, VERSION};
use polaris_scientist_run::commands::{
    Host, Io, OFFLINE_VAR, SessionStarter, execute, parse_ceiling,
};
use polaris_scientist_run::exit::Exit;
use polaris_scientist_run::runner::{RunnerConfig, RunnerError, SessionRunner};
use serde_json::json;

use support::{MODEL_ID, Reply, ScriptedModelPlugin, ScriptedProvider, billed, text};

static ENV: Mutex<()> = Mutex::new(());

const AWS_VARS: [&str; 5] = [
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
];

/// Hold the environment lock, with the offline variable set or removed and
/// every AWS variable removed.
fn environment(offline: bool) -> MutexGuard<'static, ()> {
    let guard = ENV
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: every test that touches the environment holds the lock, and the
    // runtimes the tests build are created after this call.
    unsafe {
        if offline {
            std::env::set_var(OFFLINE_VAR, "1");
        } else {
            std::env::remove_var(OFFLINE_VAR);
        }
        for name in AWS_VARS {
            std::env::remove_var(name);
        }
    }
    guard
}

/// What the stand-in session starter was asked.
#[derive(Clone, Default)]
struct Starts {
    count: Arc<AtomicUsize>,
    configs: Arc<Mutex<Vec<RunnerConfig>>>,
}

impl Starts {
    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn last(&self) -> RunnerConfig {
        self.configs
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a start")
    }

    /// A starter that counts, then refuses as a missing credential would.
    fn refusing(&self) -> SessionStarter {
        let starts = self.clone();
        Box::new(move |config| {
            starts.record(config);
            Box::pin(async { Err(RunnerError::Provider("no credentials were found".into())) })
        })
    }

    /// A starter that counts, then starts a runner on a scripted model.
    fn scripted(&self, provider: &ScriptedProvider) -> SessionStarter {
        let starts = self.clone();
        let provider = provider.clone();
        Box::new(move |mut config| {
            starts.record(config.clone());
            config.model = MODEL_ID.to_string();
            let provider = provider.clone();
            Box::pin(async move {
                SessionRunner::start_with(&config, move |server| {
                    server.add_plugins(ScriptedModelPlugin { provider });
                })
                .await
            })
        })
    }

    fn record(&self, config: RunnerConfig) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.configs.lock().unwrap().push(config);
    }
}

/// What a command printed and how it ended.
struct Ran {
    exit: Exit,
    out: String,
    err: String,
}

fn host(offline: bool, start_session: SessionStarter) -> Host {
    Host {
        offline,
        stop: StopFlag::default(),
        start_session,
        claims: None,
    }
}

/// Parse `args` and run the command on `host`.
fn run_command(host: &Host, args: &[&str]) -> Ran {
    let cli = Cli::try_parse_from(std::iter::once("polaris-scientist").chain(args.iter().copied()))
        .unwrap_or_else(|error| panic!("{args:?} did not parse: {error}"));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(execute(
            host,
            cli.command,
            &mut Io {
                out: &mut out,
                err: &mut err,
            },
        ));
    Ran {
        exit,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// A task directory and an empty scenario directory.
struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("task")).unwrap();
        std::fs::write(dir.path().join("task/evaluate.py"), "print(0)\n").unwrap();
        std::fs::create_dir_all(dir.path().join("scenario")).unwrap();
        std::fs::write(
            dir.path().join("scenario/scenario.toml"),
            "[default.usage]\nmodel_id = \"aws.bedrock/global.anthropic.claude-sonnet-5-5\"\n\
             input = 10\noutput = 1\nrequests = 1\nmicros = 100\n",
        )
        .unwrap();
        Self { dir }
    }

    fn task(&self) -> PathBuf {
        self.dir.path().join("task")
    }

    fn results(&self) -> PathBuf {
        self.dir.path().join("results")
    }

    fn scenario(&self) -> PathBuf {
        self.dir.path().join("scenario")
    }

    /// `--set` arguments that choose the scripted runner on this scenario.
    fn scripted_sets(&self) -> Vec<String> {
        vec![
            "runner.kind=\"scripted\"".to_string(),
            format!("runner.scenario=\"{}\"", self.scenario().display()),
        ]
    }
}

#[test]
fn version_names_the_build_the_agent_library_and_the_target() {
    let error = Cli::try_parse_from(["polaris-scientist", "--version"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
    let shown = error.to_string();
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), 3, "{shown}");
    assert!(lines[0].starts_with("polaris-scientist "), "{shown}");
    assert!(lines[0].contains(env!("CARGO_PKG_VERSION")), "{shown}");
    assert!(
        lines[0].contains(" (") && lines[0].ends_with(')'),
        "the build id is in brackets: {shown}"
    );
    let code_version = lines[1].strip_prefix("polaris_code ").expect(&shown);
    assert!(
        code_version.chars().next().unwrap().is_ascii_digit(),
        "a version number, not a placeholder: {shown}"
    );
    let target = lines[2].strip_prefix("target ").expect(&shown);
    assert!(target.contains('-') && target != "unknown", "{shown}");
    assert!(shown.contains(VERSION));
}

#[test]
fn ceilings_are_read_exactly_in_micro_dollars() {
    for (text, micros) in [
        ("13.5", 13_500_000),
        ("13", 13_000_000),
        ("0.000001", 1),
        ("0.1", 100_000),
        ("0", 0),
        ("2.250000", 2_250_000),
        ("1000000", 1_000_000_000_000),
    ] {
        assert_eq!(parse_ceiling(text), Ok(micros), "{text}");
    }
    for text in [
        "",
        "-1",
        "13.1234567",
        "1e3",
        "abc",
        "1,5",
        "$5",
        ".5",
        "1 000",
        "NaN",
        "99999999999999999999",
    ] {
        assert!(parse_ceiling(text).is_err(), "{text:?} is refused");
    }
}

#[test]
fn an_unknown_configuration_key_is_a_usage_error_and_nothing_starts() {
    let _env = environment(false);
    let starts = Starts::default();
    let host = host(false, starts.refusing());
    let ran = run_command(&host, &["config", "--print", "--set", "nothing.here=1"]);
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains("unknown configuration key"), "{}", ran.err);
    assert!(ran.out.is_empty());
    assert_eq!(starts.count(), 0);
}

#[test]
fn a_run_without_a_ceiling_is_a_usage_error_unless_offline() {
    let _env = environment(false);
    let work = Workspace::new();
    let starts = Starts::default();
    let host = host(false, starts.refusing());
    let ran = run_command(
        &host,
        &[
            "run",
            path(&work.task()),
            "--results-root",
            path(&work.results()),
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains("--max-usd"), "{}", ran.err);
    assert_eq!(starts.count(), 0, "no session was started");

    let resumed = run_command(&host, &["resume", "--root", path(&work.results())]);
    assert_eq!(resumed.exit, Exit::Usage);
    assert!(resumed.err.contains("--max-usd"), "{}", resumed.err);

    for bad in ["0", "-3", "lots", "1.0000001"] {
        let given = format!("--max-usd={bad}");
        let ran = run_command(
            &host,
            &[
                "run",
                path(&work.task()),
                "--results-root",
                path(&work.results()),
                &given,
            ],
        );
        assert_eq!(ran.exit, Exit::Usage, "{given}");
    }
    assert_eq!(starts.count(), 0);
}

#[test]
fn a_scripted_runner_without_the_offline_variable_is_refused() {
    let _env = environment(false);
    let work = Workspace::new();
    let starts = Starts::default();
    let host = host(false, starts.refusing());
    let mut args = vec![
        "run".to_string(),
        path(&work.task()).to_string(),
        "--results-root".to_string(),
        path(&work.results()).to_string(),
        "--max-usd".to_string(),
        "5".to_string(),
    ];
    for set in work.scripted_sets() {
        args.push("--set".to_string());
        args.push(set);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = run_command(&host, &args);
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains(OFFLINE_VAR), "{}", ran.err);
    assert_eq!(starts.count(), 0);
    assert!(!work.results().join("task").exists(), "nothing was run");
}

#[test]
fn offline_the_session_runner_is_never_built() {
    let _env = environment(true);
    let work = Workspace::new();
    let starts = Starts::default();
    let host = host(true, starts.refusing());

    // The default runner is the model runner: refused, and not built.
    let ran = run_command(
        &host,
        &[
            "run",
            path(&work.task()),
            "--results-root",
            path(&work.results()),
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains("scripted"), "{}", ran.err);

    // A scripted run goes ahead, still without it.
    let mut args = vec![
        "run".to_string(),
        path(&work.task()).to_string(),
        "--results-root".to_string(),
        path(&work.results()).to_string(),
    ];
    for set in work.scripted_sets() {
        args.push("--set".to_string());
        args.push(set);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = run_command(&host, &args);
    assert_ne!(ran.exit, Exit::Usage, "{}", ran.err);
    assert_eq!(starts.count(), 0, "the session runner was never built");

    // A call to the model is refused outright.
    let ran = run_command(
        &host,
        &[
            "agent-call",
            "--results-root",
            path(&work.results()),
            "--workspace",
            path(&work.results()),
            "--role-prompt",
            path(&work.task().join("evaluate.py")),
            "--task",
            path(&work.task().join("evaluate.py")),
            "--max-usd",
            "1",
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains(OFFLINE_VAR), "{}", ran.err);
    assert_eq!(starts.count(), 0);
}

#[test]
fn a_missing_credential_is_a_usage_error() {
    let _env = environment(false);
    let work = Workspace::new();
    let starts = Starts::default();
    let host = host(false, starts.refusing());
    let ran = run_command(
        &host,
        &[
            "run",
            path(&work.task()),
            "--results-root",
            path(&work.results()),
            "--max-usd",
            "13.5",
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains("no credentials were found"), "{}", ran.err);
    assert_eq!(starts.count(), 1);
    let config = starts.last();
    assert_eq!(config.results_root, work.results().canonicalize().unwrap());
    assert!(config.venv.ends_with("task/.venv"), "{:?}", config.venv);
    assert!(!work.results().join("task").exists(), "no run was started");
}

#[test]
fn a_task_directory_that_is_not_there_is_a_usage_error() {
    let _env = environment(false);
    let work = Workspace::new();
    let starts = Starts::default();
    let host = host(false, starts.refusing());
    let missing = work.dir.path().join("no-such-task");
    let ran = run_command(
        &host,
        &[
            "run",
            path(&missing),
            "--results-root",
            path(&work.results()),
            "--max-usd",
            "1",
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(starts.count(), 0);
}

#[test]
fn config_print_shows_the_merged_configuration_and_needs_no_credential() {
    let _env = environment(false);
    let work = Workspace::new();
    let file = work.dir.path().join("mine.toml");
    std::fs::write(
        &file,
        "[investigation.execution]\nmax_idea_cycles = 2\n[budget]\nmax_usd = \"7.25\"\n",
    )
    .unwrap();
    let starts = Starts::default();
    let host = host(false, starts.refusing());
    let ran = run_command(
        &host,
        &[
            "config",
            "--print",
            "--config",
            path(&file),
            "--set",
            "ablations.execution.num_replications=1",
        ],
    );
    assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
    assert_eq!(starts.count(), 0);
    let printed: ScientistConfig = ScientistConfig {
        values: toml::from_str(&ran.out).expect("the output is TOML"),
    };
    assert_eq!(printed.u64("investigation.execution.max_idea_cycles"), 2);
    assert_eq!(printed.u64("ablations.execution.num_replications"), 1);
    assert_eq!(printed.micros("budget.max_usd").unwrap(), 7_250_000);
    // Untouched keys keep their defaults.
    assert_eq!(printed.u64("ablations.execution.max_evals_per_variant"), 5);
}

#[test]
fn check_claims_writes_the_report_and_needs_no_model_network_or_credential() {
    let _env = environment(false);
    let work = Workspace::new();
    let paper = work.dir.path().join("paper");
    std::fs::create_dir_all(&paper).unwrap();
    std::fs::write(paper.join("paper.tex"), "x").unwrap();
    let starts = Starts::default();
    let mut host = host(false, starts.refusing());
    host.claims = Some(Arc::new(|dir: &Path| -> Result<serde_json::Value, String> {
        let tex = std::fs::read_to_string(dir.join("paper.tex")).map_err(|e| e.to_string())?;
        Ok(json!({ "claims_listed": 3, "paper_bytes": tex.len() }))
    }) as Arc<dyn ClaimsChecker>);

    let out = work.dir.path().join("reports/claims.json");
    let ran = run_command(
        &host,
        &[
            "check-claims",
            "--paper-dir",
            path(&paper),
            "--out",
            path(&out),
        ],
    );
    assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report, json!({ "claims_listed": 3, "paper_bytes": 1 }));
    assert!(ran.out.is_empty(), "nothing on standard output");

    // `-` writes the report to standard output instead.
    let ran = run_command(
        &host,
        &["check-claims", "--paper-dir", path(&paper), "--out", "-"],
    );
    assert_eq!(ran.exit, Exit::Done);
    let report: serde_json::Value = serde_json::from_str(&ran.out).unwrap();
    assert_eq!(report["claims_listed"], 3);
    assert!(ran.out.ends_with('\n'));

    // A directory the check cannot read is a failure, not a crash.
    std::fs::remove_file(paper.join("paper.tex")).unwrap();
    let ran = run_command(
        &host,
        &["check-claims", "--paper-dir", path(&paper), "--out", "-"],
    );
    assert_eq!(ran.exit, Exit::Failed);
    assert!(ran.out.is_empty());

    // A directory that is not there is a usage error.
    let ran = run_command(
        &host,
        &[
            "check-claims",
            "--paper-dir",
            path(&work.dir.path().join("none")),
            "--out",
            "-",
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(starts.count(), 0, "no model runner was built");
}

#[test]
fn check_claims_without_a_linked_checker_says_so() {
    let _env = environment(false);
    let work = Workspace::new();
    let host = host(false, Starts::default().refusing());
    let ran = run_command(
        &host,
        &[
            "check-claims",
            "--paper-dir",
            path(&work.task()),
            "--out",
            "-",
        ],
    );
    assert_eq!(ran.exit, Exit::Failed);
    assert!(ran.err.contains("no claims checker"), "{}", ran.err);
}

#[test]
fn reset_refuses_an_unknown_phase_and_a_missing_run() {
    let _env = environment(false);
    let work = Workspace::new();
    let host = host(false, Starts::default().refusing());
    let ran = run_command(
        &host,
        &["reset", "--root", path(&work.task()), "--from", "dessert"],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert!(
        ran.err.contains("ablations"),
        "the phases are listed: {}",
        ran.err
    );
    let ran = run_command(
        &host,
        &[
            "reset",
            "--root",
            path(&work.dir.path().join("none")),
            "--from",
            "paper",
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    // A directory that is not a run: the campaign says so and the command fails.
    let ran = run_command(
        &host,
        &["reset", "--root", path(&work.task()), "--from", "paper"],
    );
    assert_eq!(ran.exit, Exit::Failed);
}

#[test]
fn resume_refuses_a_directory_that_is_not_a_run() {
    let _env = environment(true);
    let work = Workspace::new();
    let host = host(true, Starts::default().refusing());
    let ran = run_command(&host, &["resume", "--root", path(&work.task())]);
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains("not a run directory"), "{}", ran.err);
}

#[test]
fn agent_call_refuses_what_it_cannot_do_before_calling_anything() {
    let _env = environment(false);
    let work = Workspace::new();
    let starts = Starts::default();
    let host = host(false, starts.refusing());
    let prompt = work.task().join("evaluate.py");
    let task = work.task();
    let base = |extra: &[&str]| -> Ran {
        let mut args = vec![
            "agent-call",
            "--results-root",
            path(work.dir.path()),
            "--workspace",
            path(&task),
            "--role-prompt",
            path(&prompt),
            "--task",
            path(&prompt),
        ];
        args.extend_from_slice(extra);
        run_command(&host, &args)
    };
    // No ceiling.
    let ran = base(&[]);
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains("--max-usd"), "{}", ran.err);
    // A prompt file that is not there.
    let ran = run_command(
        &host,
        &[
            "agent-call",
            "--results-root",
            path(work.dir.path()),
            "--workspace",
            path(&work.task()),
            "--role-prompt",
            path(&work.dir.path().join("no-prompt.md")),
            "--task",
            path(&prompt),
            "--max-usd",
            "1",
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert!(ran.err.contains("role prompt"), "{}", ran.err);
    // A workspace that is not there.
    let ran = run_command(
        &host,
        &[
            "agent-call",
            "--results-root",
            path(work.dir.path()),
            "--workspace",
            path(&work.dir.path().join("no-folder")),
            "--role-prompt",
            path(&prompt),
            "--task",
            path(&prompt),
            "--max-usd",
            "1",
        ],
    );
    assert_eq!(ran.exit, Exit::Usage);
    assert_eq!(starts.count(), 0, "nothing reached the model runner");
}

#[test]
fn agent_call_prints_how_the_call_ended_as_json() {
    let _env = environment(false);
    let work = Workspace::new();
    std::fs::write(work.dir.path().join("role.md"), "You are the checker.").unwrap();
    std::fs::write(work.dir.path().join("task.md"), "Say hello.").unwrap();
    let folder = work.dir.path().join("call");
    std::fs::create_dir_all(&folder).unwrap();

    let provider = ScriptedProvider::default();
    provider.script(
        "You are the checker.",
        vec![Reply::Text {
            text: "hello",
            usage: billed(1000, 100),
        }],
    );
    let starts = Starts::default();
    let host = host(false, starts.scripted(&provider));
    let ran = run_command(
        &host,
        &[
            "agent-call",
            "--results-root",
            path(work.dir.path()),
            "--workspace",
            path(&folder),
            "--role-prompt",
            path(&work.dir.path().join("role.md")),
            "--task",
            path(&work.dir.path().join("task.md")),
            "--max-usd",
            "0.5",
            "--max-turns",
            "5",
        ],
    );
    assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
    let report: serde_json::Value = serde_json::from_str(&ran.out).expect("JSON on stdout");
    assert_eq!(report["stop"], "finished");
    assert_eq!(report["final_text"], "hello");
    assert_eq!(report["usage"]["requests"], 1);
    // 1000 input tokens at $2 and 100 output tokens at $10 per million.
    assert_eq!(report["usage"]["micros"], 3000);
    assert_eq!(starts.count(), 1);
    assert_eq!(
        starts.last().venv,
        work.dir.path().canonicalize().unwrap().join(".venv")
    );

    // A call the ceiling cannot pay for still prints a report (any way a call
    // stops is exit 0); a call that cannot be measured is a failure.
    provider.script("You are the checker.", vec![text("again")]);
    let ran = run_command(
        &host,
        &[
            "agent-call",
            "--results-root",
            path(work.dir.path()),
            "--workspace",
            path(&folder),
            "--role-prompt",
            path(&work.dir.path().join("role.md")),
            "--task",
            path(&work.dir.path().join("task.md")),
            "--max-usd",
            "0.000001",
        ],
    );
    assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
    let report: serde_json::Value = serde_json::from_str(&ran.out).unwrap();
    assert_eq!(report["stop"], "cost_cap");

    let outside = tempfile::tempdir().unwrap();
    let ran = run_command(
        &host,
        &[
            "agent-call",
            "--results-root",
            path(work.dir.path()),
            "--workspace",
            path(outside.path()),
            "--role-prompt",
            path(&work.dir.path().join("role.md")),
            "--task",
            path(&work.dir.path().join("task.md")),
            "--max-usd",
            "0.5",
        ],
    );
    assert_eq!(ran.exit, Exit::Failed, "a folder outside the results root");
    assert!(ran.out.is_empty());
}

#[test]
fn a_task_folder_without_a_scaffold_fails_the_task_phase_and_names_what_is_missing() {
    let _env = environment(true);
    let work = Workspace::new();
    let starts = Starts::default();
    let host = host(true, starts.refusing());
    // The workspace task folder holds an evaluator and nothing else.
    let mut args = vec![
        "run".to_string(),
        path(&work.task()).to_string(),
        "--results-root".to_string(),
        path(&work.results()).to_string(),
        "--max-usd".to_string(),
        "1".to_string(),
        "--run-id".to_string(),
        "bare".to_string(),
    ];
    for set in work.scripted_sets() {
        args.push("--set".to_string());
        args.push(set);
    }
    args.push("--set".to_string());
    args.push("campaign.phases=[\"task_drafting\"]".to_string());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = run_command(&host, &args);
    assert_eq!(ran.exit, Exit::Failed, "{}", ran.err);
    let root = work
        .results()
        .canonicalize()
        .unwrap()
        .join("task/polaris/bare");
    let state = std::fs::read_to_string(root.join("pipeline_state.json")).unwrap();
    assert!(state.contains("missing task scaffold"), "{state}");
    assert!(state.contains("program_0"), "{state}");
    assert_eq!(starts.count(), 0, "the session runner was never built");
}

#[test]
fn a_scripted_run_keeps_its_ceiling_exactly_and_a_resume_may_not_change_the_shape() {
    let _env = environment(true);
    let work = Workspace::new();
    let starts = Starts::default();
    let host = host(true, starts.refusing());
    // A complete task folder, and a campaign of the task phase alone: it needs no model call,
    // so the run ends at once with no winner and nothing spent.
    let task = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../polaris_scientist/tests/fixtures/circpack/task");
    let mut args = vec![
        "run".to_string(),
        path(&task).to_string(),
        "--results-root".to_string(),
        path(&work.results()).to_string(),
        "--max-usd".to_string(),
        "13.5".to_string(),
        "--run-id".to_string(),
        "first".to_string(),
    ];
    for set in work.scripted_sets() {
        args.push("--set".to_string());
        args.push(set);
    }
    args.push("--set".to_string());
    args.push("campaign.phases=[\"task_drafting\"]".to_string());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = run_command(&host, &args);
    assert_eq!(ran.exit, Exit::Done, "{}", ran.err);
    assert!(
        ran.err
            .contains("outcome: completed_no_winner; settled $0.000000"),
        "{}",
        ran.err
    );

    let root = work
        .results()
        .canonicalize()
        .unwrap()
        .join("task/polaris/first");
    let saved = ScientistConfig::resolve(
        &[root.join("config.resolved.toml")],
        &[],
        ConfigFlags::default(),
    )
    .unwrap();
    assert_eq!(saved.micros("budget.max_usd").unwrap(), 13_500_000);
    assert_eq!(starts.count(), 0, "the session runner was never built");

    // The run directory is named by the run id, and the id is checked.
    let bad_id = run_command(
        &host,
        &[
            "run",
            path(&work.task()),
            "--results-root",
            path(&work.results()),
            "--run-id",
            "../escape",
        ],
    );
    assert_eq!(bad_id.exit, Exit::Usage);

    // A change to the shape of the campaign is refused, and nothing runs.
    let lowered = run_command(
        &host,
        &[
            "resume",
            "--root",
            path(&root),
            "--set",
            "investigation.execution.max_idea_cycles=1",
        ],
    );
    assert_eq!(lowered.exit, Exit::Usage);
    assert!(lowered.err.contains("max_idea_cycles"), "{}", lowered.err);
    let reshaped = run_command(
        &host,
        &[
            "resume",
            "--root",
            path(&root),
            "--set",
            "review.mode=\"adapter\"",
        ],
    );
    assert_eq!(reshaped.exit, Exit::Usage);
    assert!(reshaped.err.contains("review.mode"), "{}", reshaped.err);

    // A resume that changes nothing goes ahead, with a larger ceiling.
    let again = run_command(&host, &["resume", "--root", path(&root), "--max-usd", "20"]);
    assert_eq!(again.exit, Exit::Done, "{}", again.err);
    assert_eq!(starts.count(), 0);
}
