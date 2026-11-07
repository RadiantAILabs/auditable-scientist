//! The paper phase inside a campaign: order, finalize, counters and budget.

use polaris_cosmos::flow::{ScriptEntry, Usage};
use polaris_scientist::roles;
use serde_json::{Value, json};

use crate::harness::{Harness, answer, crate_path, reply};

const FULL_ORDER: [&str; 8] = [
    "reference_planner",
    "paper_strategist",
    "posthoc_ranker",
    "paper_writer",
    "citation_gap_analyst",
    "reference_planner_postdraft",
    "paper_enricher",
    "evidence_auditor",
];

fn without<'a>(order: &[&'a str], role: &str) -> Vec<&'a str> {
    order.iter().copied().filter(|r| *r != role).collect()
}

const AUDIT_PASS: &str = "AUDIT_VERDICT: PASS\nAUDIT_SUMMARY: ok\nAUDIT_FINDING_COUNT: 0";
const AUDIT_FAIL: &str = "AUDIT_VERDICT: FAIL\nAUDIT_SUMMARY: one claim is too strong\nAUDIT_FINDING_COUNT: 1\nAUDIT_FINDING: id=f1 || section=Results || claim=The score doubles. || issue=The claim overstates the margin. || evidence=paper_data.json result.best_correct_score || fix=Soften the sentence.";

fn usage(micros: u64) -> Option<Usage> {
    Some(Usage {
        model_id: "aws.bedrock/global.anthropic.claude-sonnet-5-5".into(),
        input: 100,
        output: 20,
        requests: 1,
        micros,
        ..Usage::default()
    })
}

fn counters(run: &crate::harness::Run) -> Value {
    let text = std::fs::read_to_string(run.root.join(".state/counters.json")).expect("counters");
    serde_json::from_str(&text).expect("counters are JSON")
}

#[tokio::test]
async fn the_phase_calls_its_roles_in_topology_order_and_finalizes() {
    let harness = Harness::new();
    let run = harness.run(harness.scenario()).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    assert_eq!(run.roles(), FULL_ORDER);

    let paper = run.paper();
    for name in [
        "paper_data.json",
        "references.bib",
        "references_provenance.json",
        "claims_grounding.json",
        "paper.tex",
        "paper.pdf",
        "figure_manifest.json",
        "validation_report.json",
        "paper_evidence_audit.json",
        "paper_evidence_audit.md",
        "paper_manifest.json",
        "ai_review_status.json",
        "claims_check.json",
        "posthoc_ranking.json",
        "rounds/01/paper.tex",
    ] {
        assert!(paper.join(name).is_file(), "{name} is missing");
    }
    assert!(
        !paper.join("figures").exists(),
        "the scenario's paper has no figure files"
    );
    assert_eq!(
        run.json("figure_manifest.json")["concept_figure"]["status"],
        "none"
    );
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["status"], "complete");
    assert_eq!(manifest["repair_rounds_used"], 0);
    assert_eq!(manifest["audit_state"], "passed");
    assert_eq!(manifest["caps_hit"], json!([]));
    assert_eq!(run.json("paper_evidence_audit.json")["state"], "passed");
    assert_eq!(run.json("validation_report.json")["ok"], true);
    let claims = run.json("claims_check.json");
    assert_eq!(claims["saved_model_audit"]["state"], "passed");
    assert_eq!(claims["mechanical_check"]["numbers_printed"], 4);
    assert_eq!(claims["mechanical_check"]["numbers_backed"], 4);
    assert_eq!(claims["mechanical_check"]["numbers_unbacked"], 0);
    let data = run.json("paper_data.json");
    assert_eq!(data["posthoc_analyses"][0]["rank"], 1);
    let provenance = run.json("references_provenance.json");
    assert_eq!(provenance["count"], 6, "the second round found nothing new");
    let stages: Vec<&str> = provenance["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["stage"].as_str().unwrap())
        .collect();
    assert!(
        stages.contains(&"plan") && stages.contains(&"postdraft"),
        "{stages:?}"
    );
}

#[tokio::test]
async fn the_calls_carry_the_grants_and_limits_of_the_role_book() {
    let harness = Harness::new();
    let run = harness.run(harness.scenario()).await;
    let book = roles::load(&crate_path("assets/roles"), &harness.cfg).expect("role book");
    assert!(!run.calls.is_empty());
    for call in &run.calls {
        let key = call["role"].as_str().expect("role");
        let role = &book.roles[key];
        assert_eq!(call["limits"]["max_turns"], role.max_turns, "{key}");
        assert_eq!(call["limits"]["max_micros"], role.max_micros(), "{key}");
        assert_eq!(call["grants"]["edit"], !role.read_only, "{key}");
        assert_eq!(call["grants"]["commands"], json!(role.shell), "{key}");
    }
    let auditor = run
        .calls
        .iter()
        .find(|c| c["role"] == "evidence_auditor")
        .expect("auditor call");
    assert_eq!(
        auditor["grants"]["edit"], false,
        "the auditor reads a snapshot"
    );
    let prompt = auditor["system_prompt"].as_str().expect("prompt");
    assert!(!prompt.contains("{{"), "no placeholder is left unreplaced");
}

#[tokio::test]
async fn one_analysis_is_still_ranked() {
    let harness = Harness::new();
    let mut scenario = harness.scenario();
    let ranking = harness.variant(
        "single_ranking",
        &[(
            "posthoc_ranking.json",
            r#"{"ranking_rationale": "It is the only analysis.", "ranked_analyses": [{"analysis_id": "score_forensics", "rank": 1, "relevance_to_thesis": "high", "relevance_justification": "It ties the score to the retained program.", "recommended_usage": "main_results", "key_finding_for_paper": "The score belongs to the program."}]}"#,
        )],
    );
    scenario
        .roles
        .insert("posthoc_ranker".into(), vec![answer(&ranking)]);
    let run = harness
        .run_with(scenario, |root| keep_analyses(root, 1))
        .await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    assert_eq!(run.roles(), FULL_ORDER);
    assert!(run.paper().join("posthoc_ranking.json").exists());
}

#[tokio::test]
async fn the_ranker_is_skipped_when_there_are_no_analyses() {
    let harness = Harness::new();
    let run = harness
        .run_with(harness.scenario(), |root| keep_analyses(root, 0))
        .await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    assert_eq!(run.roles(), without(&FULL_ORDER, "posthoc_ranker"));
    assert!(!run.paper().join("posthoc_ranking.json").exists());
}

/// Cuts the recorded post-hoc manifest down to its first `count` analyses.
fn keep_analyses(root: &std::path::Path, count: usize) {
    let path = root.join("posthoc/latest_manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    manifest["analyses"].as_array_mut().unwrap().truncate(count);
    std::fs::write(path, manifest.to_string()).unwrap();
}

#[tokio::test]
async fn a_review_runs_only_when_planned_and_costs_one_repair() {
    let mut harness = Harness::new();
    harness.set("paper.review_rounds", "1");
    let run = harness.run(harness.scenario()).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    let mut expected = FULL_ORDER.to_vec();
    expected.extend([
        "presentation_reviewer",
        "paper_repairer",
        "evidence_auditor",
    ]);
    assert_eq!(run.roles(), expected);
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["repair_rounds_used"], 1);
    assert_eq!(manifest["review_rounds_done"], 1);
    assert!(run.paper().join("paper_review.json").is_file());
    assert!(run.paper().join("review_feedback.md").is_file());
    assert!(run.paper().join("rounds/01/paper.tex").is_file());
    assert!(run.paper().join("rounds/02/paper.tex").is_file());
}

#[tokio::test]
async fn every_kind_of_repair_draws_on_one_counter() {
    let mut harness = Harness::new();
    harness.set("paper.review_rounds", "1");
    let mut scenario = harness.scenario();
    // The writer leaves no manuscript: a manuscript failure.
    scenario
        .roles
        .insert("paper_writer".into(), vec![reply("I could not finish.")]);
    // The first repair writes a citation that has no entry: a validation failure.
    let broken = harness.paper_variant("broken", |tex| tex.replace("Collins2003", "Missing2020"));
    let good = harness.paper_variant("good", str::to_string);
    scenario.roles.insert(
        "paper_repairer".into(),
        vec![answer(&broken), answer(&good)],
    );
    // The first two audits find a problem; the planned review comes after the
    // first clean audit.
    scenario.roles.insert(
        "evidence_auditor".into(),
        vec![reply(AUDIT_FAIL), reply(AUDIT_FAIL), reply(AUDIT_PASS)],
    );
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    assert_eq!(
        run.roles(),
        [
            "reference_planner",
            "paper_strategist",
            "posthoc_ranker",
            "paper_writer",
            // round 1: no manuscript, so no audit
            "paper_repairer",
            // round 2: an unresolved citation key and an audit finding
            "evidence_auditor",
            "paper_repairer",
            // round 3: an audit finding alone
            "evidence_auditor",
            "paper_repairer",
            // round 4: clean; the planned review asks for a repair
            "evidence_auditor",
            "presentation_reviewer",
            "paper_repairer",
            // round 5: clean
            "evidence_auditor",
        ]
    );
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["repair_rounds_used"], 4);
    assert_eq!(counters(&run)["used"]["paper.repair_round"], 4);
    // The feedback of round 2 names both causes.
    let feedback =
        std::fs::read_to_string(run.paper().join("rounds/02/repair_feedback.md")).unwrap();
    assert!(feedback.contains("Missing2020"), "{feedback}");
    assert!(feedback.contains("Soften the sentence"), "{feedback}");
    for separate in [
        "paper.refs_plan_round",
        "paper.strategy_round",
        "paper.rank_round",
    ] {
        assert_eq!(
            counters(&run)["used"][separate],
            0,
            "{separate} is separate"
        );
    }
}

#[tokio::test]
async fn the_query_plan_the_strategy_and_the_ranking_each_retry_on_their_own_counter() {
    let harness = Harness::new();
    let mut scenario = harness.scenario();
    for (role, file) in [
        ("reference_planner", "reference_query_plan.json"),
        ("paper_strategist", "paper_strategy.json"),
        ("posthoc_ranker", "posthoc_ranking.json"),
    ] {
        let unreadable = harness.variant(&format!("unreadable_{role}"), &[(file, "not json")]);
        let good = scenario.roles[role][0].clone();
        scenario
            .roles
            .insert(role.into(), vec![answer(&unreadable), good]);
    }
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    assert_eq!(
        run.roles(),
        [
            "reference_planner",
            "reference_planner",
            "paper_strategist",
            "paper_strategist",
            "posthoc_ranker",
            "posthoc_ranker",
            "paper_writer",
            "citation_gap_analyst",
            "reference_planner_postdraft",
            "paper_enricher",
            "evidence_auditor",
        ]
    );
    let used = &counters(&run)["used"];
    assert_eq!(used["paper.refs_plan_round"], 1);
    assert_eq!(used["paper.strategy_round"], 1);
    assert_eq!(used["paper.rank_round"], 1);
    assert_eq!(
        used["paper.repair_round"], 0,
        "the manuscript repair counter is untouched"
    );
    assert_eq!(run.json("paper_manifest.json")["repair_rounds_used"], 0);
}

#[tokio::test]
async fn a_used_up_counter_ends_the_phase_incomplete() {
    let mut harness = Harness::new();
    harness.set("paper.max_repair_rounds", "2");
    let mut scenario = harness.scenario();
    scenario
        .roles
        .insert("evidence_auditor".into(), vec![reply(AUDIT_FAIL)]);
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "failed", "{:?}", run.outcome);
    assert_eq!(run.outcome.outcome, "paper_incomplete");
    let count = |role: &str| run.roles().iter().filter(|r| *r == role).count();
    assert_eq!(count("paper_repairer"), 2, "the cap stops the repairs");
    assert_eq!(count("evidence_auditor"), 3, "one audit per round");
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["status"], "incomplete");
    assert_eq!(manifest["repair_rounds_used"], 2);
    assert_eq!(manifest["audit_state"], "findings");
    assert!(
        manifest["caps_hit"]
            .as_array()
            .unwrap()
            .contains(&json!("paper.repair_round")),
        "{manifest}"
    );
    assert!(manifest["reason"].as_str().unwrap().contains("used up"));
}

#[tokio::test]
async fn an_audit_that_cannot_be_read_does_not_gate_and_is_never_called_passed() {
    let harness = Harness::new();
    let mut scenario = harness.scenario();
    scenario
        .roles
        .insert("evidence_auditor".into(), vec![reply("looks fine to me")]);
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    let audits = run
        .roles()
        .iter()
        .filter(|r| *r == "evidence_auditor")
        .count();
    assert_eq!(audits, 3, "one try and two retries");
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["audit_state"], "audit_degraded");
    assert_eq!(manifest["repair_rounds_used"], 0);
    assert_eq!(
        run.json("paper_evidence_audit.json")["state"],
        "audit_degraded"
    );
    assert_eq!(
        run.json("claims_check.json")["saved_model_audit"]["state"],
        "audit_degraded"
    );
    // The retries told the auditor what was wrong.
    let retry = run
        .calls
        .iter()
        .filter(|c| c["role"] == "evidence_auditor")
        .nth(1)
        .expect("a retry");
    assert!(
        retry["turn_prompt"]
            .as_str()
            .unwrap()
            .contains("broke the output contract")
    );
}

#[tokio::test]
async fn an_operational_audit_failure_ends_the_phase() {
    let harness = Harness::new();
    let mut scenario = harness.scenario();
    scenario.roles.insert(
        "evidence_auditor".into(),
        vec![ScriptEntry {
            fail: Some("internal".into()),
            ..ScriptEntry::default()
        }],
    );
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "failed", "{:?}", run.outcome);
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["status"], "incomplete");
    assert_eq!(manifest["audit_state"], "missing");
    assert!(manifest["reason"].as_str().unwrap().contains("audit"));
}

#[tokio::test]
async fn the_same_validation_failure_twice_with_no_change_ends_the_phase() {
    let mut harness = Harness::new();
    harness.set("paper.two_stage", "false");
    let mut scenario = harness.scenario();
    // The stand-in TeX leaves no call record, so the folder is the same after every build.
    let broken = harness.paper_variant("broken", |tex| {
        tex.replace("Collins2003", "Missing2020")
            .replace("% fake: pages 10", "% fake: pages 10\n% fake: quiet")
    });
    scenario
        .roles
        .insert("paper_writer".into(), vec![answer(&broken)]);
    scenario
        .roles
        .insert("paper_repairer".into(), vec![answer(&broken)]);
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "failed", "{:?}", run.outcome);
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["status"], "incomplete");
    assert_eq!(
        manifest["repair_rounds_used"], 1,
        "one repair, then the repeat is seen"
    );
    assert!(
        manifest["reason"].as_str().unwrap().contains("repeated"),
        "{manifest}"
    );
    let report = run.json("validation_report.json");
    assert!(
        report["hard_errors"][0]
            .as_str()
            .unwrap()
            .contains("Missing2020")
    );
}

#[tokio::test]
async fn a_refused_reservation_fails_the_draft() {
    let mut harness = Harness::new();
    // The cap of the paper phase is 93% of the run limit: the planner and the
    // strategist leave too little for the writer's ceiling of $2.50.
    harness.set("budget.max_usd", "\"1.50\"");
    let mut scenario = harness.scenario();
    for (role, micros) in [
        ("reference_planner", 300_000),
        ("paper_strategist", 500_000),
        ("posthoc_ranker", 250_000),
    ] {
        let entry = &mut scenario.roles.get_mut(role).unwrap()[0];
        entry.usage = usage(micros);
    }
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "failed", "{:?}", run.outcome);
    assert_eq!(
        run.roles(),
        ["reference_planner", "paper_strategist", "posthoc_ranker"]
    );
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["status"], "incomplete");
    assert!(
        manifest["reason"]
            .as_str()
            .unwrap()
            .contains("paper writer")
    );
    assert!(
        manifest["caps_hit"]
            .as_array()
            .unwrap()
            .contains(&json!("budget.paper"))
    );
}

#[tokio::test]
async fn a_refused_reservation_skips_each_optional_step() {
    let mut harness = Harness::new();
    harness.set("budget.max_usd", "\"0.88\"");
    harness.set("paper.review_rounds", "1");
    // Required roles ask for little, so only the optional ones are refused.
    for role in ["paper_writer", "evidence_auditor"] {
        harness.set(&format!("roles.{role}.max_usd"), "\"0.01\"");
    }
    let mut scenario = harness.scenario();
    for (role, micros) in [
        ("reference_planner", 300_000),
        ("paper_strategist", 500_000),
    ] {
        scenario.roles.get_mut(role).unwrap()[0].usage = usage(micros);
    }
    let run = harness.run(scenario).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    assert_eq!(
        run.roles(),
        [
            "reference_planner",
            "paper_strategist",
            // ranking, gap analysis, enrichment and review were refused
            "paper_writer",
            "evidence_auditor",
        ]
    );
    let manifest = run.json("paper_manifest.json");
    assert_eq!(manifest["status"], "complete");
    assert_eq!(manifest["repair_rounds_used"], 0);
    assert!(
        manifest["caps_hit"]
            .as_array()
            .unwrap()
            .contains(&json!("budget.paper"))
    );
}
