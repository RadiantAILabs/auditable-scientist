//! Advisory author tests use the existing session and never replace scoring.
mod support;

use polaris_cosmos::flow::{StopCause, TaskRunner as _};
use polaris_scientist::{
    config::{ConfigFlags, ScientistConfig},
    eval,
    phases::task,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};
use support::{Fixture, fixture_with, request, text, tool};

fn config() -> ScientistConfig {
    ScientistConfig::resolve(&[], &[], ConfigFlags::default()).unwrap()
}

async fn author_fixture() -> Fixture {
    let fx = fixture_with(|runner| runner.author_test_config = Some(config())).await;
    let task = fx.run_root.path().join("task");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("evaluate.py"),
        r#"import argparse, json, runpy
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--program_path')
p.add_argument('--results_dir')
p.add_argument('--quiet', action='store_true')
a = p.parse_args()
values = runpy.run_path(a.program_path)
r = Path(a.results_dir)
(r / 'metrics.json').write_text(json.dumps({'combined_score':values['SCORE']}))
(r / 'correct.json').write_text(json.dumps({'correct':True}))
"#,
    )
    .unwrap();
    fs::set_permissions(task.join("evaluate.py"), fs::Permissions::from_mode(0o444)).unwrap();
    fs::write(
        task.join("runtime.json"),
        serde_json::to_vec(&json!({
            "python":"/usr/bin/python3", "interpreter_version":"test", "pins":{}, "installed":{},
            "evaluator_sha256":eval::file_digest(&task.join("evaluate.py")).unwrap(),
        }))
        .unwrap(),
    )
    .unwrap();
    fx
}

fn candidate(folder: &Path, index: u32, source: &str) {
    let program = folder.join(format!("outputs/program_{index}"));
    fs::create_dir_all(program.join("src")).unwrap();
    fs::write(
        program.join("program_spec.toml"),
        "code_root = 'src'\n[entrypoint]\npath = 'program.py'\n",
    )
    .unwrap();
    fs::write(program.join("src/program.py"), source).unwrap();
}

fn test() -> support::Reply {
    tool(
        "test_candidate",
        json!({"program_dir":"outputs/program_1", "timeout_seconds":10}),
    )
}

#[tokio::test]
async fn author_tests_reads_result_and_edits_in_one_session_then_host_scores_final_code() {
    let fx = author_fixture().await;
    let folder = fx.folder("author");
    candidate(&folder, 1, "SCORE = 3\n");
    fx.provider.script(&Fixture::key(&folder), vec![
        test(),
        tool("read_file", json!({"path":"tests/001/results/metrics.json"})),
        tool("edit_file", json!({"path":"outputs/program_1/src/program.py", "old_string":"SCORE = 3", "new_string":"SCORE = 7", "replace_all":false})),
        text("finished after checking the advisory score"),
    ]);
    let mut req = request("program_author", &folder);
    req.grants.edit = true;
    let report = fx.runner.run(req).await.unwrap();
    assert_eq!(report.stop, StopCause::Finished);
    assert_eq!(report.usage.requests, 4);
    assert_eq!(fx.runner.live_sessions(), 0);
    let seen = fx.provider.requests();
    assert!(seen[0].tools.iter().any(|name| name == "test_candidate"));
    assert!(seen[1].messages.contains("combined_score") && seen[1].messages.contains("advisory"));
    assert!(seen[3].messages.contains("Do the program_author task."));
    assert!(!folder.join("outputs/program_1/results").exists());
    assert_eq!(
        fs::read_to_string(folder.join("outputs/program_1/src/program.py")).unwrap(),
        "SCORE = 7\n"
    );
    let request = task::evaluator_request(
        fx.run_root.path(),
        &config(),
        folder.join("outputs/program_1"),
        folder.join("official"),
    )
    .unwrap();
    let official = eval::run_evaluator(request).await.unwrap();
    assert_eq!(official.score(), Some(7.0));
    assert!(!official.reused);
    let advisory: Value =
        serde_json::from_slice(&fs::read(folder.join("tests/001/report.json")).unwrap()).unwrap();
    assert_eq!(advisory["metrics"]["combined_score"], 3);
    fx.runner.shutdown().await;
}

#[tokio::test]
async fn author_can_ignore_the_option_and_other_roles_cannot_dispatch_it() {
    let fx = author_fixture().await;
    for role in [
        "program_author",
        "variant_author",
        "variant_retrofit",
        "ideation",
    ] {
        let folder = fx.folder(role);
        candidate(&folder, 1, "SCORE = 3\n");
        fx.provider.script(
            &Fixture::key(&folder),
            if role == "ideation" {
                vec![test(), text("done")]
            } else {
                vec![text("done")]
            },
        );
        let mut req = request(role, &folder);
        req.grants.edit = true;
        assert_eq!(fx.runner.run(req).await.unwrap().stop, StopCause::Finished);
        assert!(!folder.join("tests").exists());
        let seen = fx.provider.requests_for(&Fixture::key(&folder));
        assert_eq!(
            seen[0].tools.iter().any(|t| t == "test_candidate"),
            role != "ideation"
        );
    }
    fx.runner.shutdown().await;
}

#[tokio::test]
async fn one_test_per_call_and_no_network_or_writes_to_frozen_inputs() {
    let fx = author_fixture().await;
    let folder = fx.folder("bounded");
    let evaluator = fx.run_root.path().join("task/evaluate.py");
    let before = fs::read(&evaluator).unwrap();
    let source = format!(
        r#"from pathlib import Path
import socket, os, multiprocessing
q = multiprocessing.get_context("fork").Queue()
q.put(1)
assert q.get(timeout=1) == 1
p = Path({path:?})
blocked = 0
for action in [lambda: p.write_text('changed'), lambda: p.chmod(0o777), lambda: socket.socket(), lambda: os.open(str(p), os.O_WRONLY | os.O_TRUNC)]:
    try:
        action()
    except PermissionError:
        blocked += 1
SCORE = blocked
"#,
        path = evaluator.to_str().unwrap()
    );
    candidate(&folder, 1, &source);
    let stale = folder.join("outputs/program_1/results");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("metrics.json"), "{\"combined_score\":999}").unwrap();
    fx.provider.script(
        &Fixture::key(&folder),
        vec![test(), test(), test(), text("done")],
    );
    let mut req = request("variant_author", &folder);
    req.grants.edit = true;
    assert_eq!(fx.runner.run(req).await.unwrap().stop, StopCause::Finished);
    let advisory: Value =
        serde_json::from_slice(&fs::read(folder.join("tests/001/report.json")).unwrap()).unwrap();
    assert_eq!(advisory["metrics"]["combined_score"], 4, "{advisory}");
    assert_eq!(advisory["integrity_valid"], true);
    assert!(!folder.join("tests/001/candidate/results").exists());
    assert!(
        stale.join("metrics.json").exists(),
        "testing preserves author source files"
    );
    assert!(
        advisory["analysis_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["id"] == "analysis_files_present" && check["pass"] == false)
    );
    assert!(!folder.join("tests/002").exists());
    assert!(
        fx.provider
            .requests()
            .last()
            .unwrap()
            .messages
            .contains("author test limit reached")
    );
    assert_eq!(fs::read(&evaluator).unwrap(), before);
    assert_eq!(
        fs::metadata(&evaluator).unwrap().permissions().mode() & 0o777,
        0o444
    );
    fx.runner.shutdown().await;
}

#[tokio::test]
async fn timeout_consumes_a_test_and_symlinks_cannot_redirect_candidate_or_scratch() {
    let fx = author_fixture().await;
    let folder = fx.folder("timeout");
    candidate(&folder, 1, "import time\ntime.sleep(20)\nSCORE = 3\n");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![
            tool(
                "test_candidate",
                json!({"program_dir":"outputs/program_1", "timeout_seconds":1}),
            ),
            text("done"),
        ],
    );
    let mut req = request("program_author", &folder);
    req.grants.edit = true;
    assert_eq!(fx.runner.run(req).await.unwrap().stop, StopCause::Finished);
    let report: Value =
        serde_json::from_slice(&fs::read(folder.join("tests/001/report.json")).unwrap()).unwrap();
    assert!(report["error"].as_str().unwrap().contains("timed out"));
    assert_eq!(report["tests_remaining"], 0);
    for name in ["candidate-link", "scratch-link"] {
        let folder = fx.folder(name);
        candidate(&folder, 1, "SCORE = 3\n");
        if name == "candidate-link" {
            symlink("/tmp", folder.join("outputs/program_1/src/escape")).unwrap();
        } else {
            symlink(fx.run_root.path(), folder.join("tests")).unwrap();
        }
        fx.provider
            .script(&Fixture::key(&folder), vec![test(), text("done")]);
        let mut req = request("program_author", &folder);
        req.grants.edit = true;
        assert_eq!(fx.runner.run(req).await.unwrap().stop, StopCause::Finished);
        assert!(!folder.join("tests/001/report.json").exists());
    }
    fx.runner.shutdown().await;
}
