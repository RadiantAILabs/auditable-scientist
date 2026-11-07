//! Staging of the evidence for the paper.
//!
//! [`run`] reads what the earlier phases left in the run folder, copies a
//! bounded selection of it into `paper/dossier/`, and writes `paper_data.json`:
//! the totals the paper may quote and a registry of source ids
//! (`grounding_sources`) for every metric, count and analysis. Claims cite
//! those ids, and the number check compares the paper's numbers with them.
//!
//! A claim may state a number only if a recorded source holds it, so the
//! registry also lists the numbers a paper has reason to print: the constants
//! of the task (`task.constants.<name>`), the numeric constants a program
//! defines at module level (`program.constants.<NAME>`), and the numbers of each
//! post-hoc data file (`values` of `posthoc.<analysis>.data.<stem>`, each under
//! its path in the file).
//!
//! Besides the totals and the registry, the file carries the lists the
//! original paper data holds: `all_programs` (every evaluated attempt that has
//! a score), `top_programs` (the ten best correct ones), `improvement_trace`
//! (each correct attempt that beat all earlier ones), `evidence_sources` (which
//! phases supplied evidence) and `presentation_contract` (how the visible paper
//! names things). The top programs and the improvements are also registered as
//! sources (`result.top_program_NN`, `result.improvement_NNN`).
//!
//! The `baseline` block is written only when the official scorer accepted the
//! baseline run: `task/baseline/metrics.json` holds a finite score and
//! `task/baseline/correct.json` says `correct: true`. The packing is not judged
//! again here. The scorer allows circles of radius zero, and such circles can
//! share a centre, so a repeated centre in the printed text is not a sign of an
//! invalid packing. The block `baseline_provenance` names the files the score
//! and the verdict came from and holds the scorer's own record of the run
//! (`evaluation_manifest.json`). Without a passing verdict the block and its
//! `baseline.*` sources are left out and `limitations` says that no baseline
//! comparison is made.
//!
//! For every ablation variant and control the dossier holds three small files
//! under `ablations/variants/<id>/` (see the `variants` module): the variant's
//! specification, the diff of its source against the control's, and the
//! verification record of its last program. `ablation_records` lists them with
//! the reason for each that is absent.
//!
//! Every input is optional. A run whose ablations or post-hoc phase produced
//! nothing still gets a paper data file; the missing part is recorded under
//! `inputs`. The output is a pure function of the inputs: it holds no time,
//! no absolute path and no random value.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use super::state::{Analysis, DOSSIER_DIR};

mod variants;

/// Largest file copied into the dossier.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Most files copied into the dossier.
const MAX_FILES: usize = 400;
/// Most bytes copied into the dossier.
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
/// Most characters of the task description kept as the goal.
const GOAL_CHARS: usize = 4000;
/// Folder names never copied.
const SKIPPED_DIRS: [&str; 6] = [
    ".venv",
    "__pycache__",
    ".git",
    "node_modules",
    "_extracted_bundle",
    "_abandoned",
];

/// Files of the paper folder that the phase generates; a new attempt starts
/// without them.
const GENERATED: [&str; 23] = [
    "paper_data.json",
    "references.bib",
    "references_provenance.json",
    "claims_grounding.json",
    "paper.tex",
    "paper.pdf",
    "paper.aux",
    "paper.bbl",
    "paper.blg",
    "paper.log",
    "paper.out",
    "figure_manifest.json",
    "validation_report.json",
    "paper_evidence_audit.json",
    "paper_evidence_audit.md",
    "paper_manifest.json",
    "claims_check.json",
    "reference_query_plan.json",
    "reference_query_plan.md",
    "postdraft_reference_query_plan.json",
    "postdraft_reference_query_plan.md",
    "draft_reference_gaps.json",
    "draft_reference_gaps.md",
];

/// Folders and further files of the paper folder that the phase generates.
const GENERATED_MORE: [&str; 8] = [
    "paper_strategy.json",
    "paper_strategy.md",
    "posthoc_ranking.json",
    "audit_response.txt",
    "repair_feedback.md",
    "review_feedback.md",
    "figures",
    "rounds",
];

/// What staging found.
#[derive(Clone, Debug)]
pub struct Dossier {
    /// The content of `paper_data.json`.
    pub data: Value,
    /// Every id in `grounding_sources`.
    pub source_ids: BTreeSet<String>,
    /// Accepted post-hoc analyses.
    pub analyses: Vec<Analysis>,
    /// The task goal.
    pub goal: String,
}

/// Removes what an earlier attempt of the phase generated from `paper_dir`.
///
/// # Errors
///
/// Returns the I/O error of a removal that failed.
pub fn clean(paper_dir: &Path) -> std::io::Result<()> {
    for name in GENERATED
        .iter()
        .chain(GENERATED_MORE.iter())
        .chain([DOSSIER_DIR].iter())
    {
        let path = paper_dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path)?,
            Ok(_) => std::fs::remove_file(&path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Stages the evidence of the run at `root` into `paper_dir` and writes
/// `paper_data.json` there.
///
/// # Errors
///
/// Returns a sentence when a file cannot be written or the dossier would
/// exceed its bounds.
pub fn run(root: &Path, paper_dir: &Path) -> Result<Dossier, String> {
    std::fs::create_dir_all(paper_dir.join(DOSSIER_DIR))
        .map_err(|e| format!("cannot create the paper folder: {e}"))?;
    let dossier_dir = paper_dir.join(DOSSIER_DIR);
    let mut copier = Copier::new(root, &dossier_dir);
    let mut sources = Map::new();
    let mut inputs = Map::new();

    let baseline = read_json(&root.join("task/baseline/metrics.json"));
    let (task, goal) = task(root, baseline.as_ref(), &mut copier, &mut inputs);
    register_task_constants(&mut sources, &task);
    let best = investigation(
        root,
        baseline.as_ref(),
        &mut copier,
        &mut sources,
        &mut inputs,
    );
    let ablations = ablations(root, &mut copier, &mut sources, &mut inputs);
    let (analyses, analysis_rows) = posthoc(root, &mut copier, &mut sources, &mut inputs);
    let final_evaluation = final_evaluation(root, &mut copier, &mut inputs);
    if let Some(error) = copier.error.take() {
        return Err(error);
    }

    let totals = json!({
        "evaluated_programs": best.get("evaluated_programs").cloned().unwrap_or(json!(0)),
        "correct_programs": best.get("correct_programs").cloned().unwrap_or(json!(0)),
        "ablation_variants": ablations.get("variant_count").cloned().unwrap_or(json!(0)),
        "posthoc_analyses": analyses.len(),
    });
    let evidence_sources = evidence_sources(&inputs, &ablations, &final_evaluation);
    let source_ids: BTreeSet<String> = sources.keys().cloned().collect();
    let mut data = json!({
        "paper_profile": "standalone",
        "presentation_contract": presentation_contract(),
        "task": task,
        "totals": totals,
        "best_program": best.get("best_program").cloned().unwrap_or(Value::Null),
        "top_programs": best.get("top_programs").cloned().unwrap_or(json!([])),
        "improvement_trace": best.get("improvement_trace").cloned().unwrap_or(json!([])),
        "all_programs": best.get("all_programs").cloned().unwrap_or(json!([])),
        "ablations": ablations.get("summary").cloned().unwrap_or(Value::Null),
        "ablation_records": ablations.get("records").cloned().unwrap_or(json!([])),
        "posthoc_analyses": analysis_rows,
        "final_evaluation": final_evaluation,
        "evidence_sources": evidence_sources,
        "limitations": best.get("limitations").cloned().unwrap_or(json!([])),
        "inputs": inputs,
        "grounding_sources": sources,
    });
    for key in ["baseline", "baseline_provenance"] {
        if let Some(value) = best.get(key) {
            data[key] = value.clone();
        }
    }
    super::files::write_json(&paper_dir.join("paper_data.json"), &data)
        .map_err(|e| format!("cannot write paper_data.json: {e}"))?;
    Ok(Dossier {
        data,
        source_ids,
        analyses,
        goal,
    })
}

/// Reads a JSON file, or `None` when it is missing or not JSON.
fn read_json(path: &Path) -> Option<Value> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn read_text(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

/// A path component that cannot leave its folder.
fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && name.len() <= 120
}

fn safe_relative(path: &str) -> bool {
    let candidate = Path::new(path);
    !path.is_empty()
        && candidate
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Copies files into the dossier within fixed bounds.
struct Copier<'a> {
    root: &'a Path,
    dest: &'a Path,
    files: usize,
    bytes: u64,
    error: Option<String>,
}

impl<'a> Copier<'a> {
    fn new(root: &'a Path, dest: &'a Path) -> Self {
        Self {
            root,
            dest,
            files: 0,
            bytes: 0,
            error: None,
        }
    }

    /// Copies a regular file from `source` to `dossier/<name>`. Links, large
    /// files and files past the bounds are left out.
    fn file(&mut self, source: &Path, name: &str) -> bool {
        let Ok(meta) = std::fs::symlink_metadata(source) else {
            return false;
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            return false;
        }
        if self.files >= MAX_FILES || self.bytes + meta.len() > MAX_TOTAL_BYTES {
            self.error.get_or_insert_with(|| {
                "the evidence for the paper is larger than the dossier allows".to_string()
            });
            return false;
        }
        let target = self.dest.join(name);
        if let Some(parent) = target.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.error
                .get_or_insert_with(|| format!("cannot create the dossier folder: {e}"));
            return false;
        }
        match std::fs::copy(source, &target) {
            Ok(_) => {
                self.files += 1;
                self.bytes += meta.len();
                true
            }
            Err(e) => {
                self.error
                    .get_or_insert_with(|| format!("cannot copy {} into the dossier: {e}", name));
                false
            }
        }
    }

    /// Writes `content` to `dossier/<name>` within the same bounds as a copied
    /// file.
    fn text(&mut self, name: &str, content: &str) -> bool {
        let size = content.len() as u64;
        if size > MAX_FILE_BYTES {
            return false;
        }
        if self.files >= MAX_FILES || self.bytes + size > MAX_TOTAL_BYTES {
            self.error.get_or_insert_with(|| {
                "the evidence for the paper is larger than the dossier allows".to_string()
            });
            return false;
        }
        let target = self.dest.join(name);
        if let Some(parent) = target.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.error
                .get_or_insert_with(|| format!("cannot create the dossier folder: {e}"));
            return false;
        }
        match std::fs::write(&target, content) {
            Ok(()) => {
                self.files += 1;
                self.bytes += size;
                true
            }
            Err(e) => {
                self.error
                    .get_or_insert_with(|| format!("cannot write {name} into the dossier: {e}"));
                false
            }
        }
    }

    /// Copies a folder tree to `dossier/<name>`, in sorted order.
    fn tree(&mut self, source: &Path, name: &str) {
        let mut entries: Vec<_> = match std::fs::read_dir(source) {
            Ok(rd) => rd.filter_map(Result::ok).collect(),
            Err(_) => return,
        };
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let target = format!("{name}/{file_name}");
            if kind.is_dir() {
                if !SKIPPED_DIRS.contains(&file_name.as_str()) {
                    self.tree(&entry.path(), &target);
                }
            } else if kind.is_file() {
                self.file(&entry.path(), &target);
            }
        }
    }

    /// True when `path` is inside the run folder once links are resolved.
    fn inside_run(&self, path: &Path) -> bool {
        let (Ok(path), Ok(root)) = (path.canonicalize(), self.root.canonicalize()) else {
            return false;
        };
        path.starts_with(root)
    }
}

fn note(inputs: &mut Map<String, Value>, name: &str, found: bool, detail: &str) {
    inputs.insert(name.to_string(), json!({"found": found, "detail": detail}));
}

/// The task: its title and its constants. The constants are the numbers the
/// evaluator reports as public for the baseline run (the number of circles, for
/// example). They describe the problem, not a result, so they stay when the
/// baseline itself is left out.
fn task(
    root: &Path,
    baseline: Option<&Value>,
    copier: &mut Copier<'_>,
    inputs: &mut Map<String, Value>,
) -> (Value, String) {
    let constants: Map<String, Value> = baseline
        .and_then(|b| b.get("public"))
        .and_then(Value::as_object)
        .map(|public| {
            public
                .iter()
                .filter(|(_, value)| value.is_number())
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    let text = read_text(&root.join("task/task.md")).unwrap_or_default();
    note(inputs, "task", !text.is_empty(), "task/task.md");
    copier.file(&root.join("task/task.md"), "task.md");
    let goal: String = text.chars().take(GOAL_CHARS).collect();
    let name = root
        .join("task")
        .read_link()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| {
            text.lines()
                .find(|l| l.trim_start().starts_with('#'))
                .map(|l| l.trim_start_matches(['#', ' ']).trim().to_string())
        })
        .unwrap_or_default();
    (json!({"title": name, "constants": constants}), goal)
}

/// How the visible paper names things; the same contract the original gives
/// the standalone profile.
fn presentation_contract() -> Value {
    json!({
        "variant_unit": "model_variant",
        "run_unit": "seeded_run",
        "internal_fields": ["correct", "eligible"],
        "forbidden_visible_terms": [
            "eligible",
            "correct and eligible",
            "correctness rate",
            "worker_01",
            "worker_02",
            "combined_score",
            "rubric",
        ],
        "preferred_terms": {
            "idea": "model variant",
            "ideas": "model variants",
            "program": "seeded run",
            "programs": "seeded runs",
            "implementation": "configuration",
            "implementations": "configurations",
        },
        "threshold_rate_rule": "If a thresholded rate appears in the visible paper, keep one \
            reader-facing meaning per figure, such as target attainment, successful-run \
            fraction, or non-collapse rate. Do not reuse a single label for different \
            task-specific thresholds.",
    })
}

/// Which phases supplied evidence, as the original's `evidence_sources`:
/// `included` when the phase left its record, `missing` when it did not, and
/// `degraded` for a final evaluation that left only a launch failure.
fn evidence_sources(
    inputs: &Map<String, Value>,
    ablations: &Value,
    final_evaluation: &Value,
) -> Value {
    let found = |name: &str| {
        inputs
            .get(name)
            .and_then(|entry| entry.get("found"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let entry = |name: &str, ok: bool| {
        if ok {
            json!({"status": "included", "path": name})
        } else {
            json!({"status": "missing", "path": name})
        }
    };
    let mut out = Map::new();
    out.insert(
        "investigation".into(),
        entry("investigation", found("investigation")),
    );
    out.insert(
        "ablations".into(),
        entry(
            "ablations",
            !ablations.get("summary").is_none_or(Value::is_null),
        ),
    );
    out.insert("posthoc".into(), entry("posthoc", found("posthoc")));
    out.insert(
        "final_evaluation".into(),
        if final_evaluation.is_null() {
            entry("final_evaluation", false)
        } else {
            json!({"status": "degraded", "path": "final_evaluation",
                "reason": "the final evaluation did not run to a result"})
        },
    );
    Value::Object(out)
}

/// Number fields of a metrics file as sources `<prefix>.<name>`.
fn register_metrics(sources: &mut Map<String, Value>, prefix: &str, metrics: &Value, scope: &str) {
    let Some(object) = metrics.as_object() else {
        return;
    };
    for (name, value) in object {
        if !value.is_number() {
            continue;
        }
        let stem: String = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        if stem.is_empty() {
            continue;
        }
        let id = format!("{prefix}.{stem}");
        sources.insert(
            id.clone(),
            json!({"id": id, "type": "metric", "value": value, "scope": scope}),
        );
    }
}

/// Most numbers listed for one data file.
const MAX_LEAVES: usize = 1000;

/// Deepest nesting read from a data file.
const MAX_LEAF_DEPTH: usize = 8;

/// The numbers of a JSON document, each under its path: keys joined by dots,
/// list items by their index (`per_tolerance.1e-07.n_pairs`, `gaps.17`). A claim
/// that cites the data file may print any of them. At most [`MAX_LEAVES`]
/// numbers are listed, in document order.
fn numeric_leaves(data: &Value) -> Map<String, Value> {
    fn walk(path: &str, value: &Value, depth: usize, out: &mut Map<String, Value>) {
        if out.len() >= MAX_LEAVES || depth > MAX_LEAF_DEPTH {
            return;
        }
        match value {
            Value::Number(n) if n.as_f64().is_some_and(f64::is_finite) => {
                out.insert(path.to_string(), value.clone());
            }
            Value::Object(map) => {
                for (key, inner) in map {
                    let next = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    walk(&next, inner, depth + 1, out);
                }
            }
            Value::Array(items) => {
                for (index, inner) in items.iter().enumerate() {
                    let next = if path.is_empty() {
                        index.to_string()
                    } else {
                        format!("{path}.{index}")
                    };
                    walk(&next, inner, depth + 1, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Map::new();
    walk("", data, 0, &mut out);
    out
}

static PROGRAM_CONSTANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Z][A-Z0-9_]*)\s*=\s*(-?\d[\d_]*(?:\.\d*)?(?:[eE][-+]?\d+)?)\s*(?:#.*)?$")
        .expect("program constant pattern is valid")
});

/// Registers the numeric constants a program source defines at module level
/// (`TIME_BUDGET = 40.0`) as sources `program.constants.<NAME>`. They are
/// recorded in the staged program, so a paper may state them as settings of the
/// method.
fn register_program_constants(sources: &mut Map<String, Value>, program: &str) {
    for line in program.lines() {
        let Some(found) = PROGRAM_CONSTANT.captures(line) else {
            continue;
        };
        let literal = found[2].replace('_', "");
        let Ok(value) = literal.parse::<f64>() else {
            continue;
        };
        let Some(number) = serde_json::Number::from_f64(value) else {
            continue;
        };
        let id = format!("program.constants.{}", &found[1]);
        sources.insert(
            id.clone(),
            json!({"id": id, "type": "program_constant", "value": number,
                "file": "dossier/best_program/src/program.py"}),
        );
    }
}

/// Registers the numbers that describe the task as sources
/// `task.constants.<name>`.
fn register_task_constants(sources: &mut Map<String, Value>, task: &Value) {
    let Some(constants) = task.get("constants").and_then(Value::as_object) else {
        return;
    };
    for (name, value) in constants {
        if value.is_number() {
            let id = format!("task.constants.{name}");
            sources.insert(
                id.clone(),
                json!({"id": id, "type": "task_constant", "value": value}),
            );
        }
    }
}

fn register_count(sources: &mut Map<String, Value>, id: &str, value: usize) {
    sources.insert(
        id.to_string(),
        json!({"id": id, "type": "count", "value": value}),
    );
}

/// Sentence recorded under `limitations` when the baseline block is left out.
const NO_BASELINE: &str = "The official scorer did not accept the baseline run (it has no finite score or no passing verdict), so no baseline comparison is made.";

/// The file of the baseline's score.
const BASELINE_SCORE_FILE: &str = "task/baseline/metrics.json";

/// The file of the scorer's verdict on the baseline.
const BASELINE_VERDICT_FILE: &str = "task/baseline/correct.json";

/// The scorer's record of the run that scored the baseline.
const BASELINE_MANIFEST_FILE: &str = "task/baseline/evaluation_manifest.json";

/// Where the official scorer's acceptance of the baseline came from, or `None`
/// when the scorer did not accept it.
///
/// A baseline is accepted when its metrics hold a finite `combined_score` and
/// the scorer's verdict says `correct: true`. The verdict is read from
/// `correct.json`; a verdict that the metrics carry themselves is used when that
/// file is missing. A verdict of `false`, no verdict and no score all leave the
/// baseline out. Nothing is inferred from the printed packing.
fn accepted_baseline(root: &Path, baseline: &Value) -> Option<Value> {
    let scored = baseline
        .get("combined_score")
        .and_then(Value::as_f64)
        .is_some_and(f64::is_finite);
    let verdict = |value: &Value| value.get("correct").and_then(Value::as_bool);
    let record = read_json(&root.join(BASELINE_VERDICT_FILE));
    let (passed, verdict_file) = match &record {
        Some(record) => (verdict(record), BASELINE_VERDICT_FILE),
        None => (verdict(baseline), BASELINE_SCORE_FILE),
    };
    if !scored || passed != Some(true) {
        return None;
    }
    let manifest = read_json(&root.join(BASELINE_MANIFEST_FILE));
    let manifest_field = |name: &str| {
        manifest
            .as_ref()
            .and_then(|m| m.get(name))
            .cloned()
            .unwrap_or(Value::Null)
    };
    Some(json!({
        "score_file": BASELINE_SCORE_FILE,
        "verdict_file": verdict_file,
        "verdict": true,
        "verdict_message": record
            .as_ref()
            .and_then(|r| r.get("message"))
            .cloned()
            .unwrap_or(Value::Null),
        "scorer_record_file": manifest.as_ref().map(|_| BASELINE_MANIFEST_FILE),
        "scorer_sha256": manifest_field("evaluator_sha256"),
        "scorer_integrity_valid": manifest_field("integrity_valid"),
        "basis": "the official scorer's verdict; the packing is not judged again",
        "zero_radius_circles": "allowed by the scorer; circles of radius zero may share a centre",
    }))
}

/// One evaluated attempt with a score, as a row of `all_programs`.
struct History {
    /// Lines of the attempt history that hold a JSON record.
    evaluated: usize,
    /// Of those, records whose metrics say `correct: true`.
    correct: usize,
    /// The records that carry a numeric `combined_score`, in order.
    rows: Vec<Value>,
}

fn investigation(
    root: &Path,
    baseline: Option<&Value>,
    copier: &mut Copier<'_>,
    sources: &mut Map<String, Value>,
    inputs: &mut Map<String, Value>,
) -> Value {
    let manifest = read_json(&root.join("investigation/best_program_manifest.json"));
    note(
        inputs,
        "investigation",
        manifest.is_some(),
        "investigation/best_program_manifest.json",
    );
    let mut out = Map::new();
    if let Some(manifest) = &manifest {
        let score = manifest.get("combined_score").and_then(Value::as_f64);
        let correct = manifest.get("correct").and_then(Value::as_bool);
        let attempt = manifest
            .get("source_attempt_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(score) = score {
            sources.insert(
                "result.best_any_score".into(),
                json!({"id": "result.best_any_score", "type": "metric", "value": score,
                    "attempt_id": attempt}),
            );
            if correct == Some(true) {
                sources.insert(
                    "result.best_correct_score".into(),
                    json!({"id": "result.best_correct_score", "type": "metric", "value": score,
                        "attempt_id": attempt}),
                );
            }
        }
        out.insert(
            "best_program".into(),
            json!({"combined_score": score, "correct": correct, "source_attempt_id": attempt}),
        );
        if let Some(dir) = manifest.get("program_dir").and_then(Value::as_str) {
            let dir = PathBuf::from(dir);
            if copier.inside_run(&dir) {
                copier.tree(&dir, "best_program");
                if let Some(program) = read_text(&dir.join("src/program.py")) {
                    register_program_constants(sources, &program);
                }
            }
        }
    }
    match baseline.and_then(|b| Some((b, accepted_baseline(root, b)?))) {
        Some((baseline, provenance)) => {
            register_metrics(sources, "baseline", baseline, "baseline");
            out.insert("baseline".into(), baseline.clone());
            out.insert("baseline_provenance".into(), provenance);
            out.insert("limitations".into(), json!([]));
        }
        None => {
            out.insert("limitations".into(), json!([NO_BASELINE]));
        }
    }
    let history = attempts(root);
    register_count(
        sources,
        "result.total_evaluated_programs",
        history.evaluated,
    );
    register_count(sources, "result.total_correct_programs", history.correct);
    out.insert("evaluated_programs".into(), json!(history.evaluated));
    out.insert("correct_programs".into(), json!(history.correct));
    program_lists(&history.rows, sources, &mut out);
    for name in [
        "conclusions.md",
        "ideas_history.jsonl",
        "attempts_history.jsonl",
    ] {
        copier.file(
            &root.join("investigation").join(name),
            &format!("investigation/{name}"),
        );
    }
    Value::Object(out)
}

/// The attempt history: how many records it holds, how many are correct, and
/// a row for each record that has a score. `eval_index` counts the scored
/// rows from 1, in the order the history lists them.
fn attempts(root: &Path) -> History {
    let text = read_text(&root.join("investigation/attempts_history.jsonl")).unwrap_or_default();
    let mut history = History {
        evaluated: 0,
        correct: 0,
        rows: Vec::new(),
    };
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        history.evaluated += 1;
        let metrics = row.get("metrics");
        let correct = metrics
            .and_then(|m| m.get("correct"))
            .and_then(Value::as_bool);
        if correct == Some(true) {
            history.correct += 1;
        }
        let Some(score) = metrics
            .and_then(|m| m.get("combined_score"))
            .and_then(Value::as_f64)
            .filter(|s| s.is_finite())
        else {
            continue;
        };
        let text_of = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default();
        history.rows.push(json!({
            "eval_index": history.rows.len() + 1,
            "idea_id": text_of("idea_id"),
            "idea_dir_name": text_of("idea_dir_name"),
            "attempt_id": text_of("attempt_id"),
            "combined_score": score,
            "correct": correct,
            "eligible": correct == Some(true),
        }));
    }
    history
}

/// Adds `all_programs`, `top_programs` and `improvement_trace` to `out`, and
/// the sources `result.top_program_NN` and `result.improvement_NNN`.
///
/// The top programs are the ten best correct rows (ties go to the earlier
/// one). The improvements are the correct rows that scored higher than every
/// correct row before them.
fn program_lists(rows: &[Value], sources: &mut Map<String, Value>, out: &mut Map<String, Value>) {
    let score = |row: &Value| row["combined_score"].as_f64().unwrap_or(f64::NEG_INFINITY);
    let index = |row: &Value| row["eval_index"].as_u64().unwrap_or(0);
    let correct: Vec<&Value> = rows.iter().filter(|r| r["eligible"] == true).collect();

    let mut ranked = correct.clone();
    ranked.sort_by(|a, b| {
        score(b)
            .total_cmp(&score(a))
            .then_with(|| index(a).cmp(&index(b)))
    });
    ranked.truncate(10);

    let mut trace = Vec::new();
    let mut best = f64::NEG_INFINITY;
    for row in &correct {
        if score(row) <= best {
            continue;
        }
        best = score(row);
        let id = format!("result.improvement_{:03}", trace.len() + 1);
        trace.push(json!({
            "id": id,
            "step_index": index(row),
            "x_index": index(row).saturating_sub(1),
            "idea_id": row["idea_id"],
            "attempt_id": row["attempt_id"],
            "combined_score": row["combined_score"],
        }));
    }

    let register = |sources: &mut Map<String, Value>, id: String, row: &Value| {
        sources.insert(
            id.clone(),
            json!({"id": id, "type": "metric", "value": row["combined_score"],
                "idea_id": row["idea_id"], "attempt_id": row["attempt_id"]}),
        );
    };
    for (position, row) in ranked.iter().enumerate() {
        register(
            sources,
            format!("result.top_program_{:02}", position + 1),
            row,
        );
    }
    for row in &trace {
        let id = row["id"].as_str().unwrap_or_default().to_string();
        register(sources, id, row);
    }

    out.insert("all_programs".into(), json!(rows));
    out.insert("top_programs".into(), json!(ranked));
    out.insert("improvement_trace".into(), json!(trace));
}

fn ablations(
    root: &Path,
    copier: &mut Copier<'_>,
    sources: &mut Map<String, Value>,
    inputs: &mut Map<String, Value>,
) -> Value {
    let summary = read_json(&root.join("ablations/pipeline_summary.json"));
    note(
        inputs,
        "ablations",
        summary.is_some(),
        "ablations/pipeline_summary.json",
    );
    let Some(summary) = summary else {
        return json!({"variant_count": 0, "summary": Value::Null});
    };
    copier.file(
        &root.join("ablations/pipeline_summary.json"),
        "ablations/pipeline_summary.json",
    );
    for name in ["variant_summaries.json", "README.md"] {
        copier.file(
            &root.join("ablations").join(name),
            &format!("ablations/{name}"),
        );
    }
    copier.tree(&root.join("ablations/aggregated"), "ablations/aggregated");
    let records = variants::stage(root, copier, &summary);

    let mut count = 0;
    for (key, scope) in [("variants", "ablation"), ("controls", "control")] {
        for row in summary
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = row.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !safe_component(id) {
                continue;
            }
            if scope == "ablation" {
                count += 1;
            }
            let Some(score) = row.get("score").filter(|s| s.is_number()) else {
                continue;
            };
            let source = if scope == "ablation" {
                format!("ablation.{id}.score")
            } else {
                format!("ablation.control.{id}.score")
            };
            sources.insert(
                source.clone(),
                json!({"id": source, "type": "metric", "value": score, "scope": scope,
                    "variant_id": id,
                    "correct": row.get("correct").cloned().unwrap_or(Value::Null)}),
            );
        }
    }
    register_count(sources, "result.total_ablation_variants", count);
    for (key, id) in [
        (
            "replications_requested",
            "result.ablation_replications_requested",
        ),
        (
            "replications_observed",
            "result.ablation_replications_observed",
        ),
    ] {
        if let Some(n) = summary.get(key).and_then(Value::as_u64) {
            sources.insert(id.into(), json!({"id": id, "type": "count", "value": n}));
        }
    }
    json!({"variant_count": count, "summary": summary, "records": records})
}

fn posthoc(
    root: &Path,
    copier: &mut Copier<'_>,
    sources: &mut Map<String, Value>,
    inputs: &mut Map<String, Value>,
) -> (Vec<Analysis>, Vec<Value>) {
    let manifest = read_json(&root.join("posthoc/latest_manifest.json"));
    note(
        inputs,
        "posthoc",
        manifest.is_some(),
        "posthoc/latest_manifest.json",
    );
    let mut analyses = Vec::new();
    let mut rows = Vec::new();
    let Some(manifest) = manifest else {
        return (analyses, rows);
    };
    for entry in manifest
        .get("analyses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if entry.get("status").and_then(Value::as_str) != Some("valid") {
            continue;
        }
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(dir) = entry.get("output_dir").and_then(Value::as_str) else {
            continue;
        };
        let dir = PathBuf::from(dir);
        if !safe_component(id) || !copier.inside_run(&dir) {
            continue;
        }
        let Some(summary) =
            read_text(&dir.join("analysis_summary.md")).filter(|s| !s.trim().is_empty())
        else {
            continue;
        };
        let rubric = read_json(&dir.join("interestingness_rubric.json")).unwrap_or(json!({}));
        let mut files: Vec<String> = vec![
            "analysis_summary.md".into(),
            "interestingness_rubric.json".into(),
            "analysis_manifest.json".into(),
        ];
        let listed = entry
            .get("outputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|p| safe_relative(p));
        files.extend(listed.map(str::to_string));
        files.sort();
        files.dedup();
        let mut copied = Vec::new();
        for file in &files {
            if copier.file(&dir.join(file), &format!("posthoc/{id}/{file}")) {
                copied.push(file.clone());
            }
        }
        let summary_id = format!("posthoc.{id}.summary");
        sources.insert(
            summary_id.clone(),
            json!({"id": summary_id, "type": "posthoc_analysis", "analysis_id": id}),
        );
        for file in &copied {
            let stem = Path::new(file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if matches!(
                file.as_str(),
                "analysis_summary.md" | "interestingness_rubric.json" | "analysis_manifest.json"
            ) || stem.is_empty()
            {
                continue;
            }
            let data_id = format!("posthoc.{id}.data.{stem}");
            let mut record = json!({"id": data_id, "type": "posthoc_data", "analysis_id": id,
                "data_file": stem});
            if file.ends_with(".json")
                && let Some(data) = read_json(&dir.join(file))
            {
                let values = numeric_leaves(&data);
                if !values.is_empty() {
                    record["values"] = Value::Object(values);
                }
            }
            sources.insert(data_id, record);
        }
        rows.push(json!({
            "analysis_id": id,
            "title": rubric.get("title").cloned().unwrap_or_else(|| json!(id)),
            "summary_text": summary,
            "rubric": rubric,
            "files": copied,
            "rank": Value::Null,
        }));
        analyses.push(Analysis {
            id: id.to_string(),
            dir,
        });
    }
    (analyses, rows)
}

fn final_evaluation(
    root: &Path,
    copier: &mut Copier<'_>,
    inputs: &mut Map<String, Value>,
) -> Value {
    let record = read_json(&root.join("final_evaluation/final_launch_failure.json"));
    note(
        inputs,
        "final_evaluation",
        record.is_some(),
        "final_evaluation/final_launch_failure.json",
    );
    copier.file(
        &root.join("final_evaluation/final_launch_failure.json"),
        "final_evaluation/final_launch_failure.json",
    );
    record.unwrap_or(Value::Null)
}

/// Records the ranking of the analyses in `paper_data.json`.
///
/// # Errors
///
/// Returns a sentence when the data file cannot be read or written.
pub fn apply_ranking(paper_dir: &Path, ranking: &[(String, Value)]) -> Result<(), String> {
    let path = paper_dir.join("paper_data.json");
    let mut data = read_json(&path).ok_or("paper_data.json cannot be read")?;
    if let Some(rows) = data
        .get_mut("posthoc_analyses")
        .and_then(Value::as_array_mut)
    {
        for row in rows.iter_mut() {
            let id = row
                .get("analysis_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some((_, entry)) = ranking.iter().find(|(rid, _)| *rid == id)
                && let Some(object) = row.as_object_mut()
            {
                for key in [
                    "rank",
                    "relevance_to_thesis",
                    "relevance_justification",
                    "recommended_usage",
                    "key_finding_for_paper",
                ] {
                    if let Some(value) = entry.get(key) {
                        object.insert(key.to_string(), value.clone());
                    }
                }
            }
        }
        rows.sort_by_key(|row| row.get("rank").and_then(Value::as_u64).unwrap_or(u64::MAX));
    }
    super::files::write_json(&path, &data).map_err(|e| format!("cannot write paper_data.json: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, name: &str, text: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn run_with_results() -> (tempfile::TempDir, Dossier) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "task/task.md",
            "# Pack circles\nPlace circles in a square.",
        );
        write(
            root,
            "task/baseline/metrics.json",
            r#"{"combined_score": 1.0, "n": 3}"#,
        );
        write(root, "task/baseline/correct.json", r#"{"correct": true}"#);
        write(
            root,
            "investigation/best_program_manifest.json",
            &format!(
                r#"{{"combined_score": 2.5, "correct": true, "source_attempt_id": "c01_a1",
                    "program_dir": "{}"}}"#,
                root.join("investigation/ideas/c01/program_1").display()
            ),
        );
        write(
            root,
            "investigation/ideas/c01/program_1/src/program.py",
            "x = 1\n",
        );
        write(
            root,
            "investigation/attempts_history.jsonl",
            "{\"metrics\": {\"combined_score\": 2.5, \"correct\": true}}\n{\"metrics\": {\"correct\": false}}\n",
        );
        write(
            root,
            "ablations/pipeline_summary.json",
            r#"{"variants": [{"id": "no_swap", "kind": "ablation", "score": 1.7, "correct": true}],
                "controls": [{"id": "control_best", "score": 2.5}],
                "replications_requested": 5, "replications_observed": 5}"#,
        );
        write(
            root,
            "posthoc/latest_manifest.json",
            &format!(
                r#"{{"analyses": [
                    {{"id": "score_forensics", "status": "valid", "outputs": ["diagnostic.json"],
                      "output_dir": "{0}"}},
                    {{"id": "bad", "status": "rejected", "outputs": [], "output_dir": "{0}"}}],
                  "rejected": [], "selected_analysis_id": null}}"#,
                root.join("work/ph/001/outputs").display()
            ),
        );
        write(
            root,
            "work/ph/001/outputs/analysis_summary.md",
            "The score is 2.5.",
        );
        write(
            root,
            "work/ph/001/outputs/interestingness_rubric.json",
            r#"{"interesting": true}"#,
        );
        write(root, "work/ph/001/outputs/diagnostic.json", r#"{"x": 1}"#);
        let paper = root.join("paper");
        let dossier = run(root, &paper).unwrap();
        (dir, dossier)
    }

    #[test]
    fn totals_and_sources_come_from_the_phase_files() {
        let (_dir, dossier) = run_with_results();
        let sources = dossier.data["grounding_sources"].as_object().unwrap();
        assert_eq!(sources["result.best_correct_score"]["value"], 2.5);
        assert_eq!(sources["result.total_evaluated_programs"]["value"], 2);
        assert_eq!(sources["result.total_correct_programs"]["value"], 1);
        assert_eq!(sources["baseline.combined_score"]["value"], 1.0);
        assert_eq!(sources["ablation.no_swap.score"]["value"], 1.7);
        assert_eq!(sources["ablation.control.control_best.score"]["value"], 2.5);
        assert_eq!(sources["result.total_ablation_variants"]["value"], 1);
        assert!(sources.contains_key("posthoc.score_forensics.summary"));
        assert!(sources.contains_key("posthoc.score_forensics.data.diagnostic"));
        assert!(!sources.contains_key("posthoc.bad.summary"));
        assert_eq!(dossier.analyses.len(), 1);
        assert_eq!(dossier.data["totals"]["posthoc_analyses"], 1);
        assert_eq!(dossier.source_ids.len(), sources.len());
        assert!(dossier.goal.contains("Place circles"));
    }

    #[test]
    fn the_numbers_a_paper_may_state_are_registered_with_where_they_were_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "task/task.md", "# Pack");
        write(
            root,
            "task/baseline/metrics.json",
            r#"{"combined_score": 1.0, "public": {"num_circles": 26, "label": "x"}}"#,
        );
        write(
            root,
            "investigation/best_program_manifest.json",
            &format!(
                r#"{{"combined_score": 2.5, "correct": true, "source_attempt_id": "a",
                    "program_dir": "{}"}}"#,
                root.join("investigation/ideas/c01/program_1").display()
            ),
        );
        write(
            root,
            "investigation/ideas/c01/program_1/src/program.py",
            "N = 26\nTIME_BUDGET = 40.0  # seconds\nSHRINK = 1e-11\nMAX_ITER = 1_000\n_PRIVATE = 3\nname = 4\nLABEL = 'x'\nRATIO = 0.5 * 2\n",
        );
        write(
            root,
            "posthoc/latest_manifest.json",
            &format!(
                r#"{{"analyses": [{{"id": "a1", "status": "valid", "outputs": ["d.json", "s.txt"],
                    "output_dir": "{}"}}]}}"#,
                root.join("work/ph/outputs").display()
            ),
        );
        write(root, "work/ph/outputs/analysis_summary.md", "Summary.");
        write(
            root,
            "work/ph/outputs/d.json",
            r#"{"per_tolerance": {"1e-07": {"n_pairs": 57, "ok": true}}, "gaps": [0.5, 0.25], "label": "x"}"#,
        );
        let data = staged_in(root);
        let sources = data["grounding_sources"].as_object().unwrap();
        assert_eq!(sources["task.constants.num_circles"]["value"], 26);
        assert!(!sources.contains_key("task.constants.label"));
        assert_eq!(sources["program.constants.N"]["value"], 26.0);
        assert_eq!(sources["program.constants.TIME_BUDGET"]["value"], 40.0);
        assert_eq!(sources["program.constants.SHRINK"]["value"], 1e-11);
        assert_eq!(sources["program.constants.MAX_ITER"]["value"], 1000.0);
        for absent in ["_PRIVATE", "name", "LABEL", "RATIO"] {
            assert!(
                !sources.contains_key(&format!("program.constants.{absent}")),
                "{absent}"
            );
        }
        let values = &sources["posthoc.a1.data.d"]["values"];
        assert_eq!(values["per_tolerance.1e-07.n_pairs"], 57);
        assert_eq!(values["gaps.1"], 0.25);
        assert!(values.get("per_tolerance.1e-07.ok").is_none());
        assert!(values.get("label").is_none());
    }

    #[test]
    fn at_most_the_first_leaves_of_a_data_file_are_listed() {
        let many: Vec<u32> = (0..3000).collect();
        let leaves = numeric_leaves(&json!({"list": many}));
        assert_eq!(leaves.len(), MAX_LEAVES);
        assert!(leaves.contains_key("list.0"));
        assert!(!leaves.contains_key("list.2999"));
    }

    fn staged_in(root: &Path) -> Value {
        run(root, &root.join("paper")).unwrap().data
    }

    /// Stages a run that holds only the given files and returns its data.
    fn staged(files: &[(&str, &str)]) -> Value {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            write(dir.path(), name, text);
        }
        run(dir.path(), &dir.path().join("paper")).unwrap().data
    }

    const PASSED: &str = r#"{"correct": true, "message": "placed correctly"}"#;

    fn baseline_with(centers: &str) -> String {
        json!({"combined_score": 0.96, "public": {"centers_str": centers, "num_circles": 3}})
            .to_string()
    }

    fn baseline_sources(data: &Value) -> usize {
        data["grounding_sources"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|id| id.starts_with("baseline."))
            .count()
    }

    #[test]
    fn a_baseline_the_scorer_accepted_stays_with_its_provenance() {
        let data = staged(&[
            (
                "task/baseline/metrics.json",
                &baseline_with("  centers[0] = (0.5, 0.5)"),
            ),
            ("task/baseline/correct.json", PASSED),
            (
                "task/baseline/evaluation_manifest.json",
                r#"{"evaluator_sha256": "abc123", "integrity_valid": true}"#,
            ),
        ]);
        assert_eq!(data["baseline"]["combined_score"], 0.96);
        assert_eq!(baseline_sources(&data), 1);
        assert_eq!(data["task"]["constants"]["num_circles"], 3);
        assert_eq!(data["limitations"], json!([]));
        let provenance = &data["baseline_provenance"];
        assert_eq!(provenance["score_file"], "task/baseline/metrics.json");
        assert_eq!(provenance["verdict_file"], "task/baseline/correct.json");
        assert_eq!(provenance["verdict"], true);
        assert_eq!(provenance["verdict_message"], "placed correctly");
        assert_eq!(
            provenance["scorer_record_file"],
            "task/baseline/evaluation_manifest.json"
        );
        assert_eq!(provenance["scorer_sha256"], "abc123");
        assert_eq!(provenance["scorer_integrity_valid"], true);
    }

    #[test]
    fn a_verdict_in_the_metrics_serves_when_the_verdict_file_is_missing() {
        let data = staged(&[(
            "task/baseline/metrics.json",
            r#"{"combined_score": 1.5, "correct": true}"#,
        )]);
        assert_eq!(data["baseline"]["combined_score"], 1.5);
        assert_eq!(
            data["baseline_provenance"]["verdict_file"],
            "task/baseline/metrics.json"
        );
        assert!(data["baseline_provenance"]["scorer_record_file"].is_null());
    }

    #[test]
    fn a_baseline_without_a_score_or_a_passing_verdict_is_left_out() {
        let unscored = json!({"public": {"num_circles": 3}}).to_string();
        let scored = baseline_with("");
        let cases: [Vec<(&str, &str)>; 5] = [
            vec![
                ("task/baseline/metrics.json", unscored.as_str()),
                ("task/baseline/correct.json", PASSED),
            ],
            vec![
                ("task/baseline/metrics.json", scored.as_str()),
                ("task/baseline/correct.json", r#"{"correct": false}"#),
            ],
            vec![(
                "task/baseline/metrics.json",
                r#"{"combined_score": 1.0, "correct": false}"#,
            )],
            vec![("task/baseline/metrics.json", scored.as_str())],
            vec![],
        ];
        for files in cases {
            let data = staged(&files);
            assert!(data.get("baseline").is_none(), "{files:?}");
            assert!(data.get("baseline_provenance").is_none(), "{files:?}");
            assert_eq!(baseline_sources(&data), 0, "{files:?}");
            assert_eq!(data["limitations"], json!([NO_BASELINE]), "{files:?}");
        }
    }

    #[test]
    fn the_baseline_stays_when_the_task_constants_are_all_that_is_left() {
        let data = staged(&[
            ("task/baseline/metrics.json", &baseline_with("")),
            ("task/baseline/correct.json", r#"{"correct": false}"#),
        ]);
        assert_eq!(data["task"]["constants"]["num_circles"], 3);
    }

    /// The baseline of a saved circle packing campaign: 26 circles on a ring,
    /// circles 19 and 25 at the same centre with radius zero. The task's own
    /// scorer accepts radius zero, said `correct: true` and scored
    /// 0.9597642169962064.
    #[test]
    fn a_packing_with_zero_radius_circles_on_one_centre_stays() {
        let fixture = |name: &str| {
            std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/circpack_run/baseline")
                    .join(name),
            )
            .unwrap()
        };
        let arrays: Value = serde_json::from_str(&fixture("arrays.json")).unwrap();
        assert_eq!(arrays["centers"][19], arrays["centers"][25]);
        assert_eq!(arrays["radii"][19], 0.0);
        assert_eq!(arrays["radii"][25], 0.0);
        let sum: f64 = arrays["radii"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_f64().unwrap())
            .sum();
        assert!((sum - arrays["reported_sum"].as_f64().unwrap()).abs() < 1e-12);
        let metrics = fixture("metrics.json");
        assert!(metrics.contains("centers[19] = (0.0100, 0.0100)"));
        assert!(metrics.contains("centers[25] = (0.0100, 0.0100)"));

        let data = staged(&[
            ("task/baseline/metrics.json", &metrics),
            ("task/baseline/correct.json", &fixture("correct.json")),
            (
                "task/baseline/evaluation_manifest.json",
                &fixture("evaluation_manifest.json"),
            ),
        ]);
        assert_eq!(data["baseline"]["combined_score"], 0.959_764_216_996_206_4);
        assert_eq!(
            data["grounding_sources"]["baseline.combined_score"]["value"],
            0.959_764_216_996_206_4
        );
        assert_eq!(data["limitations"], json!([]));
        assert_eq!(data["baseline_provenance"]["verdict"], true);
        assert_eq!(data["baseline_provenance"]["scorer_integrity_valid"], true);
        assert_eq!(
            data["baseline_provenance"]["scorer_sha256"],
            "11187fcb39331efccc1c23aa2761d6eaefce200d2d81df660f32e5595d18a4fa"
        );
    }

    #[test]
    fn the_lists_follow_the_attempt_history() {
        let history = [
            r#"{"idea_id":"i1","idea_dir_name":"d1","attempt_id":"a1","metrics":{"combined_score":2.0,"correct":true}}"#,
            r#"{"idea_id":"i1","idea_dir_name":"d1","attempt_id":"a2","metrics":{"combined_score":2.5,"correct":false}}"#,
            r#"{"idea_id":"i2","idea_dir_name":"d2","attempt_id":"a3","metrics":{"combined_score":2.4,"correct":true}}"#,
            r#"{"idea_id":"i2","idea_dir_name":"d2","attempt_id":"a4","metrics":{"correct":true}}"#,
            r#"{"idea_id":"i3","idea_dir_name":"d3","attempt_id":"a5","metrics":{"combined_score":2.4,"correct":true}}"#,
            r#"{"idea_id":"i3","idea_dir_name":"d3","attempt_id":"a6","metrics":{"combined_score":2.2}}"#,
        ]
        .join("\n");
        let data = staged(&[("investigation/attempts_history.jsonl", &history)]);

        let all = data["all_programs"].as_array().unwrap();
        let attempts: Vec<&str> = all
            .iter()
            .map(|r| r["attempt_id"].as_str().unwrap())
            .collect();
        assert_eq!(attempts, ["a1", "a2", "a3", "a5", "a6"]);
        assert_eq!(all[3]["eval_index"], 4);
        assert_eq!(all[1]["correct"], false);
        assert_eq!(all[1]["eligible"], false);
        assert_eq!(all[4]["correct"], Value::Null);
        assert_eq!(all[4]["eligible"], false);
        assert_eq!(data["totals"]["evaluated_programs"], 6);

        let top: Vec<&str> = data["top_programs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["attempt_id"].as_str().unwrap())
            .collect();
        assert_eq!(top, ["a3", "a5", "a1"], "a tie goes to the earlier attempt");

        let trace = data["improvement_trace"].as_array().unwrap();
        let steps: Vec<(u64, f64)> = trace
            .iter()
            .map(|r| {
                (
                    r["step_index"].as_u64().unwrap(),
                    r["combined_score"].as_f64().unwrap(),
                )
            })
            .collect();
        assert_eq!(steps, [(1, 2.0), (3, 2.4)]);
        assert_eq!(trace[1]["id"], "result.improvement_002");
        assert_eq!(trace[1]["x_index"], 2);

        let sources = data["grounding_sources"].as_object().unwrap();
        assert_eq!(sources["result.top_program_01"]["value"], 2.4);
        assert_eq!(sources["result.top_program_01"]["attempt_id"], "a3");
        assert_eq!(sources["result.top_program_03"]["value"], 2.0);
        assert!(!sources.contains_key("result.top_program_04"));
        assert_eq!(sources["result.improvement_002"]["value"], 2.4);
        assert!(!sources.contains_key("result.improvement_003"));
    }

    #[test]
    fn only_ten_programs_are_top_programs() {
        let history: Vec<String> = (1..=12)
            .map(|n| {
                format!(
                    r#"{{"attempt_id":"a{n}","metrics":{{"combined_score":{n}.0,"correct":true}}}}"#
                )
            })
            .collect();
        let data = staged(&[("investigation/attempts_history.jsonl", &history.join("\n"))]);
        assert_eq!(data["top_programs"].as_array().unwrap().len(), 10);
        assert_eq!(data["top_programs"][0]["attempt_id"], "a12");
        assert_eq!(data["improvement_trace"].as_array().unwrap().len(), 12);
        assert_eq!(data["all_programs"].as_array().unwrap().len(), 12);
    }

    #[test]
    fn the_data_names_the_presentation_contract_and_the_evidence_sources() {
        let (_dir, dossier) = run_with_results();
        let data = &dossier.data;
        assert_eq!(
            data["presentation_contract"]["variant_unit"],
            "model_variant"
        );
        assert_eq!(data["presentation_contract"]["run_unit"], "seeded_run");
        assert_eq!(
            data["presentation_contract"]["internal_fields"],
            json!(["correct", "eligible"])
        );
        assert!(
            data["presentation_contract"]["forbidden_visible_terms"]
                .as_array()
                .unwrap()
                .contains(&json!("combined_score"))
        );
        assert_eq!(
            data["evidence_sources"]["investigation"]["status"],
            "included"
        );
        assert_eq!(data["evidence_sources"]["ablations"]["status"], "included");
        assert_eq!(data["evidence_sources"]["posthoc"]["status"], "included");
        assert_eq!(
            data["evidence_sources"]["final_evaluation"]["status"],
            "missing"
        );

        let bare = staged(&[]);
        assert_eq!(
            bare["evidence_sources"]["investigation"]["status"],
            "missing"
        );
        assert_eq!(bare["evidence_sources"]["ablations"]["status"], "missing");
        assert_eq!(bare["top_programs"], json!([]));
        assert_eq!(bare["all_programs"], json!([]));
        assert_eq!(bare["improvement_trace"], json!([]));
    }

    #[test]
    fn a_launch_failure_marks_the_final_evaluation_degraded() {
        let data = staged(&[(
            "final_evaluation/final_launch_failure.json",
            r#"{"reason": "no contract"}"#,
        )]);
        assert_eq!(
            data["evidence_sources"]["final_evaluation"]["status"],
            "degraded"
        );
    }

    #[test]
    fn the_dossier_holds_the_staged_files() {
        let (dir, _) = run_with_results();
        let dossier = dir.path().join("paper/dossier");
        for name in [
            "task.md",
            "best_program/src/program.py",
            "ablations/pipeline_summary.json",
            "posthoc/score_forensics/analysis_summary.md",
            "posthoc/score_forensics/diagnostic.json",
        ] {
            assert!(dossier.join(name).is_file(), "{name}");
        }
    }

    #[test]
    fn staging_twice_gives_the_same_data() {
        let (dir, first) = run_with_results();
        let again = run(dir.path(), &dir.path().join("paper")).unwrap();
        assert_eq!(first.data, again.data);
        let text = std::fs::read_to_string(dir.path().join("paper/paper_data.json")).unwrap();
        assert!(!text.contains("generated_at"));
        assert!(!text.contains(&dir.path().display().to_string()));
    }

    #[test]
    fn a_run_without_results_still_gets_paper_data() {
        let dir = tempfile::tempdir().unwrap();
        let dossier = run(dir.path(), &dir.path().join("paper")).unwrap();
        assert_eq!(dossier.analyses.len(), 0);
        assert_eq!(dossier.data["inputs"]["ablations"]["found"], false);
        assert_eq!(dossier.data["totals"]["evaluated_programs"], 0);
        assert!(dossier.data["grounding_sources"]["result.total_evaluated_programs"].is_object());
    }

    #[test]
    fn an_output_dir_outside_the_run_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "analysis_summary.md", "secret");
        write(
            dir.path(),
            "posthoc/latest_manifest.json",
            &format!(
                r#"{{"analyses": [{{"id": "a", "status": "valid", "outputs": [], "output_dir": "{}"}}]}}"#,
                outside.path().display()
            ),
        );
        let dossier = run(dir.path(), &dir.path().join("paper")).unwrap();
        assert!(dossier.analyses.is_empty());
    }

    #[test]
    fn ranking_orders_the_analyses_and_records_the_judgement() {
        let (dir, _) = run_with_results();
        let paper = dir.path().join("paper");
        apply_ranking(
            &paper,
            &[(
                "score_forensics".into(),
                json!({"rank": 1, "recommended_usage": "main_results"}),
            )],
        )
        .unwrap();
        let data: Value =
            serde_json::from_str(&std::fs::read_to_string(paper.join("paper_data.json")).unwrap())
                .unwrap();
        assert_eq!(data["posthoc_analyses"][0]["rank"], 1);
        assert_eq!(
            data["posthoc_analyses"][0]["recommended_usage"],
            "main_results"
        );
    }

    #[test]
    fn clean_removes_generated_files_and_keeps_others() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["paper.tex", "figures/a.png", "dossier/task.md", "notes.txt"] {
            write(dir.path(), name, "x");
        }
        clean(dir.path()).unwrap();
        assert!(!dir.path().join("paper.tex").exists());
        assert!(!dir.path().join("figures").exists());
        assert!(!dir.path().join("dossier").exists());
        assert!(dir.path().join("notes.txt").exists());
    }
}
