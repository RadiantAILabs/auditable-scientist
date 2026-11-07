//! The dossier holds a specification, a source diff and a verification record
//! for every ablation variant and control, the evidence audit reads them, and
//! a finding about the description of a variant reaches the repairer.

use std::path::Path;

use serde_json::Value;

use crate::harness::{Harness, Run, answer, reply};

const AUDIT_PASS: &str = "AUDIT_VERDICT: PASS\nAUDIT_SUMMARY: ok\nAUDIT_FINDING_COUNT: 0";
const AUDIT_VARIANT_FAIL: &str = "AUDIT_VERDICT: FAIL\nAUDIT_SUMMARY: one ablation is described as removing a step that its source still runs\nAUDIT_FINDING_COUNT: 1\nAUDIT_FINDING: id=variant-description-1 || section=Ablations || claim=The no_swap variant removes the polishing step. || issue=The source diff of no_swap changes only how the search starts; the call to polish is a context line, so the variant still runs it. || evidence=dossier/ablations/variants/no_swap/source.diff keeps the call to polish; paper_data.json ablation_records entry no_swap. || fix=Describe the variant by the change the diff shows, or remove the claim that the polishing step is removed.";

const SPEC: &str = "code_root = \"src\"\n[entrypoint]\npath = \"program.py\"\n";
const CONTROL: &str = "def search():\n    cen = optimize()\n    out = polish(cen)\n    return out\n\n\ndef polish(c):\n    return c\n";

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
    std::fs::write(path, text).expect("file");
}

fn program(root: &Path, id: &str, index: u32, source: &str, evaluated: bool) {
    let base = format!("ablations/variants/{id}/task_bundle/program_{index}");
    write(root, &format!("{base}/program_spec.toml"), SPEC);
    write(root, &format!("{base}/src/program.py"), source);
    if evaluated {
        write(root, &format!("{base}/results/correct.json"), "{}");
    }
}

/// Adds the variant folders of a control, a variant that changes one line and
/// keeps the polish, and a variant whose source equals the control's and that
/// has no verification record.
fn add_variants(root: &Path) {
    write(
        root,
        "ablations/pipeline_summary.json",
        r#"{"variants": [
            {"id": "no_swap", "kind": "ablation", "status": "completed", "score": 1.5, "correct": true, "evals": 5},
            {"id": "keep_all", "kind": "ablation", "status": "completed", "score": 2.0, "correct": true, "evals": 5}],
          "controls": [
            {"id": "control_best", "kind": "control", "status": "completed", "score": 2.0, "correct": true, "evals": 5}],
          "replications_requested": 5, "replications_observed": 5, "cross_task": "not_applicable", "caps_hit": []}"#,
    );
    program(root, "control_best", 0, CONTROL, true);
    write(
        root,
        "ablations/variants/control_best/variant_spec.json",
        r#"{"variant_id": "control_best"}"#,
    );
    program(root, "no_swap", 0, CONTROL, false);
    program(
        root,
        "no_swap",
        1,
        &CONTROL.replace("cen = optimize()", "cen = optimize_once()"),
        true,
    );
    write(
        root,
        "ablations/variants/no_swap/variant_spec.json",
        r#"{"variant_id": "no_swap", "hypothesis": "one start, without the polish"}"#,
    );
    write(
        root,
        "ablations/variants/no_swap/task_bundle/program_1/results/rubric_status.json",
        r#"{"variant_id": "no_swap", "overall_pass": true}"#,
    );
    program(root, "keep_all", 0, CONTROL, false);
    program(root, "keep_all", 1, CONTROL, true);
    write(
        root,
        "ablations/variants/keep_all/variant_spec.json",
        r#"{"variant_id": "keep_all"}"#,
    );
}

fn record(run: &Run, id: &str) -> Value {
    run.json("paper_data.json")["ablation_records"]
        .as_array()
        .expect("ablation_records")
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no record for {id}"))
        .clone()
}

fn workspace_of(run: &Run, role: &str, ordinal: usize) -> std::path::PathBuf {
    let call = run
        .calls
        .iter()
        .filter(|c| c["role"] == role)
        .nth(ordinal)
        .unwrap_or_else(|| panic!("no call {ordinal} of {role}"));
    Path::new(call["workspace"].as_str().expect("workspace")).to_path_buf()
}

/// Stages the dossier of a saved run into a new paper folder, so that the
/// variant files can be read without a model call. It reads the run folder
/// from `STAGE_RUN_ROOT` and the paper folder to write from `STAGE_PAPER_DIR`;
/// run it with `--ignored --nocapture`. Work on a copy of the run: staging
/// writes only into the paper folder, but the run is the evidence.
#[test]
#[ignore = "stages the saved run named by STAGE_RUN_ROOT into STAGE_PAPER_DIR"]
fn stage_a_saved_run_and_list_its_variant_records() {
    let root = std::env::var("STAGE_RUN_ROOT").expect("STAGE_RUN_ROOT");
    let paper = std::env::var("STAGE_PAPER_DIR").expect("STAGE_PAPER_DIR");
    let dossier = polaris_scientist::phases::paper::stage::run(Path::new(&root), Path::new(&paper))
        .expect("staging");
    for record in dossier.data["ablation_records"]
        .as_array()
        .expect("ablation_records")
    {
        println!("{record}");
    }
}

#[tokio::test]
async fn the_dossier_holds_the_files_of_every_variant_and_the_paper_data_lists_them() {
    let harness = Harness::new();
    let run = harness.run_with(harness.scenario(), add_variants).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    let dossier = run.paper().join("dossier/ablations/variants");

    // A variant that changes the source: the diff shows the change, the
    // unchanged call to the polish is context, and the record is kept.
    let diff = std::fs::read_to_string(dossier.join("no_swap/source.diff")).expect("source.diff");
    assert!(diff.contains("-    cen = optimize()"), "{diff}");
    assert!(diff.contains("+    cen = optimize_once()"), "{diff}");
    assert!(diff.contains("     out = polish(cen)"), "{diff}");
    assert!(dossier.join("no_swap/variant_spec.json").is_file());
    assert!(dossier.join("no_swap/verification.json").is_file());
    let no_swap = record(&run, "no_swap");
    assert_eq!(no_swap["scored_program"], "program_1");
    assert_eq!(no_swap["compared_with"], "control_best/program_0");
    assert_eq!(no_swap["source_diff"]["changed"], true);
    assert_eq!(no_swap["verification"]["overall_pass"], true);

    // A variant with no diff and no verification record says so.
    let same = std::fs::read_to_string(dossier.join("keep_all/source.diff")).expect("source.diff");
    assert!(same.contains("identical"), "{same}");
    assert!(!dossier.join("keep_all/verification.json").exists());
    let keep_all = record(&run, "keep_all");
    assert_eq!(keep_all["source_diff"]["changed"], false);
    assert_eq!(keep_all["verification"]["found"], false);
    assert_eq!(keep_all["files"]["verification"], Value::Null);
    assert!(
        keep_all["verification"]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "{keep_all}"
    );

    // The control is listed once and is not compared with itself.
    let control = record(&run, "control_best");
    assert_eq!(control["source_diff"]["changed"], false);
    assert_eq!(control["verification"]["found"], false);
}

#[tokio::test]
async fn a_finding_about_the_description_of_a_variant_reaches_the_repairer() {
    let harness = Harness::new();
    let mut scenario = harness.scenario();
    let good = harness.paper_variant("good", str::to_string);
    scenario.roles.insert(
        "evidence_auditor".into(),
        vec![reply(AUDIT_VARIANT_FAIL), reply(AUDIT_PASS)],
    );
    scenario
        .roles
        .insert("paper_repairer".into(), vec![answer(&good)]);
    let run = harness.run_with(scenario, add_variants).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    assert_eq!(
        &run.roles()[run.roles().len() - 3..],
        ["evidence_auditor", "paper_repairer", "evidence_auditor"],
        "the finding makes one repair, then the audit passes"
    );
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["repair_rounds_used"], 1);

    // The auditor reads the variant files in its copy of the paper.
    let audit = workspace_of(&run, "evidence_auditor", 0);
    for name in ["source.diff", "variant_spec.json", "verification.json"] {
        let path = audit
            .join("inputs/paper/dossier/ablations/variants/no_swap")
            .join(name);
        assert!(path.is_file(), "the auditor sees {}", path.display());
    }

    // The repairer reads the finding in full, and the file it points to.
    let repair = workspace_of(&run, "paper_repairer", 0);
    let feedback = std::fs::read_to_string(repair.join("inputs/paper/repair_feedback.md"))
        .expect("repair feedback");
    for part in [
        "variant-description-1 (Ablations)",
        "the call to polish is a context line",
        "dossier/ablations/variants/no_swap/source.diff",
        "Describe the variant by the change the diff shows",
    ] {
        assert!(feedback.contains(part), "{part}\n{feedback}");
    }
    assert!(
        repair
            .join("inputs/paper/dossier/ablations/variants/no_swap/source.diff")
            .is_file(),
        "the repairer sees the diff the finding names"
    );
}
