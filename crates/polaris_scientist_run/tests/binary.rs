//! The `polaris-scientist` program itself, run as a child process with an empty
//! environment.
//!
//! Nothing here starts a model session: every case ends before one would be
//! built, or runs offline on a scripted model.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const OFFLINE_VAR: &str = "POLARIS_SCIENTIST_OFFLINE";

/// The program, with nothing in its environment.
fn program() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_polaris-scientist"));
    command.env_clear();
    command
}

fn run(command: &mut Command) -> Output {
    command.output().expect("the program starts")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("the program exited by itself")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A task directory and a scenario directory that lets a campaign end at once.
struct Setup {
    dir: tempfile::TempDir,
}

impl Setup {
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

    fn scenario_setting(&self) -> String {
        format!(
            "runner.scenario=\"{}\"",
            self.dir.path().join("scenario").display()
        )
    }
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn version_prints_the_build_the_agent_library_and_the_target() {
    let output = run(program().arg("--version"));
    assert_eq!(code(&output), 0);
    let shown = stdout(&output);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), 3, "{shown}");
    assert!(
        lines[0].starts_with(&format!(
            "polaris-scientist {} (",
            env!("CARGO_PKG_VERSION")
        )),
        "{shown}"
    );
    assert!(lines[1].starts_with("polaris_code "), "{shown}");
    assert!(lines[2].starts_with("target "), "{shown}");
    assert!(!lines[1].contains("unknown"), "{shown}");
}

#[test]
fn a_command_line_that_makes_no_sense_exits_with_two() {
    for args in [
        vec!["nothing"],
        vec![],
        vec!["config"],
        vec!["run"],
        vec!["reset", "--root", "x"],
    ] {
        let output = run(program().args(&args));
        assert_eq!(code(&output), 2, "{args:?}: {}", stderr(&output));
        assert!(stdout(&output).is_empty(), "{args:?}");
    }
}

#[test]
fn a_run_without_a_spend_ceiling_exits_with_two_before_anything_starts() {
    let setup = Setup::new();
    let output = run(program().args([
        "run",
        path(&setup.task()),
        "--results-root",
        path(&setup.results()),
    ]));
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(stderr(&output).contains("--max-usd"), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert!(!setup.results().exists(), "nothing was created");
}

#[test]
fn config_print_needs_no_credential_and_shows_the_merged_values() {
    let reduced = Path::new(env!("CARGO_MANIFEST_DIR")).join("configs/circpack_reduced.toml");
    let output = run(program().args([
        "config",
        "--print",
        "--config",
        path(&reduced),
        "--set",
        "ablations.execution.max_evals_per_variant=3",
    ]));
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let printed: toml::Table = toml::from_str(&stdout(&output)).expect("TOML on stdout");
    let value = |section: &str, key: &str| -> i64 {
        printed[section]["execution"][key]
            .as_integer()
            .unwrap_or_else(|| panic!("{section}.execution.{key} is a number"))
    };
    assert_eq!(value("investigation", "max_idea_cycles"), 2);
    assert_eq!(value("ablations", "num_replications"), 1);
    assert_eq!(value("ablations", "max_evals_per_variant"), 3, "--set wins");
    assert_eq!(value("posthoc", "max_selected_analyses"), 1);

    let refused = run(program().args(["config", "--print", "--set", "no.such.key=1"]));
    assert_eq!(code(&refused), 2);
    assert!(stdout(&refused).is_empty());
}

#[test]
fn the_model_and_the_region_are_set_as_quoted_text() {
    let set = |value: &str| run(program().args(["config", "--print", "--set", value]));
    let output = set("runner.region=\"eu-west-1\"");
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let printed: toml::Table = toml::from_str(&stdout(&output)).expect("TOML on stdout");
    assert_eq!(printed["runner"]["region"].as_str(), Some("eu-west-1"));

    let output = set("runner.model=\"aws.bedrock/global.anthropic.claude-sonnet-5-5\"");
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let printed: toml::Table = toml::from_str(&stdout(&output)).expect("TOML on stdout");
    assert_eq!(
        printed["runner"]["model"].as_str(),
        Some("aws.bedrock/global.anthropic.claude-sonnet-5-5")
    );

    // Text without quotes is not a TOML string and is refused.
    let output = set("runner.region=eu-west-1");
    assert_eq!(code(&output), 2);
    assert!(stdout(&output).is_empty());
}

/// Every setting a configuration file gives, as `a.b.c` paths.
fn leaves(table: &toml::Table, prefix: &str, into: &mut BTreeMap<String, String>) {
    for (key, value) in table {
        let name = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(inner) => leaves(inner, &name, into),
            other => {
                into.insert(name, other.to_string());
            }
        }
    }
}

#[test]
fn the_reduced_circle_packing_budget_sets_exactly_four_values() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("configs/circpack_reduced.toml");
    let table: toml::Table = std::fs::read_to_string(file).unwrap().parse().unwrap();
    let mut settings = BTreeMap::new();
    leaves(&table, "", &mut settings);
    let expected: BTreeMap<String, String> = [
        ("investigation.execution.max_idea_cycles", "2"),
        ("ablations.execution.num_replications", "1"),
        ("ablations.execution.max_evals_per_variant", "2"),
        ("posthoc.execution.max_selected_analyses", "1"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    assert_eq!(settings, expected);
}

#[test]
fn the_home_directory_is_made_when_missing_and_left_alone_when_there() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("tmp/home");
    let output = run(program().env("HOME", &home).args(["config", "--print"]));
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let mode = std::fs::metadata(&home)
        .expect("HOME was made")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "only its owner has access");

    // An existing home keeps its own mode.
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o750)).unwrap();
    let output = run(program().env("HOME", &home).args(["config", "--print"]));
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o750
    );
}

#[test]
fn offline_a_scripted_campaign_runs_to_the_end_without_a_model() {
    // The research phases up to post-hoc, on the stand-in circle packing task
    // and the scripted answers that the research crate's tests use. The paper
    // phase is left out: it needs reference services and TeX.
    let setup = Setup::new();
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("../polaris_scientist/tests");
    let task = tests.join("fixtures/circpack/task");
    let scenario = format!(
        "runner.scenario=\"{}\"",
        tests.join("scenarios/campaign").display()
    );
    let output = run(program()
        .env(OFFLINE_VAR, "1")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args([
            "run",
            path(&task),
            "--results-root",
            path(&setup.results()),
            "--run-id",
            "offline",
            "--max-usd",
            "20",
            "--set",
            "runner.kind=\"scripted\"",
            "--set",
            &scenario,
            "--set",
            "campaign.phases=[\"task_drafting\",\"investigation\",\"ablations\",\"final_evaluation\",\"posthoc\"]",
        ]));
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("outcome: completed; settled $"),
        "{}",
        stderr(&output)
    );
    let outcome: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(setup.results().join("task/polaris/offline/outcome.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["outcome"], "completed", "{outcome}");
    assert_eq!(outcome["phases"]["investigation"], "completed", "{outcome}");
    assert_eq!(
        outcome["phases"]["final_evaluation"], "degraded",
        "no task contract, so the sealed final phase is not run: {outcome}"
    );
}

#[test]
fn offline_nothing_may_call_a_model() {
    let setup = Setup::new();
    // The model runner is refused, before any credential is looked for.
    let output = run(program().env(OFFLINE_VAR, "1").args([
        "run",
        path(&setup.task()),
        "--results-root",
        path(&setup.results()),
    ]));
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(stderr(&output).contains("scripted"), "{}", stderr(&output));

    let output = run(program().env(OFFLINE_VAR, "1").args([
        "agent-call",
        "--results-root",
        path(setup.dir.path()),
        "--workspace",
        path(&setup.task()),
        "--role-prompt",
        path(&setup.task().join("evaluate.py")),
        "--task",
        path(&setup.task().join("evaluate.py")),
        "--max-usd",
        "1",
    ]));
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(stderr(&output).contains(OFFLINE_VAR), "{}", stderr(&output));
}

#[test]
fn a_scripted_runner_needs_the_offline_variable() {
    let setup = Setup::new();
    let output = run(program().args([
        "run",
        path(&setup.task()),
        "--results-root",
        path(&setup.results()),
        "--max-usd",
        "5",
        "--set",
        "runner.kind=\"scripted\"",
        "--set",
        &setup.scenario_setting(),
    ]));
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(stderr(&output).contains(OFFLINE_VAR), "{}", stderr(&output));
}

/// A paper folder with two claims, their sources and a matching text.
fn write_paper(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("claims_grounding.json"),
        r#"{"claims": [
            {"claim": "The seed scores 0.9598", "source_ids": ["seed.score"], "citation_keys": []},
            {"claim": "A claim with no recorded source", "source_ids": ["none.recorded"], "citation_keys": []}
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("paper_data.json"),
        r#"{"grounding_sources": {"seed.score": {"value": 0.959764}}}"#,
    )
    .unwrap();
    std::fs::write(dir.join("paper.tex"), "The seed scores 0.9598.\n").unwrap();
}

#[test]
fn check_claims_runs_the_real_checker_and_gives_the_same_bytes_twice() {
    let work = tempfile::tempdir().unwrap();
    let paper = work.path().join("paper");
    write_paper(&paper);
    let first = work.path().join("first.json");
    let second = work.path().join("second.json");
    for out in [&first, &second] {
        let output = run(program().args([
            "check-claims",
            "--paper-dir",
            path(&paper),
            "--out",
            path(out),
        ]));
        assert_eq!(code(&output), 0, "{}", stderr(&output));
    }
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&first).unwrap()).unwrap();
    let mechanical = &report["mechanical_check"];
    assert_eq!(mechanical["claims_listed"], 2, "{report}");
    assert_eq!(mechanical["claims_resolved"], 1, "{report}");
    assert_eq!(mechanical["numbers_printed"], 1, "{report}");
    assert_eq!(mechanical["numbers_backed"], 1, "{report}");
    assert_eq!(mechanical["numbers_unbacked"], 0, "{report}");
    assert_eq!(
        mechanical["passed"], false,
        "one declared claim does not resolve: {report}"
    );
    let unresolved = mechanical["unresolved_claims"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1, "{report}");
    assert_eq!(unresolved[0]["id"], "claims[1]", "{report}");
    assert_eq!(
        unresolved[0]["claim"], "A claim with no recorded source",
        "{report}"
    );
    assert_eq!(
        unresolved[0]["unknown_source_ids"],
        serde_json::json!(["none.recorded"]),
        "{report}"
    );
    assert_eq!(report["saved_model_audit"]["state"], "missing", "{report}");
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "the same folder gives the same report"
    );
}

#[test]
fn check_claims_passes_when_every_claim_resolves() {
    let work = tempfile::tempdir().unwrap();
    let paper = work.path().join("paper");
    write_paper(&paper);
    std::fs::write(
        paper.join("claims_grounding.json"),
        r#"{"claims": [
            {"claim": "The seed scores 0.9598", "source_ids": ["seed.score"], "citation_keys": []}
        ]}"#,
    )
    .unwrap();
    let output = run(program().args(["check-claims", "--paper-dir", path(&paper), "--out", "-"]));
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let mechanical = &report["mechanical_check"];
    assert_eq!(mechanical["passed"], true, "{report}");
    assert_eq!(mechanical["claims_listed"], 1, "{report}");
    assert_eq!(mechanical["claims_resolved"], 1, "{report}");
    assert_eq!(mechanical["unresolved_claims"], serde_json::json!([]));
}

#[test]
fn check_claims_names_the_file_a_paper_folder_lacks() {
    let work = tempfile::tempdir().unwrap();
    let paper = work.path().join("paper");
    write_paper(&paper);
    std::fs::remove_file(paper.join("paper_data.json")).unwrap();
    let output = run(program().args(["check-claims", "--paper-dir", path(&paper), "--out", "-"]));
    assert_eq!(code(&output), 1, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("paper_data.json"),
        "{}",
        stderr(&output)
    );
    assert!(stdout(&output).is_empty(), "no partial report");
}
