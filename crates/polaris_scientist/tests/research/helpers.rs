use polaris_scientist::{
    eval::{self, EvalRequest},
    phases::{
        investigation::{memory, rubrics, selection},
        task::{files, runtime},
    },
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/circpack/task")
}

fn request(temp: &Path) -> EvalRequest {
    EvalRequest {
        python: "python3".into(),
        evaluator: temp.join("task/evaluate.py"),
        program_dir: temp.join("task/program_0"),
        results_dir: temp.join("results"),
        timeout: Duration::from_secs(300),
        grace: Duration::from_secs(5),
        environment: BTreeMap::new(),
    }
}
fn staged() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    files::copy_tree(&fixture(), &temp.path().join("task")).unwrap();
    temp
}

#[tokio::test]
async fn evaluator_scores_once_and_adopts_only_matching_results() {
    let temp = staged();
    let req = request(temp.path());
    let result = eval::run_evaluator(req.clone()).await.unwrap();
    assert_eq!(result.score(), Some(1.0));
    assert!(!result.reused);
    let reused = eval::run_evaluator(req.clone()).await.unwrap();
    assert!(reused.reused);
    assert_eq!(result.manifest.started, reused.manifest.started);
    fs::write(
        req.program_dir.join("src/program.py"),
        "def solution(): return 2.0\n",
    )
    .unwrap();
    assert!(
        eval::run_evaluator(req)
            .await
            .unwrap_err()
            .to_string()
            .contains("fingerprint")
    );
}

#[tokio::test]
async fn evaluator_detects_candidate_and_evaluator_tampering() {
    for evaluator_change in [false, true] {
        let temp = staged();
        let req = request(temp.path());
        let target = if evaluator_change {
            req.evaluator.clone()
        } else {
            req.program_dir.join("src/program.py")
        };
        let source = format!(
            "from pathlib import Path\ndef solution():\n    p=Path({:?})\n    p.write_text(p.read_text()+'\\n# changed')\n    return 2.0\n",
            target.to_string_lossy()
        );
        fs::write(req.program_dir.join("src/program.py"), source).unwrap();
        assert!(
            eval::run_evaluator(req)
                .await
                .unwrap_err()
                .to_string()
                .contains(if evaluator_change {
                    "evaluator changed"
                } else {
                    "candidate changed"
                })
        );
    }
}

#[tokio::test]
async fn timeout_kills_evaluator_and_its_descendant() {
    let temp = staged();
    let mut req = request(temp.path());
    req.timeout = Duration::from_millis(100);
    req.grace = Duration::from_millis(10);
    let pid_file = temp.path().join("child.pid");
    let script = format!(
        "import subprocess,time\nfrom pathlib import Path\np=subprocess.Popen(['sleep','60'])\nPath({:?}).write_text(str(p.pid))\ntime.sleep(60)\n",
        pid_file.to_string_lossy()
    );
    fs::write(&req.evaluator, script).unwrap();
    assert!(
        eval::run_evaluator(req)
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    let pid = fs::read_to_string(pid_file).unwrap();
    assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
}

#[tokio::test]
async fn evaluator_rejects_partial_results_and_environment_injection() {
    let temp = staged();
    let mut req = request(temp.path());
    req.environment
        .insert("AWS_ACCESS_KEY_ID".into(), "inert-test-marker".into());
    assert!(
        eval::run_evaluator(req.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported evaluator environment")
    );
    req.environment.clear();
    fs::write(req.results_dir.join("metrics.json"), "{}").unwrap();
    assert!(
        eval::run_evaluator(req)
            .await
            .unwrap_err()
            .to_string()
            .contains("already has results")
    );
}

#[test]
fn promotion_requires_correct_finite_numeric_scores_and_preserves_ties() {
    let temp = staged();
    let req = request(temp.path());
    fs::create_dir_all(&req.results_dir).unwrap();
    for (score, correct, usable) in [
        (json!(true), true, false),
        (json!("2.0"), true, false),
        (json!(2.0), false, false),
        (json!(2.0), true, true),
    ] {
        files::write_json(
            &req.results_dir.join("metrics.json"),
            &json!({"combined_score":score}),
        )
        .unwrap();
        files::write_json(
            &req.results_dir.join("correct.json"),
            &json!({"correct":correct}),
        )
        .unwrap();
        let candidate =
            selection::candidate(&req.program_dir, &req.results_dir, "attempt").unwrap();
        assert_eq!(candidate.is_some(), usable);
        if let Some(candidate) = candidate {
            let mut best = None;
            assert!(selection::promote(&mut best, candidate.clone()));
            assert!(!selection::promote(&mut best, candidate.clone()));
            let mut lower = candidate.clone();
            lower.combined_score = 1.0;
            assert!(!selection::promote(&mut best, lower));
            let mut lane = candidate.clone();
            lane.source_attempt_id = "lane0".into();
            assert_eq!(
                selection::join(vec![(1, candidate), (0, lane)])
                    .unwrap()
                    .source_attempt_id,
                "lane0"
            );
        }
    }
}

#[test]
fn invalid_conclusions_preserve_previous_text_and_unchanged_memory_is_not_published() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("shared");
    fs::write(&source, "Try bounded restarts.").unwrap();
    assert!(memory::publish(&source, &destination, true).unwrap());
    assert!(!memory::publish(&source, &destination, true).unwrap());
    for invalid in [
        "word ".repeat(301),
        "First.\n\nSecond.".into(),
        "First.\n \nSecond.".into(),
    ] {
        fs::write(&source, invalid).unwrap();
        assert!(memory::publish(&source, &destination, true).is_err());
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "Try bounded restarts."
        );
    }
}

#[tokio::test]
async fn mechanical_rubrics_ignore_comments_and_docstrings_and_leave_semantics_unassessed() {
    let temp = staged();
    let program = temp.path().join("task/program_0");
    fs::write(program.join("src/program.py"),"\"\"\"forbidden_docstring\"\"\"\n# forbidden_comment\ndef solution():\n    \"\"\"nested_docstring\"\"\"\n    restart_count = 2\n    return restart_count\n").unwrap();
    let rubrics = vec![
        json!({"id":"metric","description":"combined_score > 1.0"}),
        json!({"id":"code","verification":{"type":"source_contains","patterns":["restart_count = 2"]}}),
        json!({"id":"comment","verification":{"type":"source_contains","patterns":["forbidden_comment"]}}),
        json!({"id":"docstring","verification":{"type":"source_contains","patterns":["forbidden_docstring","nested_docstring"]}}),
        json!({"id":"semantic","description":"algorithm is innovative"}),
    ];
    let report = rubrics::mechanical(
        Path::new("python3"),
        &program,
        &rubrics,
        &json!({"combined_score":2.0}),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        report
            .rubrics
            .iter()
            .map(|r| r.status.as_str())
            .collect::<Vec<_>>(),
        vec!["passed", "passed", "failed", "failed", "not_assessed"]
    );
    assert!(!report.overall_pass);
    let report = rubrics::mechanical(
        Path::new("python3"),
        &program,
        &rubrics,
        &json!({"combined_score":9.0}),
        false,
    )
    .await
    .unwrap();
    assert!(report.rubrics.iter().all(|r| r.status == "not_assessed"));
}

#[test]
fn runtime_requires_exact_pins() {
    assert!(runtime::parse_pins("numpy>=2").is_err());
    assert!(runtime::parse_pins("numpy==2.*").is_err());
    assert!(runtime::parse_pins("--index-url=x").is_err());
    assert_eq!(
        runtime::parse_pins("# comment\nnumpy==2.4.2\nscipy==1.17.1\n")
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn runtime_reuses_environment_and_fails_on_pin_mismatch() {
    let temp = staged();
    let task = temp.path().join("task");
    let record = runtime::provision(
        &task,
        Path::new("python3"),
        Path::new("/nonexistent-wheelhouse"),
    )
    .await
    .unwrap();
    assert!(record.pins.is_empty());
    let again = runtime::provision(
        &task,
        Path::new("python3"),
        Path::new("/nonexistent-wheelhouse"),
    )
    .await
    .unwrap();
    assert_eq!(record.python, again.python);
    fs::write(task.join("requirements.txt"), "pip==0.0.0\n").unwrap();
    assert!(
        runtime::provision(
            &task,
            Path::new("python3"),
            Path::new("/nonexistent-wheelhouse")
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("pin mismatch")
    );
}

/// Write a one-module wheel named `tinyextra` into `dir`, with Python's zip
/// support, so that a test can install from a wheelhouse without a network.
fn write_tiny_wheel(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let script = "import sys, zipfile\n\
        name = 'tinyextra-1.0'\n\
        files = {\n\
        'tinyextra/__init__.py': 'VALUE = 1\\n',\n\
        name + '.dist-info/METADATA': 'Metadata-Version: 2.1\\nName: tinyextra\\nVersion: 1.0\\n',\n\
        name + '.dist-info/WHEEL': 'Wheel-Version: 1.0\\nGenerator: test\\nRoot-Is-Purelib: true\\nTag: py3-none-any\\n',\n\
        }\n\
        record = ''.join(p + ',,\\n' for p in files) + name + '.dist-info/RECORD,,\\n'\n\
        with zipfile.ZipFile(sys.argv[1] + '/' + name + '-py3-none-any.whl', 'w') as z:\n\
        \x20   for p, body in files.items():\n\
        \x20       z.writestr(p, body)\n\
        \x20   z.writestr(name + '.dist-info/RECORD', record)\n";
    let status = std::process::Command::new("python3")
        .args(["-I", "-c", script])
        .arg(dir)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn runtime_installs_extra_packages_from_the_wheelhouse_only() {
    let temp = staged();
    let task = temp.path().join("task");
    let wheelhouse = temp.path().join("wheelhouse");
    write_tiny_wheel(&wheelhouse);
    let extras = vec!["tinyextra".to_string(), "not-in-the-wheelhouse".to_string()];
    let record = runtime::provision_with_extras(&task, Path::new("python3"), &wheelhouse, &extras)
        .await
        .unwrap();
    // The package the wheelhouse holds is installed and recorded; the other is
    // left out without failing the runtime, and nothing is fetched for it.
    assert_eq!(
        record.extras.get("tinyextra").map(String::as_str),
        Some("1.0")
    );
    assert!(!record.extras.contains_key("not-in-the-wheelhouse"));
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(task.join("runtime.json")).unwrap()).unwrap();
    assert_eq!(saved["extras"]["tinyextra"], "1.0");
    // A runtime made without the extras gains them on the next call.
    let plain = staged();
    let plain_task = plain.path().join("task");
    let none = runtime::provision(&plain_task, Path::new("python3"), &wheelhouse)
        .await
        .unwrap();
    assert!(none.extras.is_empty());
    let later =
        runtime::provision_with_extras(&plain_task, Path::new("python3"), &wheelhouse, &extras)
            .await
            .unwrap();
    assert_eq!(
        later.extras.get("tinyextra").map(String::as_str),
        Some("1.0")
    );
}

#[tokio::test]
async fn runtime_refuses_an_extra_package_name_that_pip_would_read_as_an_option() {
    let temp = staged();
    let error = runtime::provision_with_extras(
        &temp.path().join("task"),
        Path::new("python3"),
        Path::new("/nonexistent-wheelhouse"),
        &["--index-url=http://example.invalid".to_string()],
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("invalid extra package name"));
}

#[test]
fn idea_contract_requires_ordered_plan_both_rubric_kinds_and_matching_arrays() {
    use polaris_scientist::phases::investigation::idea;
    let targets = json!([{"id":"metric","description":"combined_score > 1","type":"metric"},{"id":"implementation","description":"Use an explicit score","type":"implementation"}]);
    let valid = json!({"title":"Explicit score","hypothesis":"A higher explicit score improves the objective.","implementation_plan":["Write the score."],"target_rubrics":targets});
    assert_eq!(
        idea::normalize(&valid, &targets, 2).unwrap()["idea_id"],
        "c02_explicit_score"
    );
    for key in ["title", "hypothesis", "implementation_plan"] {
        let mut invalid = valid.clone();
        invalid[key] = json!("");
        assert!(idea::normalize(&invalid, &targets, 2).is_err());
    }
    assert!(idea::normalize(&valid, &json!([]), 2).is_err());
    let mut invalid = valid.clone();
    invalid["target_rubrics"] = json!([{"id":"metric","description":"score","type":"metric"}]);
    assert!(idea::normalize(&invalid, &invalid["target_rubrics"], 2).is_err());
}

#[test]
fn posthoc_filter_removes_blocked_missing_duplicate_and_excess_proposals() {
    use polaris_scientist::phases::posthoc::filter;
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("metrics.json"), "{}").unwrap();
    let valid = json!({"id":"forensics","title":"Inspect score","family":"metrics","requires_retrain":false,"required_artifacts":["metrics.json"],"expected_outputs":["diagnostic.json"],"implementation_plan":["Read retained metrics."]});
    let mut candidates = Vec::new();
    for (key, value) in [
        ("requires_retrain", json!(true)),
        ("implementation_plan", json!(["Rerun evaluate.py"])),
        ("required_artifacts", json!(["missing.json"])),
        ("expected_outputs", json!(["../escape"])),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        candidates.push(invalid);
    }
    candidates.push(valid.clone());
    candidates.push(valid.clone());
    let mut excess = valid;
    excess["id"] = json!("other");
    excess["title"] = json!("Other title");
    excess["family"] = json!("other");
    candidates.push(excess);
    let (accepted, rejected) = filter::select(&candidates, &json!({}), temp.path(), 1);
    assert_eq!(accepted.len(), 1);
    assert_eq!(rejected.len(), 6);
    assert!(
        rejected.last().unwrap()["reason"]
            .as_str()
            .unwrap()
            .contains("cap")
    );
}

#[test]
fn posthoc_filter_accepts_paths_written_as_the_analysis_workspace_shows_them() {
    use polaris_scientist::phases::posthoc::filter;
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("program/results")).unwrap();
    fs::write(temp.path().join("program/results/extra.npz"), "x").unwrap();
    fs::write(temp.path().join("metrics.json"), "{}").unwrap();
    let proposal = |id: &str, artifacts: serde_json::Value, outputs: serde_json::Value| json!({"id":id,"title":format!("Title {id}"),"family":format!("family {id}"),"requires_retrain":false,"required_artifacts":artifacts,"expected_outputs":outputs,"implementation_plan":["Read retained files."]});
    let candidates = vec![
        proposal(
            "workspace_style",
            json!(["inputs/bundle/program/results/extra.npz"]),
            json!(["outputs/analysis_a/plot.png", "table.csv"]),
        ),
        proposal(
            "plain_style",
            json!(["program/results/extra.npz", "metrics.json"]),
            json!(["plain.json"]),
        ),
        proposal(
            "still_missing",
            json!(["inputs/bundle/program/results/absent.npz"]),
            json!(["outputs/x.json"]),
        ),
        proposal(
            "still_unsafe",
            json!(["inputs/bundle/../metrics.json"]),
            json!(["x.json"]),
        ),
        proposal(
            "empty_output_name",
            json!(["metrics.json"]),
            json!(["outputs/"]),
        ),
    ];
    let (accepted, rejected) = filter::select(&candidates, &json!({}), temp.path(), 5);
    let ids: Vec<_> = accepted.iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["workspace_style", "plain_style"]);
    assert_eq!(
        accepted[0]["required_artifacts"],
        json!(["program/results/extra.npz"])
    );
    assert_eq!(
        accepted[0]["expected_outputs"],
        json!(["analysis_a/plot.png", "table.csv"])
    );
    assert_eq!(
        accepted[1]["required_artifacts"],
        json!(["program/results/extra.npz", "metrics.json"])
    );
    assert_eq!(accepted[1]["expected_outputs"], json!(["plain.json"]));
    let reasons: Vec<_> = rejected
        .iter()
        .map(|r| (r["id"].as_str().unwrap(), r["reason"].as_str().unwrap()))
        .collect();
    assert_eq!(
        reasons,
        [
            ("still_missing", "missing or unsafe required artifact"),
            ("still_unsafe", "missing or unsafe required artifact"),
            ("empty_output_name", "missing or unsafe expected output"),
        ]
    );
}

#[test]
fn analysis_attempts_restore_pristine_sources_and_detect_digest_changes() {
    use polaris_scientist::phases::posthoc::filter;
    let temp = tempfile::tempdir().unwrap();
    let frozen = temp.path().join("frozen");
    let work = temp.path().join("work");
    fs::create_dir(&frozen).unwrap();
    fs::write(frozen.join("artifact.json"), "original").unwrap();
    let digest = eval::fingerprint(&frozen).unwrap();
    filter::fresh(&frozen, &digest, &work).unwrap();
    fs::write(work.join("artifact.json"), "mutated").unwrap();
    filter::fresh(&frozen, &digest, &work).unwrap();
    assert_eq!(
        fs::read_to_string(work.join("artifact.json")).unwrap(),
        "original"
    );
    fs::write(frozen.join("artifact.json"), "tampered").unwrap();
    assert!(filter::fresh(&frozen, &digest, &work).is_err());
}

#[test]
fn retrofit_rejects_evaluator_changes_and_variant_caps_preserve_order() {
    use polaris_scientist::phases::ablations::artifacts;
    let temp = staged();
    let canonical = temp.path().join("task/evaluate.py");
    let digest = eval::file_digest(&canonical).unwrap();
    let good = artifacts::check_retrofit(
        &canonical,
        &digest,
        &temp.path().join("task"),
        &temp.path().join("task/program_0"),
    );
    assert!(good.is_ok());
    fs::write(&canonical, "# changed").unwrap();
    assert!(
        artifacts::check_retrofit(
            &canonical,
            &digest,
            temp.path(),
            &temp.path().join("task/program_0")
        )
        .is_err()
    );
    let candidates = vec![
        json!({"variant_id":"a","kind":"ablation"}),
        json!({"variant_id":"b","kind":"ablation"}),
        json!({"variant_id":"c","kind":"baseline"}),
    ];
    let selected = artifacts::selected(&candidates, &json!({}), 1, 1);
    assert_eq!(
        selected
            .iter()
            .map(|v| v["variant_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["a", "c"]
    );
}

#[test]
fn candidate_hash_covers_data_named_like_evaluator_outputs_outside_the_reserved_results_directory()
{
    let temp = staged();
    let program = temp.path().join("task/program_0");
    let before = eval::fingerprint(&program).unwrap();
    fs::write(program.join("metrics.json"), "candidate input").unwrap();
    assert_ne!(before, eval::fingerprint(&program).unwrap());
    fs::remove_file(program.join("metrics.json")).unwrap();
    fs::create_dir(program.join("results")).unwrap();
    fs::write(program.join("results/metrics.json"), "evaluator output").unwrap();
    assert_eq!(before, eval::fingerprint(&program).unwrap());
}

#[test]
fn candidate_hash_ignores_the_run_outputs_a_program_writes_next_to_its_code() {
    let temp = staged();
    let program = temp.path().join("task/program_0");
    let before = eval::fingerprint(&program).unwrap();
    for folder in [program.clone(), program.join("src")] {
        fs::write(folder.join("_results.json"), "{}").unwrap();
        fs::create_dir_all(folder.join("analysis")).unwrap();
        fs::write(folder.join("analysis/run_summary.csv"), "a,b\n1,2\n").unwrap();
    }
    assert_eq!(before, eval::fingerprint(&program).unwrap());
    // The same names deeper in the tree, or any other file, are still part of the program.
    fs::create_dir_all(program.join("src/lib/analysis")).unwrap();
    fs::write(program.join("src/lib/analysis/helper.py"), "x = 1\n").unwrap();
    assert_ne!(before, eval::fingerprint(&program).unwrap());
    fs::remove_dir_all(program.join("src/lib")).unwrap();
    assert_eq!(before, eval::fingerprint(&program).unwrap());
    fs::write(program.join("src/notes.txt"), "x").unwrap();
    assert_ne!(before, eval::fingerprint(&program).unwrap());
}

#[tokio::test]
async fn a_program_that_writes_its_run_outputs_beside_its_code_is_scored_and_reused() {
    let temp = staged();
    let req = request(temp.path());
    let source = "from pathlib import Path\n\
        def solution():\n\
        \x20   here = Path(__file__).parent\n\
        \x20   (here / '_results.json').write_text('{}')\n\
        \x20   (here / 'analysis').mkdir(exist_ok=True)\n\
        \x20   (here / 'analysis' / 'run_summary.csv').write_text('a,b\\n1,2\\n')\n\
        \x20   return 1.5\n";
    fs::write(req.program_dir.join("src/program.py"), source).unwrap();
    let first = eval::run_evaluator(req.clone()).await.unwrap();
    assert_eq!(first.score(), Some(1.5));
    assert!(first.manifest.integrity_valid);
    assert!(
        req.program_dir
            .join("src/analysis/run_summary.csv")
            .is_file()
    );
    let second = eval::run_evaluator(req).await.unwrap();
    assert!(second.reused);
}

#[tokio::test]
async fn cancelling_evaluation_kills_its_process_group() {
    let temp = staged();
    let req = request(temp.path());
    let pid_file = temp.path().join("cancelled_child.pid");
    fs::write(&req.evaluator,format!("import subprocess,time\nfrom pathlib import Path\np=subprocess.Popen(['sleep','60'])\nPath({:?}).write_text(str(p.pid))\ntime.sleep(60)\n",pid_file.to_string_lossy())).unwrap();
    let task = tokio::spawn(eval::run_evaluator(req));
    for _ in 0..200 {
        if pid_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(pid_file.exists());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let pid = fs::read_to_string(pid_file).unwrap();
    for _ in 0..200 {
        if !Path::new(&format!("/proc/{}", pid.trim())).exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("cancelled evaluator left a descendant behind");
}

#[test]
fn posthoc_receipts_require_grounding_guardrails_and_resolving_evidence_paths() {
    use polaris_scientist::phases::posthoc::filter;
    let temp = tempfile::tempdir().unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bundle = temp.path().join("inputs/bundle");
    let outputs = temp.path().join("outputs");
    files::copy_tree(&fixture().join("program_0"), &bundle.join("program")).unwrap();
    files::write_json(
        &bundle.join("program/results/metrics.json"),
        &json!({"combined_score":2.0}),
    )
    .unwrap();
    files::copy_tree(
        &assets.join("tests/scenarios/posthoc/roles/posthoc_analyst/01/outputs"),
        &outputs,
    )
    .unwrap();
    let proposals: serde_json::Value =
        serde_json::from_slice(
            &fs::read(assets.join(
                "tests/scenarios/posthoc/roles/posthoc_proposer/01/outputs/posthoc_bundle.json",
            ))
            .unwrap(),
        )
        .unwrap();
    let spec = &proposals["analyses"][0];
    let digest = eval::fingerprint(&bundle).unwrap();
    assert!(filter::validate(&bundle, &digest, &outputs, spec, 1, 0.01).is_ok());
    let manifest_path = outputs.join("analysis_manifest.json");
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    for (field, value) in [
        ("reran_evaluator", json!(true)),
        ("grounded", json!(false)),
        ("analysis_id", json!("wrong")),
        ("evidence_paths", json!(["../outside"])),
        ("primary_metric_tolerance", json!(1.0)),
    ] {
        let mut invalid = original.clone();
        invalid[field] = value;
        files::write_json(&manifest_path, &invalid).unwrap();
        assert!(
            filter::validate(&bundle, &digest, &outputs, spec, 1, 0.01).is_err(),
            "{field}"
        );
    }
}

#[test]
fn posthoc_rejections_name_the_path_or_rubric_field_the_analyst_must_correct() {
    use polaris_scientist::phases::posthoc::filter;
    let temp = tempfile::tempdir().unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bundle = temp.path().join("inputs/bundle");
    let outputs = temp.path().join("outputs");
    files::copy_tree(&fixture().join("program_0"), &bundle.join("program")).unwrap();
    files::write_json(
        &bundle.join("program/results/metrics.json"),
        &json!({"combined_score":2.0}),
    )
    .unwrap();
    files::copy_tree(
        &assets.join("tests/scenarios/posthoc/roles/posthoc_analyst/01/outputs"),
        &outputs,
    )
    .unwrap();
    let proposals: serde_json::Value =
        serde_json::from_slice(
            &fs::read(assets.join(
                "tests/scenarios/posthoc/roles/posthoc_proposer/01/outputs/posthoc_bundle.json",
            ))
            .unwrap(),
        )
        .unwrap();
    let spec = &proposals["analyses"][0];
    let digest = eval::fingerprint(&bundle).unwrap();
    let manifest_path = outputs.join("analysis_manifest.json");
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let rejection = |manifest: &serde_json::Value| {
        files::write_json(&manifest_path, manifest).unwrap();
        filter::validate(&bundle, &digest, &outputs, spec, 1, 0.01)
            .unwrap_err()
            .to_string()
    };

    // A list of outputs written from the workspace root, not from outputs/, is refused by name.
    let mut prefixed = original.clone();
    let listed: Vec<_> = original["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| json!(format!("outputs/{}", p.as_str().unwrap())))
        .collect();
    prefixed["outputs"] = json!(listed);
    let message = rejection(&prefixed);
    assert!(message.contains(&listed[0].to_string()), "{message}");
    assert!(message.contains("relative to outputs/"), "{message}");

    // Evidence under the workspace's inputs/ but outside inputs/bundle/ is refused by name.
    fs::write(temp.path().join("inputs/analysis_spec.json"), "{}").unwrap();
    let mut spec_evidence = original.clone();
    spec_evidence["evidence_paths"] = json!(["inputs/analysis_spec.json"]);
    let message = rejection(&spec_evidence);
    assert!(message.contains("inputs/analysis_spec.json"), "{message}");

    // A rubric entry without an evidence sentence is refused with the rule spelled out.
    files::write_json(&manifest_path, &original).unwrap();
    let status_path = outputs.join("rubric_status.json");
    let mut status: serde_json::Value =
        serde_json::from_slice(&fs::read(&status_path).unwrap()).unwrap();
    status["rubrics"][0]
        .as_object_mut()
        .unwrap()
        .remove("evidence");
    files::write_json(&status_path, &status).unwrap();
    let message = filter::validate(&bundle, &digest, &outputs, spec, 1, 0.01)
        .unwrap_err()
        .to_string();
    assert!(message.contains("evidence string"), "{message}");
}
