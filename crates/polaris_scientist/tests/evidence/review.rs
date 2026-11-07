//! The review phase after the paper phase.

use serde_json::Value;

use crate::harness::{Harness, Run};

const AUDIT_FAIL: &str = "AUDIT_VERDICT: FAIL\nAUDIT_SUMMARY: one claim is too strong\nAUDIT_FINDING_COUNT: 1\nAUDIT_FINDING: id=f1 || section=Results || claim=The score doubles. || issue=The claim overstates the margin. || evidence=paper_data.json result.best_correct_score || fix=Soften the sentence.";

fn status_file(run: &Run) -> Value {
    run.json("ai_review_status.json")
}

#[tokio::test]
async fn with_the_external_reviewer_off_the_phase_records_why_it_was_skipped() {
    let harness = Harness::new();
    let run = harness.run(harness.scenario()).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    assert_eq!(run.outcome.phases["review"], "skipped", "{:?}", run.outcome);
    let status = status_file(&run);
    assert_eq!(status["status"], "skipped");
    assert_eq!(
        status["reason"],
        "external reviewer disabled; scored after the campaign"
    );
    assert!(
        !run.paper().join("revision_status.json").exists(),
        "with no review the revision step writes nothing"
    );
    assert_eq!(
        run.roles().len(),
        8,
        "the review phase makes no model call: {:?}",
        run.roles()
    );
}

#[tokio::test]
async fn with_the_adapter_mode_the_phase_records_that_the_review_is_deferred() {
    let mut harness = Harness::new();
    harness.set("review.mode", "\"adapter\"");
    let run = harness.run(harness.scenario()).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    let status = status_file(&run);
    assert_eq!(status["status"], "deferred");
    assert_eq!(
        status["reason"],
        "the external reviewer runs after the campaign"
    );
}

#[tokio::test]
async fn a_review_that_is_present_makes_the_revision_report_that_it_is_not_built() {
    let harness = Harness::new();
    let run = harness
        .run_with(harness.scenario(), |root| {
            let paper = root.join("paper");
            std::fs::create_dir_all(&paper).expect("paper folder");
            std::fs::write(paper.join("ai_review.json"), "{\"score\": 5}").expect("review");
        })
        .await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    let revision = run.json("revision_status.json");
    assert_eq!(revision["status"], "not_built");
    assert!(
        revision["reason"]
            .as_str()
            .expect("reason")
            .contains("not built")
    );
    // The paper is as the paper phase left it.
    assert_eq!(run.json("paper_manifest.json")["status"], "complete");
}

#[tokio::test]
async fn the_review_phase_does_not_run_after_an_incomplete_paper() {
    let mut harness = Harness::new();
    harness.set("paper.max_repair_rounds", "1");
    let mut scenario = harness.scenario();
    scenario.roles.insert(
        "evidence_auditor".into(),
        vec![crate::harness::reply(AUDIT_FAIL)],
    );
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "failed", "{:?}", run.outcome);
    assert_eq!(run.outcome.phases["review"], "skipped", "{:?}", run.outcome);
    assert!(
        !run.paper().join("ai_review_status.json").exists(),
        "the paper failure is the result of the run"
    );
}
