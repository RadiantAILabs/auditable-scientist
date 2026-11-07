//! What each ablation variant and control did, for the paper roles.
//!
//! The pipeline summary holds a score for every variant, but a score does not
//! say what the variant changed. For each variant and control the summary lists,
//! this module stages three small files under
//! `dossier/ablations/variants/<id>/`:
//!
//! * `variant_spec.json`: the specification the variant was asked to follow;
//! * `source.diff`: the source of the last program evaluated for the variant,
//!   compared with the first program of the control (the unchanged winner),
//!   which every other variant starts from; the seed baseline is compared the
//!   same way, so its diff shows what the winner added to the seed;
//! * `verification.json`: the record the variant verifier wrote for that
//!   program (`results/rubric_status.json`).
//!
//! A diff is staged instead of the program, so the dossier stays small. A part
//! that is not there is not guessed: the record in `paper_data.json`
//! (`ablation_records`) names the file when it exists and says why when it does
//! not, so a role can tell "the variant changed nothing" from "there is no
//! evidence of what it changed".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use walkdir::WalkDir;

use super::{Copier, SKIPPED_DIRS, read_text, safe_component, safe_relative};
use crate::phases::paper::state::DOSSIER_DIR;

/// Folder of the variant records below the dossier.
const RECORDS_DIR: &str = "ablations/variants";

/// Most source files read from one program.
const MAX_SOURCE_FILES: usize = 128;

/// Most source bytes read from one program.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

/// Most bytes of one staged diff; the rest is left out and the record says so.
const MAX_DIFF_BYTES: usize = 64 * 1024;

/// Lines of context around each change.
const DIFF_CONTEXT: usize = 3;

/// The source of one program: its `.py` files and its entrypoint, by path
/// below the code root.
struct Source {
    /// The code root as the program names it (`src`).
    code_root: String,
    /// File text by path below the code root.
    files: BTreeMap<String, String>,
}

/// The comparison of two programs.
struct SourceDiff {
    /// The unified diff, one section for each file that differs.
    text: String,
    /// Files that differ, by path below the code root.
    files_changed: Vec<String>,
    /// Lines the variant added.
    added: usize,
    /// Lines the variant removed.
    removed: usize,
    /// True when `text` stops before the whole diff.
    truncated: bool,
}

/// Stages the records of every variant and control in `summary` and returns one
/// entry for each, in the order of the summary. A control that the summary lists
/// twice appears once.
pub(super) fn stage(root: &Path, copier: &mut Copier<'_>, summary: &Value) -> Vec<Value> {
    let rows: Vec<&Value> = ["variants", "controls"]
        .iter()
        .filter_map(|key| summary.get(key).and_then(Value::as_array))
        .flatten()
        .collect();
    let control = rows
        .iter()
        .filter(|row| row.get("kind").and_then(Value::as_str) == Some("control"))
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .find(|id| safe_component(id));
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        if safe_component(id) && seen.insert(id.to_string()) {
            records.push(record(root, copier, id, row, control));
        }
    }
    records
}

/// Where a staged file lives, relative to the paper folder.
fn staged_path(id: &str, file: &str) -> String {
    format!("{DOSSIER_DIR}/{RECORDS_DIR}/{id}/{file}")
}

/// Where a staged file is copied to, relative to the dossier.
fn dossier_name(id: &str, file: &str) -> String {
    format!("{RECORDS_DIR}/{id}/{file}")
}

/// The program a variant is compared with.
enum Reference {
    /// The variant is the control.
    Itself,
    /// The first program of the control, or the variant's own first program
    /// when the run has no control. `name` heads the old side of the diff.
    Program {
        name: String,
        label: String,
        dir: PathBuf,
    },
    /// There is nothing to compare with.
    Missing,
}

/// The program that the source of `id` is compared with.
fn reference(
    root: &Path,
    copier: &Copier<'_>,
    id: &str,
    index: u32,
    control: Option<&str>,
) -> Reference {
    let variants = root.join("ablations/variants");
    match control {
        Some(control) if control == id => Reference::Itself,
        Some(control) => {
            let dir = variants.join(control).join("task_bundle/program_0");
            if dir.is_dir() && copier.inside_run(&dir) {
                Reference::Program {
                    name: control.to_string(),
                    label: format!("{control}/program_0"),
                    dir,
                }
            } else {
                Reference::Missing
            }
        }
        None if index > 0 => Reference::Program {
            name: "program_0".to_string(),
            label: format!("{id}/program_0"),
            dir: variants.join(id).join("task_bundle/program_0"),
        },
        None => Reference::Missing,
    }
}

fn record(
    root: &Path,
    copier: &mut Copier<'_>,
    id: &str,
    row: &Value,
    control: Option<&str>,
) -> Value {
    let dir = root.join("ablations/variants").join(id);
    let usable = dir.is_dir() && copier.inside_run(&dir);
    let field = |name: &str| row.get(name).cloned().unwrap_or(Value::Null);

    let spec = usable
        && copier.file(
            &dir.join("variant_spec.json"),
            &dossier_name(id, "variant_spec.json"),
        );
    let program = if usable { scored_program(&dir) } else { None };

    let mut compared_with = Value::Null;
    let (diff_file, source_diff) = match program {
        Some(index) => {
            let reference = reference(root, copier, id, index, control);
            if let Reference::Program { label, .. } = &reference {
                compared_with = json!(label);
            }
            stage_diff(copier, &dir, id, index, &reference)
        }
        None => (
            false,
            json!({"changed": null,
                "note": "no evaluated program of this variant was found, so its source cannot be compared"}),
        ),
    };
    let (verification_file, verification) = match program {
        Some(index) => stage_verification(copier, &dir, id, index, row),
        None => (
            false,
            json!({"found": false, "overall_pass": null,
                "reason": "no evaluated program of this variant was found"}),
        ),
    };
    let file = |staged: bool, name: &str| {
        if staged {
            json!(staged_path(id, name))
        } else {
            Value::Null
        }
    };
    json!({
        "id": id,
        "kind": field("kind"),
        "status": field("status"),
        "score": field("score"),
        "scored_program": program.map(|index| format!("program_{index}")),
        "compared_with": compared_with,
        "files": {
            "spec": file(spec, "variant_spec.json"),
            "source_diff": file(diff_file, "source.diff"),
            "verification": file(verification_file, "verification.json"),
        },
        "source_diff": source_diff,
        "verification": verification,
    })
}

/// The last program evaluated for a variant: the highest `program_N` whose
/// `results` folder exists. The summary's score is that program's score. A
/// control has only `program_0`, which the evaluator scored.
fn scored_program(dir: &Path) -> Option<u32> {
    let bundle = dir.join("task_bundle");
    std::fs::read_dir(bundle)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let index = entry
                .file_name()
                .to_str()?
                .strip_prefix("program_")?
                .parse::<u32>()
                .ok()?;
            std::fs::symlink_metadata(entry.path().join("results"))
                .is_ok_and(|meta| meta.is_dir())
                .then_some(index)
        })
        .max()
}

/// Stages `source.diff` for the program `index` and returns whether the file was
/// staged, with the description for the record.
fn stage_diff(
    copier: &mut Copier<'_>,
    dir: &Path,
    id: &str,
    index: u32,
    reference: &Reference,
) -> (bool, Value) {
    let name = dossier_name(id, "source.diff");
    let (old_name, label, old_dir) = match reference {
        Reference::Itself => {
            let text = "This is the control program itself; there is no other source to compare.\n";
            let staged = copier.text(&name, text);
            return (
                staged,
                json!({"changed": false, "files_changed": [], "lines_added": 0, "lines_removed": 0,
                    "truncated": false,
                    "note": "this is the control program; nothing is compared"}),
            );
        }
        Reference::Missing => {
            return (
                false,
                json!({"changed": null,
                    "note": "the control program was not found, so the source cannot be compared"}),
            );
        }
        Reference::Program { name, label, dir } => (name, label, dir),
    };
    let (old, new) = match (
        read_source(old_dir),
        read_source(&dir.join(format!("task_bundle/program_{index}"))),
    ) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(reason), _) | (_, Err(reason)) => {
            return (
                false,
                json!({"changed": null,
                    "note": format!("the source could not be compared: {reason}")}),
            );
        }
    };
    let diff = compare(&old, &new, old_name, id);
    let changed = !diff.files_changed.is_empty();
    let text = if changed {
        diff.text.clone()
    } else {
        format!(
            "The source of program_{index} is identical to {label} ({} source files compared).\n",
            new.files.len()
        )
    };
    let staged = copier.text(&name, &text);
    (
        staged,
        json!({"changed": changed, "files_changed": diff.files_changed,
            "lines_added": diff.added, "lines_removed": diff.removed,
            "truncated": diff.truncated}),
    )
}

/// Stages `verification.json` for the program `index` and returns whether the
/// file was staged, with the description for the record.
fn stage_verification(
    copier: &mut Copier<'_>,
    dir: &Path,
    id: &str,
    index: u32,
    row: &Value,
) -> (bool, Value) {
    let path = dir.join(format!(
        "task_bundle/program_{index}/results/rubric_status.json"
    ));
    let overall_pass = read_text(&path)
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("overall_pass").and_then(Value::as_bool));
    if overall_pass.is_some() && copier.file(&path, &dossier_name(id, "verification.json")) {
        return (true, json!({"found": true, "overall_pass": overall_pass}));
    }
    let unchanged = index == 0 || row.get("kind").and_then(Value::as_str) == Some("control");
    let reason = if unchanged {
        "no verification record exists for a program that was not edited; the evaluator scored it as it was"
    } else {
        "no verification record was found for the last evaluated program"
    };
    (
        false,
        json!({"found": false, "overall_pass": null, "reason": reason}),
    )
}

/// Reads the `.py` files and the entrypoint of the program in `program`.
fn read_source(program: &Path) -> Result<Source, String> {
    let spec: toml::Value = read_text(&program.join("program_spec.toml"))
        .ok_or_else(|| "program_spec.toml is missing".to_string())?
        .parse()
        .map_err(|e: toml::de::Error| format!("program_spec.toml: {e}"))?;
    let code_root = spec
        .get("code_root")
        .and_then(toml::Value::as_str)
        .unwrap_or("src");
    if code_root != "." && !safe_relative(code_root) {
        return Err("the code root leaves the program".to_string());
    }
    let entrypoint = spec
        .get("entrypoint")
        .and_then(|entry| entry.get("path"))
        .and_then(toml::Value::as_str)
        .map(str::to_string);
    let base: PathBuf = program.join(code_root);
    let mut files = BTreeMap::new();
    let mut bytes = 0_u64;
    let walker = WalkDir::new(&base)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            !(entry.file_type().is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| SKIPPED_DIRS.contains(&name)))
        });
    for entry in walker {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(&base) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let wanted = relative.ends_with(".py") || entrypoint.as_deref() == Some(relative.as_str());
        if !wanted {
            continue;
        }
        let Some(text) = read_text(entry.path()) else {
            continue;
        };
        bytes += text.len() as u64;
        if bytes > MAX_SOURCE_BYTES || files.len() >= MAX_SOURCE_FILES {
            return Err("the program source is larger than the dossier compares".to_string());
        }
        files.insert(relative, text);
    }
    Ok(Source {
        code_root: code_root.to_string(),
        files,
    })
}

/// Joins a code root and a path below it, as a label for a diff header.
fn label(side: &str, code_root: &str, relative: &str) -> String {
    if code_root == "." {
        format!("{side}/{relative}")
    } else {
        format!("{side}/{code_root}/{relative}")
    }
}

/// The unified diff of `new` against `old`, file by file. The names head the two
/// sides of each file's diff.
fn compare(old_source: &Source, new_source: &Source, old_name: &str, new_name: &str) -> SourceDiff {
    let paths: BTreeSet<&String> = old_source
        .files
        .keys()
        .chain(new_source.files.keys())
        .collect();
    let mut text = String::new();
    let mut files_changed = Vec::new();
    let (mut added, mut removed) = (0, 0);
    for path in paths {
        let old = old_source.files.get(path);
        let new = new_source.files.get(path);
        if old == new {
            continue;
        }
        let diff = TextDiff::from_lines(
            old.map_or("", String::as_str),
            new.map_or("", String::as_str),
        );
        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Insert => added += 1,
                ChangeTag::Delete => removed += 1,
                ChangeTag::Equal => {}
            }
        }
        let old_label = old.map_or_else(
            || "/dev/null".to_string(),
            |_| label(old_name, &old_source.code_root, path),
        );
        let new_label = new.map_or_else(
            || "/dev/null".to_string(),
            |_| label(new_name, &new_source.code_root, path),
        );
        text.push_str(
            &diff
                .unified_diff()
                .context_radius(DIFF_CONTEXT)
                .header(&old_label, &new_label)
                .to_string(),
        );
        files_changed.push(path.clone());
    }
    let truncated = text.len() > MAX_DIFF_BYTES;
    if truncated {
        let mut cut = MAX_DIFF_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let cut = text[..cut].rfind('\n').map_or(0, |at| at + 1);
        let hidden = text[cut..].lines().count();
        text.truncate(cut);
        text.push_str(&format!(
            "\\ diff cut here: {hidden} more lines are not shown\n"
        ));
    }
    SourceDiff {
        text,
        files_changed,
        added,
        removed,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phases::paper::stage;

    const SPEC: &str = "code_root = \"src\"\n[entrypoint]\npath = \"program.py\"\n";

    const CONTROL_SOURCE: &str = "def search():\n    cen = optimize()\n    out = polish(cen)\n    return out\n\n\ndef polish(c):\n    return c\n";

    fn write(root: &Path, name: &str, text: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn program(root: &Path, id: &str, index: u32, source: &str, evaluated: bool) {
        let base = format!("ablations/variants/{id}/task_bundle/program_{index}");
        write(root, &format!("{base}/program_spec.toml"), SPEC);
        write(root, &format!("{base}/src/program.py"), source);
        if evaluated {
            write(root, &format!("{base}/results/correct.json"), "{}");
        }
    }

    fn verification(root: &Path, id: &str, index: u32, pass: bool) {
        write(
            root,
            &format!(
                "ablations/variants/{id}/task_bundle/program_{index}/results/rubric_status.json"
            ),
            &format!(
                r#"{{"variant_id": "{id}", "overall_pass": {pass}, "rubrics": [{{"id": "variant_intent_preserved", "pass": {pass}, "evidence": "polish is still called"}}]}}"#
            ),
        );
    }

    /// A run folder with a control, a variant that changes one line, a variant
    /// whose source equals the control and a variant with no verification record.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "ablations/pipeline_summary.json",
            r#"{"variants": [
                {"id": "control_best", "kind": "control", "status": "pass", "score": 2.5},
                {"id": "single_start", "kind": "baseline", "status": "pass", "score": 2.4},
                {"id": "same_source", "kind": "ablation", "status": "fail", "score": 2.5},
                {"id": "unverified", "kind": "ablation", "status": "pass", "score": 2.3},
                {"id": "never_ran", "kind": "ablation", "status": "fail", "score": null}],
              "controls": [
                {"id": "control_best", "kind": "control", "status": "pass", "score": 2.5},
                {"id": "seed_baseline", "kind": "baseline", "status": "pass", "score": 0.9}]}"#,
        );
        program(
            root,
            "seed_baseline",
            0,
            &CONTROL_SOURCE.replace("out = polish(cen)", "out = cen"),
            true,
        );
        write(
            root,
            "ablations/variants/seed_baseline/variant_spec.json",
            r#"{"variant_id": "seed_baseline"}"#,
        );
        program(root, "control_best", 0, CONTROL_SOURCE, true);
        write(
            root,
            "ablations/variants/control_best/variant_spec.json",
            r#"{"variant_id": "control_best"}"#,
        );

        let changed = CONTROL_SOURCE.replace("cen = optimize()", "cen = optimize_once()");
        program(root, "single_start", 0, CONTROL_SOURCE, false);
        program(root, "single_start", 1, "broken\n", true);
        program(root, "single_start", 2, &changed, true);
        verification(root, "single_start", 1, false);
        verification(root, "single_start", 2, true);
        write(
            root,
            "ablations/variants/single_start/variant_spec.json",
            r#"{"variant_id": "single_start", "hypothesis": "one start, with the polish"}"#,
        );

        program(root, "same_source", 0, CONTROL_SOURCE, false);
        program(root, "same_source", 1, CONTROL_SOURCE, true);
        verification(root, "same_source", 1, false);
        write(
            root,
            "ablations/variants/same_source/variant_spec.json",
            r#"{"variant_id": "same_source"}"#,
        );

        program(root, "unverified", 0, CONTROL_SOURCE, false);
        program(root, "unverified", 1, &changed, true);
        write(
            root,
            "ablations/variants/unverified/variant_spec.json",
            r#"{"variant_id": "unverified"}"#,
        );
        dir
    }

    fn staged(root: &Path) -> Vec<Value> {
        let paper = root.join("paper");
        let dossier = stage::run(root, &paper).unwrap();
        dossier.data["ablation_records"].as_array().unwrap().clone()
    }

    fn by_id<'a>(records: &'a [Value], id: &str) -> &'a Value {
        records
            .iter()
            .find(|record| record["id"] == id)
            .unwrap_or_else(|| panic!("no record for {id}"))
    }

    fn read(root: &Path, name: &str) -> String {
        std::fs::read_to_string(root.join("paper").join(name)).unwrap()
    }

    #[test]
    fn a_variant_gets_its_spec_its_diff_and_its_verification_record() {
        let dir = fixture();
        let records = staged(dir.path());
        let record = by_id(&records, "single_start");
        assert_eq!(record["scored_program"], "program_2");
        assert_eq!(record["compared_with"], "control_best/program_0");
        assert_eq!(record["status"], "pass");
        for name in ["variant_spec.json", "source.diff", "verification.json"] {
            assert!(
                dir.path()
                    .join(format!(
                        "paper/dossier/ablations/variants/single_start/{name}"
                    ))
                    .is_file(),
                "{name}"
            );
        }
        assert_eq!(
            record["files"]["source_diff"],
            "dossier/ablations/variants/single_start/source.diff"
        );
        assert_eq!(record["source_diff"]["changed"], true);
        assert_eq!(
            record["source_diff"]["files_changed"],
            json!(["program.py"])
        );
        assert_eq!(record["source_diff"]["lines_added"], 1);
        assert_eq!(record["source_diff"]["lines_removed"], 1);
        assert_eq!(record["source_diff"]["truncated"], false);
        assert_eq!(record["verification"]["found"], true);
        assert_eq!(record["verification"]["overall_pass"], true);

        let diff = read(
            dir.path(),
            "dossier/ablations/variants/single_start/source.diff",
        );
        assert!(diff.contains("--- control_best/src/program.py"), "{diff}");
        assert!(diff.contains("+++ single_start/src/program.py"), "{diff}");
        assert!(diff.contains("-    cen = optimize()"), "{diff}");
        assert!(diff.contains("+    cen = optimize_once()"), "{diff}");
        // The unchanged polish call is context, so a reader sees it is still there.
        assert!(diff.contains("     out = polish(cen)"), "{diff}");
        // The verification record is the one of the last program, not the first.
        let record = read(
            dir.path(),
            "dossier/ablations/variants/single_start/verification.json",
        );
        assert!(record.contains("polish is still called"), "{record}");
        assert!(record.contains("\"overall_pass\": true"), "{record}");
    }

    #[test]
    fn a_variant_with_no_diff_says_so_instead_of_staging_an_empty_file() {
        let dir = fixture();
        let records = staged(dir.path());
        let record = by_id(&records, "same_source");
        assert_eq!(record["scored_program"], "program_1");
        assert_eq!(record["source_diff"]["changed"], false);
        assert_eq!(record["source_diff"]["files_changed"], json!([]));
        let text = read(
            dir.path(),
            "dossier/ablations/variants/same_source/source.diff",
        );
        assert!(
            text.contains("identical to control_best/program_0 (1 source files compared)"),
            "{text}"
        );
        // A failed verification is still a record the roles may read.
        assert_eq!(record["verification"]["found"], true);
        assert_eq!(record["verification"]["overall_pass"], false);
    }

    #[test]
    fn a_missing_verification_record_is_named_and_no_file_is_made_up() {
        let dir = fixture();
        let records = staged(dir.path());
        let record = by_id(&records, "unverified");
        assert_eq!(record["verification"]["found"], false);
        assert!(
            record["verification"]["reason"]
                .as_str()
                .unwrap()
                .contains("no verification record"),
            "{record}"
        );
        assert_eq!(record["files"]["verification"], Value::Null);
        assert!(
            !dir.path()
                .join("paper/dossier/ablations/variants/unverified/verification.json")
                .exists()
        );
        // The rest of the record is still there.
        assert_eq!(record["source_diff"]["changed"], true);
        assert!(record["files"]["spec"].is_string());
    }

    #[test]
    fn a_control_has_no_diff_and_no_verification_and_appears_once() {
        let dir = fixture();
        let records = staged(dir.path());
        assert_eq!(
            records
                .iter()
                .filter(|record| record["id"] == "control_best")
                .count(),
            1
        );
        let record = by_id(&records, "control_best");
        assert_eq!(record["scored_program"], "program_0");
        assert_eq!(record["compared_with"], Value::Null);
        assert_eq!(record["source_diff"]["changed"], false);
        assert_eq!(record["verification"]["found"], false);
        assert!(
            record["verification"]["reason"]
                .as_str()
                .unwrap()
                .contains("not edited"),
            "{record}"
        );
        assert!(record["files"]["spec"].is_string());
    }

    #[test]
    fn the_seed_baseline_is_compared_with_the_control_not_with_itself() {
        let dir = fixture();
        let records = staged(dir.path());
        let record = by_id(&records, "seed_baseline");
        assert_eq!(record["scored_program"], "program_0");
        assert_eq!(record["compared_with"], "control_best/program_0");
        assert_eq!(record["source_diff"]["changed"], true);
        let diff = read(
            dir.path(),
            "dossier/ablations/variants/seed_baseline/source.diff",
        );
        assert!(diff.contains("--- control_best/src/program.py"), "{diff}");
        assert!(diff.contains("+++ seed_baseline/src/program.py"), "{diff}");
        assert!(diff.contains("-    out = polish(cen)"), "{diff}");
        assert!(diff.contains("+    out = cen"), "{diff}");
    }

    #[test]
    fn without_a_control_a_variant_is_compared_with_its_own_first_program() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "ablations/pipeline_summary.json",
            r#"{"variants": [{"id": "v", "kind": "ablation", "status": "pass", "score": 1.0}]}"#,
        );
        program(root, "v", 0, CONTROL_SOURCE, false);
        program(root, "v", 1, "x = 1\n", true);
        let records = staged(root);
        let record = by_id(&records, "v");
        assert_eq!(record["compared_with"], "v/program_0");
        assert_eq!(record["source_diff"]["changed"], true);
        let diff = read(root, "dossier/ablations/variants/v/source.diff");
        assert!(diff.contains("--- program_0/src/program.py"), "{diff}");
    }

    #[test]
    fn a_variant_that_never_ran_gets_a_record_with_nothing_staged() {
        let dir = fixture();
        let records = staged(dir.path());
        let record = by_id(&records, "never_ran");
        assert_eq!(record["scored_program"], Value::Null);
        assert_eq!(record["files"]["spec"], Value::Null);
        assert_eq!(record["files"]["source_diff"], Value::Null);
        assert_eq!(record["files"]["verification"], Value::Null);
        assert_eq!(record["source_diff"]["changed"], Value::Null);
        assert!(
            !dir.path()
                .join("paper/dossier/ablations/variants/never_ran")
                .exists()
        );
    }

    #[test]
    fn a_run_without_ablations_has_no_records() {
        let dir = tempfile::tempdir().unwrap();
        let data = stage::run(dir.path(), &dir.path().join("paper"))
            .unwrap()
            .data;
        assert_eq!(data["ablation_records"], json!([]));
    }

    #[test]
    fn an_id_that_leaves_its_folder_is_not_staged() {
        let dir = fixture();
        write(
            dir.path(),
            "ablations/pipeline_summary.json",
            r#"{"variants": [{"id": "../task", "kind": "ablation", "status": "pass", "score": 1.0}]}"#,
        );
        assert!(staged(dir.path()).is_empty());
    }

    #[test]
    fn a_long_diff_is_cut_and_the_record_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "ablations/pipeline_summary.json",
            r#"{"variants": [{"id": "big", "kind": "ablation", "status": "pass", "score": 1.0}]}"#,
        );
        let control = "x = 1\n";
        let rewritten: String = (0..20_000).map(|n| format!("line_{n} = {n}\n")).collect();
        program(root, "big", 0, control, false);
        program(root, "big", 1, &rewritten, true);
        let records = staged(root);
        let record = by_id(&records, "big");
        assert_eq!(record["source_diff"]["truncated"], true);
        assert_eq!(record["source_diff"]["lines_added"], 20_000);
        let text = read(root, "dossier/ablations/variants/big/source.diff");
        assert!(text.len() <= MAX_DIFF_BYTES + 100, "{}", text.len());
        assert!(text.contains("more lines are not shown"), "{text}");
    }

    #[test]
    fn the_numeric_order_of_programs_decides_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for index in [2, 10, 9] {
            program(root, "v", index, "a = 1\n", true);
        }
        let found = scored_program(&root.join("ablations/variants/v"));
        assert_eq!(found, Some(10));
    }
}
