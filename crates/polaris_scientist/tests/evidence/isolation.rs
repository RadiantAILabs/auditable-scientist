//! A role that only replies reads a copy of the paper. The step fails when the
//! paper, or that copy, changes while the role works.

use std::path::Path;
use std::time::Duration;

use polaris_cosmos::flow::ScriptEntry;
use serde_json::json;

use crate::harness::{Harness, Run};

const AUDIT_PASS: &str = "AUDIT_VERDICT: PASS\nAUDIT_SUMMARY: ok\nAUDIT_FINDING_COUNT: 0";
const REVIEW: &str = r#"{"status": "pass", "summary": "Short and grounded.", "strengths": [], "major_comments": [], "minor_comments": [], "questions": [], "score": {"value": 6, "label": "weak_accept"}}"#;

/// A reply that takes long enough for the test to change the paper meanwhile.
fn slow_reply(text: &str) -> ScriptEntry {
    ScriptEntry {
        final_text: text.to_string(),
        delay_ms: 1_500,
        ..ScriptEntry::default()
    }
}

/// Waits until a call of the step family `prefix` has its workspace, then
/// appends a line to `paper.tex` in the paper folder.
async fn edit_paper_once_staged(root: &Path, prefix: &str) {
    let steps = root.join("work/paper");
    for _ in 0..600 {
        let staged = std::fs::read_dir(&steps)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .any(|e| e.path().join("001/inputs/paper/paper.tex").is_file());
        if staged {
            let tex = root.join("paper/paper.tex");
            let mut text = std::fs::read_to_string(&tex).expect("paper.tex");
            text.push_str("% changed while a role was reading\n");
            std::fs::write(&tex, text).expect("edit paper.tex");
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no call of {prefix} was staged");
}

async fn run_and_edit(
    harness: &Harness,
    scenario: polaris_cosmos::flow::Scenario,
    prefix: &str,
) -> Run {
    let root = harness.root();
    let (run, ()) = tokio::join!(harness.run(scenario), edit_paper_once_staged(&root, prefix));
    run
}

#[tokio::test]
async fn the_auditor_reads_a_copy_and_may_run_commands_but_has_no_grant_to_edit() {
    let harness = Harness::new();
    let run = harness.run(harness.scenario()).await;
    let call = run
        .calls
        .iter()
        .find(|c| c["role"] == "evidence_auditor")
        .expect("auditor call");
    assert_eq!(call["grants"]["edit"], false);
    assert_eq!(
        call["grants"]["commands"],
        json!([
            "ls",
            "ls *",
            "cat *",
            "rg *",
            "diff *",
            "python *",
            "python3 *",
            "grep",
            "grep *",
            "head",
            "head *",
            "tail",
            "tail *",
            "wc",
            "wc *",
            "sort",
            "sort *",
            "uniq",
            "uniq *",
            "cd *"
        ])
    );
    let workspace = Path::new(call["workspace"].as_str().expect("workspace"));
    assert!(
        workspace.join("inputs/paper/paper.tex").is_file(),
        "the auditor reads a copy of the paper"
    );
    assert!(
        !workspace.join("outputs").exists()
            || workspace
                .join("outputs")
                .read_dir()
                .unwrap()
                .next()
                .is_none(),
        "the auditor has nothing to write"
    );
}

#[tokio::test]
async fn an_audit_fails_when_the_paper_changes_while_it_is_read() {
    let harness = Harness::new();
    let mut scenario = harness.scenario();
    scenario
        .roles
        .insert("evidence_auditor".into(), vec![slow_reply(AUDIT_PASS)]);
    let run = run_and_edit(&harness, scenario, "audit").await;
    assert_eq!(run.paper_status(), "failed", "{:?}", run.outcome);
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["status"], "incomplete");
    assert_eq!(manifest["audit_state"], "missing");
    let reason = manifest["reason"].as_str().expect("reason");
    assert!(
        reason.contains("the paper changed while it was audited: paper.tex"),
        "{reason}"
    );
    // The passing verdict of an audit that read a moving paper is not recorded.
    assert!(!run.paper().join("paper_evidence_audit.json").exists());
}

#[tokio::test]
async fn a_review_fails_when_the_paper_changes_while_it_is_read() {
    let mut harness = Harness::new();
    harness.set("paper.review_rounds", "1");
    let mut scenario = harness.scenario();
    scenario
        .roles
        .insert("presentation_reviewer".into(), vec![slow_reply(REVIEW)]);
    let run = run_and_edit(&harness, scenario, "review").await;
    assert_eq!(run.paper_status(), "failed", "{:?}", run.outcome);
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["status"], "incomplete");
    let reason = manifest["reason"].as_str().expect("reason");
    assert!(
        reason.contains("the paper changed while it was reviewed: paper.tex"),
        "{reason}"
    );
    assert!(!run.paper().join("paper_review.json").exists());
}

#[tokio::test]
async fn a_role_that_writes_into_its_copy_of_the_paper_is_stopped() {
    let harness = Harness::new();
    let mut scenario = harness.scenario();
    // The copy is read-only: the scripted answer cannot overwrite it.
    let dir = harness.variant("edited_copy", &[]);
    std::fs::create_dir_all(dir.join("inputs/paper")).expect("copy folder");
    std::fs::write(dir.join("inputs/paper/paper.tex"), "% edited\n").expect("edit");
    scenario.roles.insert(
        "evidence_auditor".into(),
        vec![ScriptEntry {
            final_text: AUDIT_PASS.to_string(),
            files: Some(dir),
            ..ScriptEntry::default()
        }],
    );
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "failed", "{:?}", run.outcome);
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["audit_state"], "missing");
    assert!(!run.paper().join("paper_evidence_audit.json").exists());
    // The paper itself is as the writer left it.
    let tex = std::fs::read_to_string(run.paper().join("paper.tex")).expect("paper.tex");
    assert!(!tex.contains("% edited"), "{tex}");
}

#[tokio::test]
async fn a_manuscript_writer_works_outside_the_paper_folder() {
    let harness = Harness::new();
    let run = harness.run(harness.scenario()).await;
    let writer = run
        .calls
        .iter()
        .find(|c| c["role"] == "paper_writer")
        .expect("writer call");
    assert_eq!(writer["grants"]["edit"], true);
    let workspace = Path::new(writer["workspace"].as_str().expect("workspace"));
    assert!(workspace.join("outputs").is_dir());
    assert!(
        !workspace.starts_with(run.paper()),
        "the writer works outside the paper folder"
    );
}
