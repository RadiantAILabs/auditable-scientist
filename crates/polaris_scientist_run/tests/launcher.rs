//! The two launcher scripts, run against a stand-in `docker` that is first on
//! `PATH`. No container is started and no image is used.
//!
//! The stand-in writes the arguments of `docker run` and of `docker stop` to
//! files in its own folder, and can be told to hold the container "running"
//! until `docker stop` is called.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const STAND_IN: &str = r#"#!/usr/bin/env bash
case "$1" in
  run)
    printf '%s\n' "$@" > "$STUB_DIR/run.args"
    : > "$STUB_DIR/run.started"
    if [ -n "${STUB_PRINT:-}" ]; then
      printf '%s' "$STUB_PRINT"
    fi
    if [ -n "${STUB_HOLD:-}" ]; then
      while [ ! -e "$STUB_DIR/stopped" ]; do sleep 0.05; done
    fi
    exit "${STUB_STATUS:-0}"
    ;;
  stop)
    printf '%s\n' "$@" > "$STUB_DIR/stop.args"
    : > "$STUB_DIR/stopped"
    ;;
  *)
    echo "unexpected docker command: $*" >&2
    exit 99
    ;;
esac
"#;

const KEY: &str = "example-key-id-value-0001";
const SECRET: &str = "example-secret-value-0002";
const TOKEN: &str = "example-session-token-value-0003";

fn launcher_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("launcher")
        .join(name)
}

/// The folder of the stand-in `docker`, and of everything the launcher is
/// given to work with.
struct Stub {
    dir: tempfile::TempDir,
}

impl Stub {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let docker = bin.join("docker");
        std::fs::write(&docker, STAND_IN).unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(dir.path().join("task")).unwrap();
        std::fs::create_dir_all(dir.path().join("paper")).unwrap();
        Self { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn task(&self) -> PathBuf {
        self.root().join("task")
    }

    fn paper(&self) -> PathBuf {
        self.root().join("paper")
    }

    fn out(&self) -> PathBuf {
        self.root().join("out")
    }

    /// A launcher command with nothing but the stand-in `docker` and the
    /// system tools around it.
    fn command(&self, script: &str) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(launcher_path(script))
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.path().join("bin").display()),
            )
            .env("STUB_DIR", self.dir.path());
        command
    }

    /// The campaign launcher with a task folder, an output folder and the
    /// credentials the launcher requires.
    fn campaign(&self, label: &str) -> Command {
        let mut command = self.command("run_polaris.sh");
        command
            .arg(label)
            .env("POLARIS_SCI_TASK_DIR", self.task())
            .env("POLARIS_SCI_OUT", self.out())
            .env("AWS_ACCESS_KEY_ID", KEY)
            .env("AWS_SECRET_ACCESS_KEY", SECRET)
            .env("AWS_SESSION_TOKEN", TOKEN);
        command
    }

    fn lines(&self, file: &str) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.path().join(file)).ok()?;
        Some(text.lines().map(str::to_string).collect())
    }

    fn run_args(&self) -> Vec<String> {
        self.lines("run.args").expect("docker run was called")
    }

    fn docker_was_called(&self) -> bool {
        self.dir.path().join("run.args").exists() || self.dir.path().join("stop.args").exists()
    }
}

fn ran(command: &mut Command) -> Output {
    command.stdin(Stdio::null()).output().expect("bash starts")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("the script exited by itself")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ids() -> String {
    // SAFETY: `getuid` and `getgid` take no arguments and cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    format!("{uid}:{gid}")
}

/// Wait for `condition` for up to ten seconds.
fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("gave up waiting for {what}");
}

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    panic!("the launcher did not end");
}

#[test]
fn both_scripts_can_be_run_directly() {
    for name in ["run_polaris.sh", "run_claims_check.sh"] {
        let mode = std::fs::metadata(launcher_path(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "{name} is executable");
    }
    let image = Path::new(env!("CARGO_MANIFEST_DIR")).join("image/build.sh");
    assert_eq!(
        std::fs::metadata(image).unwrap().permissions().mode() & 0o111,
        0o111
    );
}

#[test]
fn the_container_gets_the_limits_the_mounts_and_the_command() {
    let stub = Stub::new();
    let results = stub.out().join("results/lab-1");
    let output = ran(stub.campaign("lab-1").args([
        "--max-usd",
        "13.5",
        "--config",
        "/opt/polaris-scientist/configs/circpack_reduced.toml",
        "--set",
        "a.b=1",
    ]));
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let results = results.to_str().unwrap();
    let task = stub.task();
    let expected: Vec<String> = [
        "run",
        "--rm",
        "--sig-proxy=false",
        "--name",
        "polaris-sci-lab-1",
        "--user",
        &ids(),
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "1024",
        "--cpus",
        "4",
        "--memory",
        "12g",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,size=2g",
        "-e",
        "HOME=/tmp/home",
        "-e",
        "AWS_ACCESS_KEY_ID",
        "-e",
        "AWS_SECRET_ACCESS_KEY",
        "-e",
        "AWS_SESSION_TOKEN",
        "-e",
        "AWS_REGION",
        "-e",
        "AWS_DEFAULT_REGION",
        "--mount",
        &format!("type=bind,src={},dst=/task-src,readonly", task.display()),
        "--mount",
        &format!("type=bind,src={results},dst={results}"),
        "polaris-scientist:latest",
        "polaris-scientist",
        "run",
        "/task-src",
        "--results-root",
        results,
        "--max-usd",
        "13.5",
        "--config",
        "/opt/polaris-scientist/configs/circpack_reduced.toml",
        "--set",
        "a.b=1",
    ]
    .iter()
    .map(|part| (*part).to_string())
    .collect();
    assert_eq!(stub.run_args(), expected);
    assert!(
        Path::new(results).is_dir(),
        "the results root exists on the host before the mount"
    );
}

#[test]
fn credentials_are_handed_over_by_name_and_their_values_are_never_shown() {
    let stub = Stub::new();
    // Even a shell trace must not show them.
    let mut command = Command::new("bash");
    command
        .arg("-x")
        .arg(launcher_path("run_polaris.sh"))
        .arg("secrets")
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", stub.dir.path().join("bin").display()),
        )
        .env("STUB_DIR", stub.dir.path())
        .env("POLARIS_SCI_TASK_DIR", stub.task())
        .env("POLARIS_SCI_OUT", stub.out())
        .env("AWS_ACCESS_KEY_ID", KEY)
        .env("AWS_SECRET_ACCESS_KEY", SECRET)
        .env("AWS_SESSION_TOKEN", TOKEN)
        .env("AWS_REGION", "eu-west-2");
    let output = ran(&mut command);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let args = stub.run_args();
    for value in [KEY, SECRET, TOKEN] {
        assert!(
            args.iter().all(|arg| !arg.contains(value)),
            "a credential value is on the command line"
        );
        assert!(
            !stderr(&output).contains(value),
            "a credential value is in the trace"
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(value));
    }
    for name in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
    ] {
        let at = args
            .iter()
            .position(|arg| arg == name)
            .unwrap_or_else(|| panic!("{name} is passed by name"));
        assert_eq!(args[at - 1], "-e");
    }
    assert!(
        args.iter()
            .filter(|arg| arg.starts_with("AWS_"))
            .all(|arg| !arg.contains('=')),
        "no AWS variable is given a value"
    );
    // The only variable that is given a value is the home folder.
    let given: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(at, _)| *at > 0 && args[*at - 1] == "-e")
        .map(|(_, arg)| arg)
        .filter(|arg| arg.contains('='))
        .collect();
    assert_eq!(given, ["HOME=/tmp/home"]);
}

#[test]
fn a_missing_credential_is_exit_two_and_no_container_is_started() {
    for missing in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
    ] {
        for empty in [false, true] {
            let stub = Stub::new();
            let mut command = stub.campaign("lab");
            if empty {
                command.env(missing, "");
            } else {
                command.env_remove(missing);
            }
            let output = ran(&mut command);
            assert_eq!(code(&output), 2, "{missing}: {}", stderr(&output));
            assert!(stderr(&output).contains(missing), "{}", stderr(&output));
            assert!(!stub.docker_was_called(), "{missing}: docker was called");
            assert!(!stub.out().exists(), "{missing}: a folder was made");
        }
    }
}

#[test]
fn a_bad_label_or_task_folder_is_exit_two_and_no_container_is_started() {
    for label in ["", "-lab", ".lab", "a/b", "a b", "a;b", "lab\n"] {
        let stub = Stub::new();
        let output = ran(&mut stub.campaign(label));
        assert_eq!(code(&output), 2, "{label:?}: {}", stderr(&output));
        assert!(!stub.docker_was_called(), "{label:?}");
    }

    let stub = Stub::new();
    let output = ran(&mut stub.command("run_polaris.sh"));
    assert_eq!(code(&output), 2, "no label");

    let stub = Stub::new();
    let output = ran(stub
        .campaign("lab")
        .env("POLARIS_SCI_TASK_DIR", stub.root().join("none")));
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(!stub.docker_was_called());

    // A comma would split the mount option.
    let stub = Stub::new();
    let odd = stub.root().join("a,b");
    std::fs::create_dir_all(&odd).unwrap();
    let output = ran(stub.campaign("lab").env("POLARIS_SCI_TASK_DIR", &odd));
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(!stub.docker_was_called());
}

#[test]
fn the_task_folder_and_the_output_root_must_be_named() {
    for missing in ["POLARIS_SCI_TASK_DIR", "POLARIS_SCI_OUT"] {
        for empty in [false, true] {
            let stub = Stub::new();
            let mut command = stub.campaign("lab");
            if empty {
                command.env(missing, "");
            } else {
                command.env_remove(missing);
            }
            let output = ran(&mut command);
            assert_eq!(code(&output), 2, "{missing}: {}", stderr(&output));
            assert!(stderr(&output).contains(missing), "{}", stderr(&output));
            assert!(!stub.docker_was_called(), "{missing}: docker was called");
            assert!(!stub.out().exists(), "{missing}: a folder was made");
        }
    }
}

#[test]
fn the_limits_and_the_image_can_be_changed_from_the_environment() {
    let stub = Stub::new();
    let output = ran(stub
        .campaign("lab")
        .env("POLARIS_SCI_CPUS", "2")
        .env("POLARIS_SCI_MEMORY", "6g")
        .env("POLARIS_SCI_IMAGE", "polaris-scientist:test"));
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let args = stub.run_args();
    let value_after = |flag: &str| -> String {
        let at = args.iter().position(|arg| arg == flag).expect(flag);
        args[at + 1].clone()
    };
    assert_eq!(value_after("--cpus"), "2");
    assert_eq!(value_after("--memory"), "6g");
    assert!(args.iter().any(|arg| arg == "polaris-scientist:test"));
    assert!(!args.iter().any(|arg| arg == "polaris-scientist:latest"));
}

#[test]
fn the_exit_status_of_the_container_is_the_exit_status_of_the_launcher() {
    for status in ["0", "1", "2", "4", "137"] {
        let stub = Stub::new();
        let output = ran(stub.campaign("lab").env("STUB_STATUS", status));
        assert_eq!(code(&output).to_string(), status, "{}", stderr(&output));
    }
}

#[test]
fn a_stop_signal_stops_the_container_with_sixty_seconds_to_finish() {
    for (signal, name) in [(libc::SIGTERM, "TERM"), (libc::SIGINT, "INT")] {
        let stub = Stub::new();
        let mut child = stub
            .campaign("lab-stop")
            .env("STUB_HOLD", "1")
            .env("STUB_STATUS", "4")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("bash starts");
        wait_for("the container to start", || {
            stub.dir.path().join("run.started").exists()
        });
        assert!(
            !stub.dir.path().join("stop.args").exists(),
            "not stopped yet"
        );

        let pid = libc::pid_t::try_from(child.id()).unwrap();
        // SAFETY: `kill` takes plain integers; `pid` is the launcher this test
        // started and still holds.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);

        let status = wait_for_exit(&mut child);
        assert_eq!(status.code(), Some(4), "SIG{name}: the container's status");
        assert_eq!(
            stub.lines("stop.args").expect("docker stop was called"),
            ["stop", "--time", "60", "polaris-sci-lab-stop"],
            "SIG{name}"
        );
    }
}

#[test]
fn the_claims_check_runs_with_no_network_no_credential_and_read_only_mounts() {
    let stub = Stub::new();
    let report = stub.root().join("reports/claims.json");
    let output = ran(stub
        .command("run_claims_check.sh")
        .arg(stub.paper())
        .arg(&report)
        // Credentials in the caller's environment must not reach it.
        .env("AWS_ACCESS_KEY_ID", KEY)
        .env("AWS_SECRET_ACCESS_KEY", SECRET)
        .env("AWS_SESSION_TOKEN", TOKEN)
        .env("STUB_PRINT", "{\"claims\": []}\n"));
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let expected: Vec<String> = [
        "run",
        "--rm",
        "--init",
        "--network",
        "none",
        "--read-only",
        "--user",
        &ids(),
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "512",
        "--cpus",
        "2",
        "--memory",
        "4g",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,size=512m",
        "-e",
        "HOME=/tmp",
        "--mount",
        &format!(
            "type=bind,src={},dst=/paper,readonly",
            stub.paper().display()
        ),
        "polaris-scientist:latest",
        "polaris-scientist",
        "check-claims",
        "--paper-dir",
        "/paper",
        "--out",
        "-",
    ]
    .iter()
    .map(|part| (*part).to_string())
    .collect();
    let args = stub.run_args();
    assert_eq!(args, expected);
    assert!(
        args.iter().all(|arg| !arg.contains("AWS_")),
        "no credential variable"
    );
    assert_eq!(
        std::fs::read_to_string(&report).unwrap(),
        "{\"claims\": []}\n",
        "the program's standard output is the report"
    );
    let leftovers: Vec<_> = std::fs::read_dir(report.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "no partial file is left: {leftovers:?}");
}

#[test]
fn a_failed_claims_check_leaves_no_report_behind() {
    let stub = Stub::new();
    let report = stub.root().join("reports/claims.json");
    let output = ran(stub
        .command("run_claims_check.sh")
        .arg(stub.paper())
        .arg(&report)
        .env("STUB_PRINT", "half a report")
        .env("STUB_STATUS", "1"));
    assert_eq!(code(&output), 1, "{}", stderr(&output));
    assert!(!report.exists());
    let leftovers = std::fs::read_dir(report.parent().unwrap()).unwrap().count();
    assert_eq!(leftovers, 0, "the partial file is removed");
}

#[test]
fn the_claims_check_refuses_what_it_cannot_mount() {
    let stub = Stub::new();
    let output = ran(stub.command("run_claims_check.sh").arg(stub.paper()));
    assert_eq!(code(&output), 2, "one argument");
    let output = ran(stub
        .command("run_claims_check.sh")
        .arg(stub.root().join("none"))
        .arg(stub.root().join("report.json")));
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(!stub.docker_was_called());
}
