//! The paper check on a finished paper folder with one defect at a time.
//!
//! The clean folder is what the paper phase leaves on the scenario; each test
//! copies it, breaks one thing, runs the check with the stand-in TeX programs
//! and reads `validation_report.json`.

use std::path::Path;

use polaris_scientist::phases::paper::checks::{CheckSettings, validate_and_write};
use serde_json::{Value, json};

use crate::harness::{Harness, copy_tree, stand_in_tools};

struct Folder {
    _harness: Harness,
    dir: tempfile::TempDir,
}

impl Folder {
    async fn clean() -> Self {
        let harness = Harness::new();
        let run = harness.run(harness.scenario()).await;
        assert_eq!(run.paper_status(), "completed", "{:?}", run.outcome);
        let dir = tempfile::tempdir().expect("temp dir");
        copy_tree(&run.paper(), dir.path(), &run.root);
        for stale in ["validation_report.json", "paper.pdf"] {
            let _ = std::fs::remove_file(dir.path().join(stale));
        }
        Self {
            _harness: harness,
            dir,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn text(&self, name: &str) -> String {
        std::fs::read_to_string(self.path().join(name)).expect("read file")
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.path().join(name), text).expect("write file");
    }

    fn edit(&self, name: &str, from: &str, to: &str) {
        let text = self.text(name);
        assert!(text.contains(from), "{name} has no {from:?}");
        self.write(name, &text.replace(from, to));
    }

    async fn check(&self) -> Value {
        let settings = CheckSettings {
            venue: "iclr".into(),
            tools: stand_in_tools(),
            compile: true,
        };
        validate_and_write(self.path(), &settings).await;
        serde_json::from_str(&self.text("validation_report.json")).expect("report is JSON")
    }
}

fn errors(report: &Value, section: &str) -> Vec<String> {
    report[section]["hard_errors"]
        .as_array()
        .unwrap_or_else(|| panic!("{section} has no hard_errors"))
        .iter()
        .map(|e| e.as_str().expect("text").to_string())
        .collect()
}

fn all_errors(report: &Value) -> Vec<String> {
    report["hard_errors"]
        .as_array()
        .expect("hard_errors")
        .iter()
        .map(|e| e.as_str().expect("text").to_string())
        .collect()
}

fn mentions(list: &[String], needle: &str) -> bool {
    list.iter().any(|e| e.contains(needle))
}

#[tokio::test]
async fn a_clean_paper_passes_every_check() {
    let folder = Folder::clean().await;
    let report = folder.check().await;
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["hard_errors"], json!([]));
    assert_eq!(report["numbers_printed"], 4, "{report}");
    assert_eq!(report["unbacked_numbers"], json!([]));
    assert_eq!(report["latex"]["ok"], true);
    assert_eq!(report["page_validation"]["body_pages"], 9);
}

#[tokio::test]
async fn a_printed_number_that_a_claim_states_and_the_data_contradicts_is_listed() {
    let folder = Folder::clean().await;
    folder.edit("paper.tex", "scores 2.0,", "scores 2.5,");
    folder.edit("paper.tex", "to 2.0.", "to 2.5.");
    folder.edit("claims_grounding.json", "scores 2.0,", "scores 2.5,");
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    let unbacked = report["unbacked_numbers"].as_array().expect("list");
    assert_eq!(
        unbacked.len(),
        2,
        "the abstract and the results: {unbacked:?}"
    );
    for finding in unbacked {
        assert_eq!(finding["printed"], "2.5");
        assert_eq!(finding["reason"], "no_recorded_value");
        assert_eq!(finding["claims_considered"], json!(["c1"]));
        assert_eq!(finding["nearest"]["value"], 2.0);
        assert_eq!(finding["nearest"]["source_id"], "result.best_correct_score");
        assert_eq!(finding["file"], "paper.tex");
    }
    let errors = all_errors(&report);
    assert!(mentions(&errors, "claim c1 states 2.5"), "{errors:?}");
    assert!(
        mentions(&errors, "paper.tex:"),
        "the place is named: {errors:?}"
    );
}

#[tokio::test]
async fn a_number_no_claim_states_is_an_error_that_names_its_sentence() {
    let folder = Folder::clean().await;
    folder.edit(
        "paper.tex",
        "rises.\n",
        "rises. We ran 37 trials and kept 4.25 as a margin.\n",
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], false, "{report}");
    let unbacked = report["unbacked_numbers"].as_array().expect("list");
    let printed: Vec<&str> = unbacked
        .iter()
        .map(|f| f["printed"].as_str().expect("text"))
        .collect();
    assert_eq!(printed, ["37", "4.25"]);
    assert!(unbacked.iter().all(|f| f["reason"] == "not_stated"));
    let errors = all_errors(&report);
    assert_eq!(
        errors.len(),
        2,
        "one message for the sentence and the advice: {errors:?}"
    );
    assert!(errors[0].contains("prints 37 and 4.25"), "{}", errors[0]);
    assert!(errors[0].contains("We ran 37 trials"), "{}", errors[0]);
    assert!(
        errors[1].starts_with("To fix an unbacked number"),
        "{}",
        errors[1]
    );
}

#[tokio::test]
async fn a_command_that_loads_a_file_the_check_does_not_read_is_an_error() {
    let folder = Folder::clean().await;
    folder.edit(
        "paper.tex",
        "rises.\n",
        "rises.\n\\lstinputlisting{run.log}\n",
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], false, "{report}");
    let errors = all_errors(&report);
    assert!(
        mentions(&errors, "\\lstinputlisting"),
        "the command is named: {errors:?}"
    );
}

#[tokio::test]
async fn a_number_printed_with_fewer_digits_still_matches_after_rounding() {
    let folder = Folder::clean().await;
    folder.edit("claims_grounding.json", "scores 2.0,", "scores 2,");
    folder.edit("paper.tex", "scores 2.0,", "scores 2,");
    let report = folder.check().await;
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["unbacked_numbers"], json!([]));
}

#[tokio::test]
async fn an_unknown_claim_id_is_named() {
    let folder = Folder::clean().await;
    let mut claims: Value =
        serde_json::from_str(&folder.text("claims_grounding.json")).expect("claims");
    claims["claims"][0]["source_ids"][0] = json!("result.nowhere");
    folder.write("claims_grounding.json", &claims.to_string());
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    assert!(
        mentions(
            &errors(&report, "claims_validation"),
            "claims[0].source_ids has unknown id `result.nowhere`"
        ),
        "{report}"
    );
}

#[tokio::test]
async fn an_unresolved_citation_key_is_named() {
    let folder = Folder::clean().await;
    folder.edit("paper.tex", "Collins2003", "Nobody1999");
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    assert!(mentions(
        &all_errors(&report),
        "cites keys that references.bib does not define: `Nobody1999`"
    ));
}

#[tokio::test]
async fn a_bibliography_that_does_not_match_its_provenance_is_named() {
    let folder = Folder::clean().await;
    let mut provenance: Value =
        serde_json::from_str(&folder.text("references_provenance.json")).expect("provenance");
    provenance["references"].as_array_mut().expect("rows").pop();
    folder.write("references_provenance.json", &provenance.to_string());
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    assert!(mentions(
        &errors(&report, "reference_validation"),
        "keys missing from references_provenance.json"
    ));
}

/// A PNG header that claims `width` by `height` pixels.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend_from_slice(&width.to_be_bytes());
    png.extend_from_slice(&height.to_be_bytes());
    png.extend_from_slice(&[8, 2, 0, 0, 0]);
    png
}

#[tokio::test]
async fn a_paper_with_a_figure_that_matches_its_manifest_is_valid() {
    let folder = Folder::clean().await;
    std::fs::create_dir_all(folder.path().join("figures")).expect("figures folder");
    std::fs::write(
        folder.path().join("figures/scores.png"),
        png_header(2400, 800),
    )
    .expect("png");
    std::fs::write(folder.path().join("figures/scores.pdf"), b"%PDF-1.5 figure").expect("pdf");
    folder.write(
        "figure_manifest.json",
        &json!({"figures": [{
            "figure_id": "scores", "figure_type": "bar",
            "path_png": "figures/scores.png", "path_pdf": "figures/scores.pdf",
            "n_panels": 1, "panels_per_row": 1,
            "categories_per_panel": [2], "series_per_panel": [1],
            "min_font_size_pt": 8,
            "source_ids": ["result.best_correct_score"],
            "placement": "full_width"
        }]})
        .to_string(),
    );
    folder.edit(
        "paper.tex",
        "\\bibliographystyle",
        "\\begin{figure}[t]\n\\includegraphics[width=\\linewidth]{figures/scores.pdf}\n\\caption{Scores.}\n\\end{figure}\n\\bibliographystyle",
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["figure_validation"]["figure_count"], 1);
    assert_eq!(report["figure_validation"]["soft_warnings"], json!([]));
}

#[tokio::test]
async fn a_figure_file_that_is_missing_is_named() {
    let folder = Folder::clean().await;
    folder.write(
        "figure_manifest.json",
        &json!({"figures": [{
            "figure_id": "scores", "figure_type": "bar",
            "path_png": "figures/scores.png", "path_pdf": "figures/scores.pdf",
            "n_panels": 1, "panels_per_row": 1,
            "categories_per_panel": [2], "series_per_panel": [1]
        }]})
        .to_string(),
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    assert!(
        mentions(
            &errors(&report, "figure_validation"),
            "missing PNG artifact: figures/scores.png"
        ),
        "{report}"
    );
}

#[tokio::test]
async fn a_figure_file_that_does_not_match_its_metadata_is_named() {
    let folder = Folder::clean().await;
    // A PNG header far smaller than the venue asks for, and a "PDF" that is not one.
    std::fs::create_dir_all(folder.path().join("figures")).expect("figures folder");
    std::fs::write(folder.path().join("figures/scores.png"), png_header(40, 30)).expect("png");
    std::fs::write(folder.path().join("figures/scores.pdf"), b"not a pdf").expect("pdf");
    folder.write(
        "figure_manifest.json",
        &json!({"figures": [{
            "figure_id": "scores", "figure_type": "bar",
            "path_png": "figures/scores.png", "path_pdf": "figures/scores.pdf",
            "n_panels": 1, "panels_per_row": 2,
            "categories_per_panel": [2], "series_per_panel": [1],
            "source_ids": ["result.best_correct_score"],
            "placement": "full_width"
        }]})
        .to_string(),
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    let figure = errors(&report, "figure_validation");
    assert!(
        mentions(
            &figure,
            "figures[0] missing PDF artifact: figures/scores.pdf"
        ),
        "{figure:?}"
    );
    // Layout and size problems are warnings, not errors.
    assert!(
        !mentions(&figure, "panels_per_row") && !mentions(&figure, "PNG width"),
        "{figure:?}"
    );
    let soft: Vec<String> = report["figure_validation"]["soft_warnings"]
        .as_array()
        .expect("soft warnings")
        .iter()
        .map(|w| w.as_str().expect("text").to_string())
        .collect();
    assert!(mentions(&soft, "PNG width too small"), "{soft:?}");
    assert!(
        mentions(&soft, "figures[0] panels_per_row exceeds n_panels"),
        "{soft:?}"
    );
}

#[tokio::test]
async fn a_tex_dependency_outside_the_paper_folder_is_named() {
    let folder = Folder::clean().await;
    folder.edit(
        "paper.tex",
        "\\bibliographystyle",
        "\\input{../outside/extra}\n\\bibliographystyle",
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    assert!(
        mentions(
            &errors(&report, "dependency_validation"),
            "leaves the paper directory"
        ),
        "{report}"
    );
    assert_eq!(report["latex"]["ran"], false, "{report}");
    assert!(!folder.path().join("paper.pdf").exists());
}

#[tokio::test]
async fn a_figure_named_by_a_built_path_is_not_compiled() {
    let folder = Folder::clean().await;
    folder.edit(
        "paper.tex",
        "\\bibliographystyle",
        "\\includegraphics{figures/\\jobname}\n\\bibliographystyle",
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    assert!(
        mentions(&errors(&report, "figure_validation"), "by a built path"),
        "{report}"
    );
    assert_eq!(report["latex"]["ran"], false, "{report}");
    assert!(!folder.path().join("paper.pdf").exists());
}

#[tokio::test]
async fn a_missing_paper_is_not_compiled() {
    let folder = Folder::clean().await;
    std::fs::remove_file(folder.path().join("paper.tex")).expect("remove");
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    assert_eq!(report["latex"]["ran"], false, "{report}");
}

#[tokio::test]
async fn a_number_error_alone_still_builds_the_pdf_and_the_paper_is_not_ok() {
    let folder = Folder::clean().await;
    folder.edit("paper.tex", "scores 2.0,", "scores 2.5,");
    folder.edit("paper.tex", "to 2.0.", "to 2.5.");
    folder.edit("claims_grounding.json", "scores 2.0,", "scores 2.5,");
    let report = folder.check().await;
    assert_eq!(report["ok"], false, "{report}");
    assert!(
        mentions(&all_errors(&report), "claim c1 states 2.5"),
        "{report}"
    );
    assert_eq!(report["latex"]["ran"], true, "{report}");
    assert_eq!(report["latex"]["ok"], true, "{report}");
    assert!(report["pdf_path"].is_string(), "{report}");
    assert!(folder.path().join("paper.pdf").exists());
}

#[tokio::test]
async fn a_missing_citation_alone_still_builds_the_pdf() {
    let folder = Folder::clean().await;
    folder.edit(
        "paper.tex",
        "\\bibliographystyle",
        "\\cite{nobody2020}\n\\bibliographystyle",
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], false, "{report}");
    assert!(mentions(&all_errors(&report), "nobody2020"), "{report}");
    assert_eq!(report["latex"]["ran"], true, "{report}");
    assert!(folder.path().join("paper.pdf").exists());
}

#[tokio::test]
async fn a_body_longer_than_the_page_budget_is_named() {
    let folder = Folder::clean().await;
    folder.edit("paper.tex", "% fake: pages 10", "% fake: pages 14");
    folder.edit(
        "paper.tex",
        "% fake: refs 9 420 550",
        "% fake: refs 13 420 550",
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], false);
    let page = errors(&report, "page_validation");
    assert!(
        page.iter().any(|e| e.contains("body") && e.contains('9')),
        "{page:?}"
    );
    assert_eq!(report["page_validation"]["max_body_pages"], 9);
}

#[tokio::test]
async fn a_paper_with_no_concept_figure_and_a_manifest_that_says_so_is_valid() {
    let folder = Folder::clean().await;
    folder.write(
        "figure_manifest.json",
        &json!({
            "figures": [],
            "concept_figure": {"status": "none", "reason": "no overview diagram was drawn"}
        })
        .to_string(),
    );
    let report = folder.check().await;
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(errors(&report, "figure_validation"), Vec::<String>::new());
}
