use polaris_scientist::{
    durable::copy_snapshot,
    phases::{
        ablations::{artifacts, contracts},
        task::files,
    },
};
use serde_json::json;
use std::{fs, path::Path};

#[test]
fn frozen_source_clones_are_writable_and_unchanged_sources_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/circpack/task/program_0");
    let frozen = temp.path().join("inputs/program_0");
    copy_snapshot(&source, &frozen, true).unwrap();
    assert_eq!(
        fs::metadata(frozen.join("src/program.py"))
            .unwrap()
            .permissions()
            .mode()
            & 0o222,
        0
    );
    let protected = temp.path().join("protected_copy");
    files::copy_tree(&frozen, &protected).unwrap();
    assert_eq!(
        fs::metadata(protected.join("src/program.py"))
            .unwrap()
            .permissions()
            .mode()
            & 0o222,
        0
    );
    let broken = temp.path().join("broken_clone");
    assert!(
        std::process::Command::new("cp")
            .arg("-r")
            .arg(&frozen)
            .arg(&broken)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::metadata(broken.join("src/program.py"))
            .unwrap()
            .permissions()
            .mode()
            & 0o222,
        0
    );
    assert!(contracts::require_source_change(&frozen, &broken).is_err());
    for (name, clone) in [("host", true), ("snapshot", false), ("shell", false)] {
        let output = temp.path().join(name);
        if clone {
            files::copy_editable_tree(&frozen, &output).unwrap();
        } else if name == "snapshot" {
            copy_snapshot(&frozen, &output, false).unwrap();
        } else {
            assert!(
                std::process::Command::new("cp")
                    .args(["-r", "--no-preserve=mode"])
                    .arg(&frozen)
                    .arg(&output)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        assert_eq!(
            fs::metadata(output.join("src/program.py"))
                .unwrap()
                .permissions()
                .mode()
                & 0o600,
            0o600
        );
        assert_ne!(
            fs::metadata(output.join("src"))
                .unwrap()
                .permissions()
                .mode()
                & 0o200,
            0
        );
        assert!(
            contracts::require_source_change(&frozen, &output)
                .unwrap_err()
                .to_string()
                .contains("unchanged")
        );
        fs::write(output.join("README.md"), "Claims a different method").unwrap();
        fs::write(output.join("src/README.md"), "Claims a different method").unwrap();
        assert!(contracts::require_source_change(&frozen, &output).is_err());
        fs::write(
            output.join("src/program.py"),
            "def solution(): return 2.0\n",
        )
        .unwrap();
        contracts::require_source_change(&frozen, &output).unwrap();
    }
    // Restore directory access only after proving that the frozen input was preserved.
    for tree in [&frozen, &broken] {
        for entry in walkdir::WalkDir::new(tree) {
            let entry = entry.unwrap();
            if entry.file_type().is_dir() {
                files::owner_writable(entry.path()).unwrap();
            }
        }
    }
    assert_eq!(
        fs::read(frozen.join("src/program.py")).unwrap(),
        fs::read(source.join("src/program.py")).unwrap()
    );
}

#[test]
fn trusted_contract_cannot_be_replaced_by_proposed_metric_only_rubrics() {
    let temp = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/circpack/task/program_0");
    let spec =
        json!({"variant_id":"remove_feature","kind":"ablation","target_rubrics":[{"id":"valid"}]});
    let directory = artifacts::materialize(temp.path(), &spec, &source).unwrap();
    let targets: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("target_rubrics.json")).unwrap()).unwrap();
    assert_eq!(targets.as_array().unwrap().len(), 9);
    for id in [
        "variant_intent_preserved",
        "single_change_scope",
        "correctness_recorded",
    ] {
        assert!(targets.as_array().unwrap().iter().any(|r| r["id"] == id));
    }
    assert_eq!(
        contracts::targets(
            &json!({"variant_id":"external","kind":"baseline","base_program":"reference"})
        )
        .len(),
        12
    );
    let targets = contracts::targets(&spec);
    let valid = json!({"variant_id":"forged","overall_pass":true,"rubrics":targets.iter().map(|r|json!({"id":r["id"],"pass":true,"evidence":"source:4 implements declared removal"})).collect::<Vec<_>>()});
    assert!(artifacts::verifier_pass(&targets, &valid, true).unwrap());
    for change in 0..5 {
        let mut bad = valid.clone();
        match change {
            0 => {
                bad["rubrics"][0]["id"] = json!("valid");
            }
            1 => {
                bad["rubrics"][0] = bad["rubrics"][1].clone();
            }
            2 => {
                bad["rubrics"][0]["pass"] = json!("true");
            }
            3 => {
                bad["rubrics"][0]["evidence"] = json!("");
            }
            _ => {
                bad["rubrics"].as_array_mut().unwrap().pop();
            }
        }
        assert!(artifacts::verifier_pass(&targets, &bad, true).is_err());
    }
    let mut failed = valid.clone();
    failed["rubrics"][6]["pass"] = json!(false);
    assert!(!artifacts::verifier_pass(&targets, &failed, true).unwrap());
    assert!(!artifacts::verifier_pass(&targets, &valid, false).unwrap());
}

#[tokio::test]
async fn mechanical_analysis_checks_accept_header_only_timeseries_and_reject_false_metrics() {
    use polaris_scientist::eval::{self, EvalRequest};
    use std::{collections::BTreeMap, time::Duration};
    let temp = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let program = temp.path().join("program");
    files::copy_tree(
        &fixtures.join("scenarios/ablations/roles/variant_author/01/outputs/program_1"),
        &program,
    )
    .unwrap();
    let evaluation = eval::run_evaluator(EvalRequest {
        python: "python3".into(),
        evaluator: fixtures.join("fixtures/circpack/task/evaluate.py"),
        program_dir: program.clone(),
        results_dir: program.join("results"),
        timeout: Duration::from_secs(30),
        grace: Duration::from_secs(1),
        environment: BTreeMap::new(),
    })
    .await
    .unwrap();
    let checks = contracts::mechanical(&program, &evaluation).unwrap();
    assert_eq!(checks.len(), 6);
    assert!(checks.iter().all(|r| r["pass"] == true), "{checks:?}");
    let csv = program.join("src/analysis/run_summary.csv");
    let original = fs::read_to_string(&csv).unwrap();
    fs::write(&csv, original.replace("True,2.0,True", "False,9.0,True")).unwrap();
    let checks = contracts::mechanical(&program, &evaluation).unwrap();
    for id in [
        "metrics_consistent_with_metrics_json",
        "correctness_recorded",
    ] {
        assert_eq!(
            checks.iter().find(|r| r["id"] == id).unwrap()["pass"],
            false
        );
    }
    fs::remove_file(program.join("src/analysis/timeseries.csv")).unwrap();
    let checks = contracts::mechanical(&program, &evaluation).unwrap();
    assert_eq!(checks[0]["pass"], false);
    assert_eq!(checks[3]["pass"], false);
}
