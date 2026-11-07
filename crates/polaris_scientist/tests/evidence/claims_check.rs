//! The offline claims check on hand-written paper folders and on the paper of
//! a saved circle packing campaign.
//!
//! The samples keep the shapes of the files a paper folder can hold when
//! another system wrote it: claims without ids, `grounding_sources` keyed by
//! id, and an audit record with `verdict`, `finding_count` and `findings`. A
//! check that needs more than those files would not run on a paper this graph
//! did not write.

use std::path::Path;

use polaris_scientist::claims::{self, AuditState, ClaimsError, Reason};
use serde_json::{Value, json};

use crate::harness::Harness;

struct Sample {
    dir: tempfile::TempDir,
}

impl Sample {
    /// A folder with two claims, three sources, a bibliography and no audit.
    fn new(body: &str) -> Self {
        let sample = Self {
            dir: tempfile::tempdir().expect("temp dir"),
        };
        sample.json(
            "claims_grounding.json",
            &json!({"claims": [
                {
                    "claim": "Seed ring construction scores 0.9598",
                    "source_ids": ["ablation.variant_baseline_reference.score.mean"],
                    "citation_keys": []
                },
                {
                    "claim": "The method follows Smith (2020) and reaches 2.6311",
                    "source_ids": ["ablation.variant_control_best.score.mean"],
                    "citation_keys": ["smith2020"]
                },
                {
                    "claim": "A claim whose source was never recorded",
                    "source_ids": ["ablation.nowhere"],
                    "citation_keys": []
                }
            ]}),
        );
        sample.json(
            "paper_data.json",
            &json!({"grounding_sources": {
                "ablation.variant_baseline_reference.score.mean": {"value": 0.959_764},
                "ablation.variant_control_best.score.mean": {"value": 2.631_094},
                "ablation.unused": {"value": 7.0}
            }}),
        );
        sample.text("paper.tex", &document(body));
        sample.text(
            "references.bib",
            "@article{smith2020,\n  title={A title},\n  year={2020}\n}\n",
        );
        sample
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn text(&self, name: &str, body: &str) {
        let target = self.path().join(name);
        std::fs::create_dir_all(target.parent().expect("parent")).expect("folder");
        std::fs::write(target, body).expect("write file");
    }

    fn json(&self, name: &str, value: &Value) {
        self.text(name, &serde_json::to_string_pretty(value).expect("json"));
    }

    fn audit(&self, record: &Value) {
        self.json("paper_evidence_audit.json", record);
    }
}

fn document(body: &str) -> String {
    format!("\\documentclass{{article}}\n\\begin{{document}}\n{body}\n\\end{{document}}\n")
}

const GOOD_TEX: &str = "The seed ring construction scores 0.9598. We follow \\citet{smith2020} and the method reaches 2.6311.\n";

#[test]
fn counts_and_the_unbacked_list_follow_the_claims() {
    let sample = Sample::new(GOOD_TEX);
    sample.audit(&json!({
        "finding_count": 0, "findings": [], "summary": "ok", "verdict": "pass"
    }));
    let check = claims::check_paper(sample.path()).expect("check");
    let mechanical = &check.mechanical_check;
    assert_eq!(mechanical.claims_listed, 3);
    assert_eq!(
        mechanical.claims_resolved, 2,
        "the third source does not exist"
    );
    assert_eq!(mechanical.numbers_printed, 2);
    assert_eq!(mechanical.numbers_backed, 2);
    assert!(mechanical.unbacked.is_empty());
    assert!(
        !mechanical.passed,
        "every number is backed, but one declared claim does not resolve"
    );
    assert_eq!(mechanical.unresolved_claims.len(), 1);
    let unresolved = &mechanical.unresolved_claims[0];
    assert_eq!(unresolved.id, "claims[2]");
    assert_eq!(unresolved.claim, "A claim whose source was never recorded");
    assert_eq!(unresolved.unknown_source_ids, ["ablation.nowhere"]);
    assert_eq!(check.saved_model_audit.state, AuditState::Passed);
    assert_eq!(check.checker_version, claims::CHECKER_VERSION);
}

#[test]
fn the_verdict_passes_once_every_declared_claim_resolves() {
    let sample = Sample::new(GOOD_TEX);
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [
            {
                "claim": "Seed ring construction scores 0.9598",
                "source_ids": ["ablation.variant_baseline_reference.score.mean"],
                "citation_keys": []
            },
            {
                "claim": "The method follows Smith (2020) and reaches 2.6311",
                "source_ids": ["ablation.variant_control_best.score.mean"],
                "citation_keys": ["smith2020"]
            }
        ]}),
    );
    let mechanical = claims::check_paper(sample.path())
        .expect("check")
        .mechanical_check;
    assert_eq!(
        (mechanical.claims_listed, mechanical.claims_resolved),
        (2, 2)
    );
    assert!(mechanical.unresolved_claims.is_empty());
    assert!(mechanical.passed);
}

#[test]
fn the_report_says_which_part_is_fresh_and_which_is_the_saved_audit() {
    let sample = Sample::new(GOOD_TEX);
    sample.audit(&json!({"verdict": "pass", "finding_count": 0, "findings": []}));
    let check = claims::check_paper(sample.path()).expect("check");
    assert!(check.mechanical_check.what.contains("Computed by this run"));
    assert!(
        check
            .saved_model_audit
            .what
            .contains("did not run the evidence audit"),
        "{}",
        check.saved_model_audit.what
    );
    assert_eq!(check.saved_model_audit.file, "paper_evidence_audit.json");
    // A passing saved audit does not make a number the check cannot back pass.
    sample.text(
        "paper.tex",
        &document("The ring construction scores 0.9611 here."),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(check.saved_model_audit.state, AuditState::Passed);
    assert_eq!(check.mechanical_check.numbers_unbacked, 1);
    assert!(!check.mechanical_check.passed);
}

#[test]
fn a_claim_may_cite_a_reference_by_the_id_in_the_provenance() {
    let sample = Sample::new(GOOD_TEX);
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [
            {
                "claim": "Related work treats packing problems",
                "source_ids": ["ref.openalex.W1", "ref.s2.abc"],
                "citation_keys": ["smith2020"]
            },
            {
                "claim": "Related work that was never retrieved",
                "source_ids": ["ref.openalex.W9"],
                "citation_keys": []
            }
        ]}),
    );
    // Without the provenance file neither claim resolves.
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(check.mechanical_check.claims_resolved, 0);

    sample.json(
        "references_provenance.json",
        &json!({"count": 2, "references": [
            {"id": "ref.openalex.W1", "citation_key": "smith2020"},
            {"id": "ref.s2.abc", "citation_key": "other2021"}
        ]}),
    );
    sample.text(
        "paper.tex",
        &document("Related work treats packing problems."),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(check.mechanical_check.claims_listed, 2);
    assert_eq!(
        check.mechanical_check.claims_resolved, 1,
        "only the retrieved reference"
    );
    assert_eq!(
        check.mechanical_check.numbers_printed, 0,
        "a reference holds no number"
    );
}

#[test]
fn a_provenance_file_that_is_not_json_is_an_error_that_names_it() {
    let sample = Sample::new(GOOD_TEX);
    sample.text("references_provenance.json", "{ not json");
    let error = claims::check_paper(sample.path()).expect_err("must fail");
    assert!(matches!(error, ClaimsError::Malformed { .. }), "{error:?}");
    assert!(
        error.to_string().contains("references_provenance.json"),
        "{error}"
    );
}

#[test]
fn a_printed_number_that_differs_from_its_source_is_listed_with_the_nearest_value() {
    let sample = Sample::new("The seed ring construction scores 0.9611.\n");
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [
            {
                "claim": "Seed ring construction scores 0.9611",
                "source_ids": ["ablation.variant_baseline_reference.score.mean"],
                "citation_keys": []
            }
        ]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    let mechanical = &check.mechanical_check;
    assert_eq!(
        (mechanical.numbers_printed, mechanical.numbers_backed),
        (1, 0)
    );
    let finding = &mechanical.unbacked[0];
    assert_eq!(finding.printed, "0.9611");
    assert_eq!(finding.reason, Some(Reason::NoRecordedValue));
    assert_eq!(
        finding.claims_considered,
        ["claims[0]"],
        "claims without ids are indexed"
    );
    let nearest = finding.nearest.as_ref().expect("nearest value");
    assert_eq!(nearest.value, 0.959_764);
    assert_eq!(
        nearest.source_id,
        "ablation.variant_baseline_reference.score.mean"
    );
    assert_eq!((finding.file.as_str(), finding.line), ("paper.tex", 3));
}

#[test]
fn a_number_in_the_prose_that_no_claim_states_is_not_backed() {
    let sample = Sample::new(
        "The seed ring construction scores 0.9598 after 57 restarts of the ring construction.\n",
    );
    let check = claims::check_paper(sample.path()).expect("check");
    let mechanical = &check.mechanical_check;
    assert_eq!(
        (mechanical.numbers_printed, mechanical.numbers_backed),
        (2, 1)
    );
    assert_eq!(mechanical.unbacked[0].printed, "57");
    assert_eq!(mechanical.unbacked[0].reason, Some(Reason::NotStated));
}

#[test]
fn deleting_a_number_from_a_claim_does_not_hide_it_in_the_text() {
    let sample = Sample::new("The ring construction reaches 0.9598 and makes 21 wall contacts.\n");
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{
            "id": "c1",
            "claim": "The ring construction reaches 0.9598 and makes 21 wall contacts",
            "source_ids": ["m.score", "m.walls"],
            "citation_keys": []
        }]}),
    );
    sample.json(
        "paper_data.json",
        &json!({"grounding_sources": {"m.score": {"value": 0.959_764}, "m.walls": {"value": 21}}}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(check.mechanical_check.numbers_unbacked, 0);

    // A repair that drops the wall count from the claim, and leaves the text.
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{
            "id": "c1",
            "claim": "The ring construction reaches 0.9598 and makes wall contacts",
            "source_ids": ["m.score", "m.walls"],
            "citation_keys": []
        }]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    let unbacked = &check.mechanical_check.unbacked;
    assert_eq!(unbacked.len(), 1, "{unbacked:?}");
    assert_eq!(unbacked[0].printed, "21");
    assert_eq!(unbacked[0].reason, Some(Reason::NotStated));

    // Removing it from the text as well puts text and registry in step again.
    sample.text(
        "paper.tex",
        &document("The ring construction reaches 0.9598 and makes wall contacts.\n"),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(check.mechanical_check.numbers_unbacked, 0);
}

#[test]
fn a_claim_cannot_cover_a_number_printed_in_text_it_is_not_about() {
    let sample = Sample::new("The wall clock time of the run was 0.9598 seconds overall.\n");
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(
        check.mechanical_check.unbacked[0].reason,
        Some(Reason::ClaimIsAboutOtherText)
    );
}

#[test]
fn signs_count() {
    let sample = Sample::new("The ablation score difference to the control is $-0.1991$.\n");
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{
            "claim": "The ablation score difference to the control is -0.1991",
            "source_ids": ["ablation.delta"],
            "citation_keys": []
        }]}),
    );
    sample.json(
        "paper_data.json",
        &json!({"grounding_sources": {"ablation.delta": {"value": -0.1991}}}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(check.mechanical_check.numbers_unbacked, 0);

    sample.text(
        "paper.tex",
        &document("The ablation score difference to the control is 0.1991.\n"),
    );
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{
            "claim": "The ablation score difference to the control is 0.1991",
            "source_ids": ["ablation.delta"],
            "citation_keys": []
        }]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(
        check.mechanical_check.numbers_unbacked, 1,
        "the source is negative"
    );
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{
            "claim": "The ablation score difference to the control is 0.1991",
            "source_ids": ["ablation.delta"],
            "derivations": [{"expression": "abs(d)", "inputs": {"d": "ablation.delta"}}],
            "citation_keys": []
        }]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(
        check.mechanical_check.numbers_unbacked, 0,
        "a recorded derivation"
    );
    let backing = check.mechanical_check.backed[0]
        .backed_by
        .as_ref()
        .expect("backed");
    assert_eq!(backing.derivation.as_deref(), Some("abs(d)"));
}

#[test]
fn numbers_in_imported_tables_and_macros_are_checked() {
    let sample = Sample::new(
        "\\begin{table}\n\\caption{Scores of the ring construction and the control.}\n\\input{tables/scores.tex}\n\\end{table}\nThe best score is \\best.\n",
    );
    sample.text(
        "tables/scores.tex",
        "\\begin{tabular}{lr}\nVariant & Score \\\\\nring construction & 0.9598 \\\\\ncontrol & 2.6311 \\\\\ninvented & 3.1415 \\\\\n\\end{tabular}\n",
    );
    sample.text(
        "paper.tex",
        "\\documentclass{article}\n\\newcommand{\\best}{2.6311}\n\\newcommand{\\secret}{4.2}\n\\begin{document}\n\\begin{table}\n\\caption{Scores of the ring construction and the control.}\n\\input{tables/scores.tex}\n\\end{table}\nThe best score is \\best.\n\\end{document}\n",
    );
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{
            "claim": "The best score is 2.6311; the table lists the scores of the ring construction and the control",
            "source_ids": ["ablation.variant_baseline_reference.score.mean", "ablation.variant_control_best.score.mean"],
            "citation_keys": []
        }]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    let mechanical = &check.mechanical_check;
    assert_eq!(mechanical.files_read, ["paper.tex", "tables/scores.tex"]);
    let unbacked: Vec<(&str, &str, usize)> = mechanical
        .unbacked
        .iter()
        .map(|f| (f.printed.as_str(), f.file.as_str(), f.line))
        .collect();
    assert_eq!(
        unbacked,
        [("4.2", "paper.tex", 3), ("3.1415", "tables/scores.tex", 5)],
        "the invented table cell and the unused macro are found"
    );
}

#[test]
fn a_derivation_must_recompute_to_the_printed_number() {
    let sample = Sample::new("The control reaches 2.74 times the ring construction score.\n");
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{
            "claim": "The control reaches 2.74 times the ring construction score",
            "source_ids": ["ablation.variant_baseline_reference.score.mean", "ablation.variant_control_best.score.mean"],
            "derivations": [{
                "expression": "a / b",
                "inputs": {"a": "ablation.variant_control_best.score.mean",
                           "b": "ablation.variant_baseline_reference.score.mean"}
            }],
            "citation_keys": []
        }]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(
        check.mechanical_check.numbers_unbacked, 0,
        "2.631094 / 0.959764 is 2.74"
    );

    sample.text(
        "paper.tex",
        &document("The control reaches 3.74 times the ring construction score.\n"),
    );
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{
            "claim": "The control reaches 3.74 times the ring construction score",
            "source_ids": ["ablation.variant_baseline_reference.score.mean", "ablation.variant_control_best.score.mean"],
            "derivations": [{
                "expression": "a / b",
                "inputs": {"a": "ablation.variant_control_best.score.mean",
                           "b": "ablation.variant_baseline_reference.score.mean"}
            }],
            "citation_keys": []
        }]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(check.mechanical_check.numbers_unbacked, 1);
    assert_eq!(
        check.mechanical_check.unbacked[0].reason,
        Some(Reason::NoRecordedValue)
    );
}

/// Where a manuscript can print a number: each case is one place, written with
/// the number 57 (or the given value), and the check must find it.
#[test]
fn a_number_printed_in_any_part_of_the_manuscript_is_found() {
    let cases: [(&str, &str, &[&str]); 18] = [
        ("body text", "We ran 57 restarts.", &["57"]),
        ("footnote", "Text.\\footnote{Here 57 appear.}", &["57"]),
        ("bold text", "A \\textbf{57} here.", &["57"]),
        ("inline math", "We set $k=57$ here.", &["57"]),
        ("display math", "\\[ y = 3.5 + 57 \\]", &["3.5", "57"]),
        (
            "verbatim",
            "\\begin{verbatim}\nbudget = 57\n\\end{verbatim}",
            &["57"],
        ),
        (
            "caption",
            "\\begin{figure}\n\\caption{Runs: 57.}\n\\end{figure}",
            &["57"],
        ),
        (
            "a macro defined in the body",
            "\\newcommand{\\runs}{57}Done.",
            &["57"],
        ),
        ("section title", "\\section{The 57 runs}\nText.", &["57"]),
        (
            "link text",
            "See \\href{https://x.org/57}{57 results}.",
            &["57"],
        ),
        (
            "before a comment",
            "Ran 57 % and 99 are in the comment",
            &["57"],
        ),
        ("percent", "About 57\\% of runs.", &["57"]),
        ("thousands", "About 1{,}250 runs.", &["1250"]),
        ("exponent", "Roughly $5.7\\times 10^{2}$ runs.", &["5.7e2"]),
        (
            "fraction",
            "Share $\\frac{57}{100}$ of runs.",
            &["57", "100"],
        ),
        (
            "a list item",
            "\\begin{itemize}\n\\item 57 runs\n\\end{itemize}",
            &["57"],
        ),
        (
            "a table cell outside a float",
            "\\begin{tabular}{lr}\nName & Count \\\\\nruns & 57 \\\\\n\\end{tabular}",
            &["57"],
        ),
        (
            "inside a drawing's node text",
            "\\begin{tikzpicture}\\draw (0,0) -- (1,1) node {57};\\end{tikzpicture}",
            &["57"],
        ),
    ];
    for (name, body, expected) in cases {
        let sample = Sample::new(body);
        sample.json(
            "claims_grounding.json",
            &json!({"claims": [{"claim": "nothing numeric", "source_ids": ["m.a"], "citation_keys": []}]}),
        );
        sample.json(
            "paper_data.json",
            &json!({"grounding_sources": {"m.a": {"value": 12345.0}}}),
        );
        let check = claims::check_paper(sample.path()).expect("check");
        let found: Vec<&str> = check
            .mechanical_check
            .unbacked
            .iter()
            .map(|f| f.printed.as_str())
            .collect();
        assert_eq!(found, expected, "{name}");
        assert!(!check.mechanical_check.passed, "{name}");
    }
}

/// What is not printed as a result: each case prints numbers that are labels,
/// layout, years, formula structure or text the compiler never typesets.
#[test]
fn numbers_that_are_not_printed_results_are_not_asked_for() {
    let body = "\\iffalse 57 \\fi\n% 58 in a comment\nSee Table~3, Figure 2, Section 4 and Eq.~(5) in \\citet{a2020} and \\cite{b2021} (2024).\n\\includegraphics[width=0.8\\textwidth]{figures/a57.pdf}\n\\vspace{3mm}\nLet $x_{12} \\ge 0$, $y^2 \\le 1$ and $z \\in [0,1]$ with $d_1 = 2 e$.\n\\begin{tabular}{lr@{\\hspace{14pt}}r}\n$i$ & Radius & Count \\\\\n0 & 0.5 & 7 \\\\\n\\end{tabular}\n\\begin{enumerate}\n\\item[1.] First.\n\\end{enumerate}\n";
    let sample = Sample::new(body);
    sample.json(
        "claims_grounding.json",
        &json!({"claims": [{"claim": "nothing numeric", "source_ids": ["m.a"], "citation_keys": []}]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    let found: Vec<&str> = check
        .mechanical_check
        .unbacked
        .iter()
        .map(|f| f.printed.as_str())
        .collect();
    assert_eq!(
        found,
        ["0.5", "7"],
        "only the result cells of the table are asked for"
    );
}

#[test]
fn the_audit_record_maps_to_one_of_four_states() {
    let cases = [
        (
            "an older record that passed",
            json!({"verdict": "pass", "finding_count": 0, "findings": []}),
            AuditState::Passed,
        ),
        (
            "an older record with findings",
            json!({"verdict": "fail", "finding_count": 3, "findings": [{}, {}, {}]}),
            AuditState::Findings,
        ),
        (
            "a record that states it passed",
            json!({"state": "passed", "finding_count": 0}),
            AuditState::Passed,
        ),
        (
            "a record that states findings",
            json!({"state": "findings", "finding_count": 1}),
            AuditState::Findings,
        ),
        (
            "a record that states the audit was degraded",
            json!({"state": "audit_degraded", "reason": "no verdict line"}),
            AuditState::AuditDegraded,
        ),
        (
            "a verdict without a count of findings",
            json!({"verdict": "pass"}),
            AuditState::AuditDegraded,
        ),
        (
            "a record with no verdict at all",
            json!({"summary": "x"}),
            AuditState::AuditDegraded,
        ),
    ];
    for (name, record, expected) in cases {
        let sample = Sample::new(GOOD_TEX);
        sample.audit(&record);
        let check = claims::check_paper(sample.path()).expect("check");
        assert_eq!(check.saved_model_audit.state, expected, "{name}: {record}");
    }
}

#[test]
fn a_damaged_audit_record_is_degraded_and_no_record_is_missing() {
    let sample = Sample::new(GOOD_TEX);
    assert_eq!(
        claims::check_paper(sample.path())
            .expect("check")
            .saved_model_audit
            .state,
        AuditState::Missing
    );
    sample.text("paper_evidence_audit.json", "{not json");
    assert_eq!(
        claims::check_paper(sample.path())
            .expect("check")
            .saved_model_audit
            .state,
        AuditState::AuditDegraded
    );
}

#[test]
fn a_second_run_gives_the_same_answer_and_the_same_report_file() {
    let sample = Sample::new(GOOD_TEX);
    sample.audit(&json!({"verdict": "fail", "finding_count": 1, "findings": [{}]}));
    let first = claims::check_paper(sample.path()).expect("first run");
    let second = claims::check_paper(sample.path()).expect("second run");
    assert_eq!(first, second);

    let out = sample.path().join("out");
    claims::write_report(&first, &out.join(claims::REPORT_FILE)).expect("write");
    let one = std::fs::read(out.join(claims::REPORT_FILE)).expect("read");
    claims::write_report(&second, &out.join(claims::REPORT_FILE)).expect("write again");
    let two = std::fs::read(out.join(claims::REPORT_FILE)).expect("read again");
    assert_eq!(one, two, "the report is byte for byte the same");

    let report: Value = serde_json::from_slice(&one).expect("report is JSON");
    let mut keys: Vec<&str> = report
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["checker_version", "mechanical_check", "saved_model_audit"]
    );
    let mut mechanical: Vec<&str> = report["mechanical_check"]
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    mechanical.sort_unstable();
    assert_eq!(
        mechanical,
        [
            "backed",
            "claims_listed",
            "claims_resolved",
            "derivation_problems",
            "files_read",
            "import_problems",
            "numbers_backed",
            "numbers_printed",
            "numbers_unbacked",
            "passed",
            "unbacked",
            "unresolved_claims",
            "what"
        ]
    );
    assert_eq!(report["saved_model_audit"]["state"], "findings");
    assert!(one.ends_with(b"\n"));
}

#[test]
fn grounding_sources_may_be_a_list_of_records_with_ids() {
    let sample = Sample::new(GOOD_TEX);
    sample.json(
        "paper_data.json",
        &json!({"grounding_sources": [
            {"id": "ablation.variant_baseline_reference.score.mean", "value": 0.959_764},
            {"id": "ablation.variant_control_best.score.mean", "value": 2.631_094}
        ]}),
    );
    let check = claims::check_paper(sample.path()).expect("check");
    assert_eq!(check.mechanical_check.claims_resolved, 2);
    assert_eq!(check.mechanical_check.numbers_backed, 2);
}

#[test]
fn a_missing_input_file_is_an_error_that_names_it() {
    for name in ["claims_grounding.json", "paper_data.json", "paper.tex"] {
        let sample = Sample::new(GOOD_TEX);
        std::fs::remove_file(sample.path().join(name)).expect("remove");
        let error = claims::check_paper(sample.path()).expect_err("an error");
        assert!(matches!(error, ClaimsError::Missing { .. }), "{error:?}");
        assert!(error.to_string().contains(name), "{error}");
    }
}

#[test]
fn an_unreadable_claims_file_is_an_error_that_names_it() {
    let sample = Sample::new(GOOD_TEX);
    sample.text("claims_grounding.json", "{\"claims\": 3}");
    let error = claims::check_paper(sample.path()).expect_err("an error");
    assert!(matches!(error, ClaimsError::Malformed { .. }), "{error:?}");
    assert!(
        error.to_string().contains("claims_grounding.json"),
        "{error}"
    );
}

#[test]
fn an_import_that_leaves_the_folder_is_not_read() {
    let sample = Sample::new("\\input{../outside.tex}\nThe ring construction scores 0.9598.\n");
    let check = claims::check_paper(sample.path()).expect("check");
    let mechanical = &check.mechanical_check;
    assert_eq!(
        mechanical.import_problems.len(),
        1,
        "{:?}",
        mechanical.import_problems
    );
    assert_eq!(mechanical.numbers_printed, 1);
}

#[tokio::test]
async fn the_phase_writes_the_report_the_check_gives_for_its_folder() {
    let harness = Harness::new();
    let run = harness.run(harness.scenario()).await;
    assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
    let check = claims::check_paper(&run.paper()).expect("check");
    let written: claims::ClaimsCheck =
        serde_json::from_value(run.json(claims::REPORT_FILE)).expect("report shape");
    assert_eq!(written, check);
    assert_eq!(written.saved_model_audit.state, AuditState::Passed);
}

/// Checks a paper folder named by `CLAIMS_CHECK_PAPER_DIR` and prints the
/// report. Run by hand:
/// `CLAIMS_CHECK_PAPER_DIR=<folder> cargo test --test evidence by_hand -- --ignored --nocapture`.
#[test]
#[ignore = "reads a folder named by CLAIMS_CHECK_PAPER_DIR"]
fn by_hand_check_the_paper_folder_named_in_the_environment() {
    let dir = std::env::var("CLAIMS_CHECK_PAPER_DIR").expect("CLAIMS_CHECK_PAPER_DIR is not set");
    let check = claims::check_paper(Path::new(&dir)).expect("check");
    println!(
        "{}",
        serde_json::to_string_pretty(&check).expect("report is JSON")
    );
}

// ---------------------------------------------------------------------------
// The paper of a saved campaign

/// A copy of the paper folder of a saved circle packing campaign
/// (`tests/fixtures/circpack_run/paper`).
fn saved_paper() -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("folder");
        for entry in std::fs::read_dir(from).expect("read") {
            let entry = entry.expect("entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("type").is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).expect("copy");
            }
        }
    }
    let dir = tempfile::tempdir().expect("dir");
    copy(
        &crate::harness::crate_path("tests/fixtures/circpack_run/paper"),
        dir.path(),
    );
    dir
}

#[test]
fn the_audit_probe_that_changed_57_circle_to_999_circle_now_fails() {
    let dir = saved_paper();
    let before = claims::check_paper(dir.path()).expect("check");
    assert_eq!(before.saved_model_audit.state, AuditState::Passed);

    let tex = std::fs::read_to_string(dir.path().join("paper.tex")).expect("read");
    assert_eq!(
        tex.matches("57 circle").count(),
        4,
        "the probe edits four places"
    );
    std::fs::write(
        dir.path().join("paper.tex"),
        tex.replace("57 circle", "999 circle"),
    )
    .expect("write");
    let after = claims::check_paper(dir.path()).expect("check");

    // The probe's number is printed and nothing backs it.
    let printed_999: Vec<_> = after
        .mechanical_check
        .unbacked
        .iter()
        .filter(|f| f.printed == "999")
        .collect();
    assert_eq!(printed_999.len(), 4, "{printed_999:?}");
    assert!(
        printed_999
            .iter()
            .all(|f| f.reason == Some(Reason::NotStated))
    );
    assert!(printed_999.iter().all(|f| f.excerpt.contains("999")));
    // The saved audit still reads as it was saved: it was not run again.
    assert_eq!(after.saved_model_audit.state, AuditState::Passed);
    assert!(after.mechanical_check.numbers_unbacked > 0);
    assert!(!after.mechanical_check.passed);
}

#[test]
fn the_saved_paper_shows_which_of_its_numbers_nothing_backs() {
    let dir = saved_paper();
    let check = claims::check_paper(dir.path()).expect("check");
    let mechanical = &check.mechanical_check;
    assert_eq!(
        mechanical.files_read,
        [
            "paper.tex",
            "paper_authors.tex",
            "tables/final_packing.tex",
            "tables/tolerance_sweep.tex"
        ]
    );
    assert_eq!(
        mechanical.numbers_printed,
        mechanical.numbers_backed + mechanical.numbers_unbacked
    );
    assert!(
        mechanical.numbers_printed > 300,
        "{}",
        mechanical.numbers_printed
    );

    let backed: std::collections::BTreeSet<&str> = mechanical
        .backed
        .iter()
        .map(|f| f.printed.as_str())
        .collect();
    for number in ["2.6298", "0.9598", "2.4307"] {
        assert!(backed.contains(number), "{number} is a recorded score");
    }
    // The contact counts, the degrees of freedom and the gap the repairs took
    // out of the claim sentences are still printed and nothing backs them.
    let unbacked = |printed: &str, line: usize| {
        mechanical
            .unbacked
            .iter()
            .any(|f| f.printed == printed && f.line == line && f.file == "paper.tex")
    };
    assert!(unbacked("57", 23), "abstract");
    assert!(unbacked("21", 23), "abstract");
    assert!(unbacked("78", 23), "abstract");
    assert!(unbacked("57", 51), "contributions");
    assert!(
        unbacked("0.1991", 203),
        "the drop in the sum when SLSQP is removed"
    );
    assert!(unbacked("57", 215), "the contact analysis section");
    // Table cells of imported files are found with their file and line.
    assert!(
        mechanical
            .unbacked
            .iter()
            .any(|f| f.file == "tables/tolerance_sweep.tex" && f.printed == "57")
    );
    assert!(
        mechanical
            .unbacked
            .iter()
            .any(|f| f.file == "tables/final_packing.tex" && f.printed == "0.0663")
    );
    // The task constant is a recorded source only if the claim cites it.
    assert!(mechanical.unbacked.iter().any(|f| f.printed == "26"));
}
