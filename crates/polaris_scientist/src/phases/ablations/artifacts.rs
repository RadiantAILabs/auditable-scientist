//! Frozen winner extraction and sequential variant artifacts.
use crate::{
    eval::{self, EvalError},
    phases::{
        investigation::{calls::read_json, selection::Incumbent},
        task::files::{self, safe_relative},
    },
};
use polaris_cosmos::flow::StepKey;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Whether `id` can name a variant: a plain folder name that is also valid
/// inside the variant's step key. A name made only of digits is not.
fn usable_id(id: &str) -> bool {
    safe_relative(id)
        && !id.contains('/')
        && StepKey::new(format!("ablations/variants/{id}")).is_ok()
}

/// Materialize selected program, its score evidence, the seed and task scaffold.
pub fn extract(root: &Path, phase: &str) -> Result<PathBuf, EvalError> {
    let bundle = root.join(phase).join("_extracted_bundle");
    if bundle.join("extraction_manifest.json").exists() {
        return Ok(bundle);
    }
    let incumbent: Incumbent = serde_json::from_slice(&fs::read(
        root.join("investigation/best_program_manifest.json"),
    )?)?;
    files::copy_tree(&incumbent.program_dir, &bundle.join("program"))?;
    if !bundle.join("program/results/metrics.json").exists() {
        let results = if incumbent.source_attempt_id == "baseline" {
            root.join("task/baseline")
        } else {
            incumbent.program_dir.join("results")
        };
        files::copy_tree(&results, &bundle.join("program/results"))?;
    }
    files::copy_tree(
        &root.join("task/program_0"),
        &bundle.join("reference_program"),
    )?;
    files::copy_tree(&root.join("task"), &bundle.join("task"))?;
    let best = fs::read_to_string(eval::entrypoint(&bundle.join("program"))?)?;
    let seed = fs::read_to_string(eval::entrypoint(&bundle.join("reference_program"))?)?;
    files::atomic_write(
        &bundle.join("program.diff"),
        similar::TextDiff::from_lines(&seed, &best)
            .unified_diff()
            .header("reference", "best")
            .to_string()
            .as_bytes(),
    )?;
    files::write_json(
        &bundle.join("extraction_manifest.json"),
        &json!({"program_dir":incumbent.program_dir,"source_attempt_id":incumbent.source_attempt_id,"combined_score":incumbent.combined_score,"program_sha256":eval::fingerprint(&bundle.join("program"))?,"evaluator_sha256":eval::file_digest(&bundle.join("task/evaluate.py"))?}),
    )?;
    Ok(bundle)
}
/// Verify that instrumentation preserved the canonical scorer and valid entrypoint.
pub fn check_retrofit(
    canonical_evaluator: &Path,
    before: &str,
    staged_bundle: &Path,
    program: &Path,
) -> Result<Value, EvalError> {
    let after = eval::file_digest(canonical_evaluator)?;
    let staged = staged_bundle.join("task/evaluate.py");
    if after != before || staged.exists() && eval::file_digest(&staged)? != before {
        return Err(EvalError("retrofit changed evaluator".into()));
    }
    eval::entrypoint(program)?;
    Ok(
        json!({"evaluator_sha256_before":before,"evaluator_sha256_after":after,"retrofitted_program_sha256":eval::fingerprint(program)?}),
    )
}
/// Apply selector decisions and separate ablation/baseline caps, retaining order.
pub fn selected(
    candidates: &[Value],
    selection: &Value,
    max_ablations: usize,
    max_baselines: usize,
) -> Vec<Value> {
    let decisions = selection
        .get("decisions")
        .and_then(Value::as_array)
        .or_else(|| selection.as_array());
    let mut counts = BTreeMap::new();
    let mut accepted = Vec::new();
    for candidate in candidates {
        let id = candidate
            .get("variant_id")
            .or_else(|| candidate.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !usable_id(id) || matches!(id, "control_best" | "baseline_reference") {
            continue;
        }
        let kind = candidate
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("ablation");
        if !matches!(kind, "ablation" | "baseline") {
            continue;
        }
        if decisions.is_some_and(|ds| {
            ds.iter().any(|d| {
                d.get("variant_id")
                    .or_else(|| d.get("id"))
                    .and_then(Value::as_str)
                    == Some(id)
                    && d.get("accepted").and_then(Value::as_bool) == Some(false)
            })
        }) {
            continue;
        }
        let count = counts.entry(kind).or_insert(0usize);
        let cap = if kind == "baseline" {
            max_baselines
        } else {
            max_ablations
        };
        if *count >= cap
            || accepted.iter().any(|v: &Value| {
                v.get("variant_id")
                    .or_else(|| v.get("id"))
                    .and_then(Value::as_str)
                    == Some(id)
            })
        {
            continue;
        }
        *count += 1;
        accepted.push(candidate.clone());
    }
    accepted
}
/// Materialize immutable seed inputs and rubric contracts for a variant.
pub fn materialize(root: &Path, spec: &Value, source: &Path) -> Result<PathBuf, EvalError> {
    let id = spec
        .get("variant_id")
        .or_else(|| spec.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| EvalError("variant_id missing".into()))?;
    if !usable_id(id) {
        return Err(EvalError("unsafe variant id".into()));
    }
    let directory = root.join("ablations/variants").join(id);
    if !directory.join("task_bundle/program_0").exists() {
        files::copy_tree(source, &directory.join("task_bundle/program_0"))?;
        remove_results(&directory.join("task_bundle/program_0"))?;
    }
    let targets = super::contracts::targets(spec);
    let mut canonical_spec = spec.clone();
    canonical_spec["target_rubrics"] = json!(targets);
    files::write_json(&directory.join("variant_spec.json"), &canonical_spec)?;
    files::write_json(&directory.join("target_rubrics.json"), &targets)?;
    files::write_json(
        &directory.join("rubric_contract.json"),
        &json!({"contract_version":1,"variant_id":id,"variant_kind":spec["kind"],"rubrics":targets}),
    )?;
    Ok(directory)
}
/// Remove evaluator-owned artifacts when cloning an immutable seed.
pub fn remove_results(program: &Path) -> Result<(), EvalError> {
    for name in ["results", "analysis"] {
        let p = program.join(name);
        if p.is_dir() {
            fs::remove_dir_all(p)?;
        }
    }
    for name in [
        "metrics.json",
        "correct.json",
        "evaluation_manifest.json",
        "rubric_status.json",
        "stdout.log",
        "stderr.log",
    ] {
        let p = program.join(name);
        if p.is_file() {
            fs::remove_file(p)?;
        }
    }
    Ok(())
}
/// Canonical verifier output cannot change host identities, scores, or rubric ids.
pub fn verifier_pass(targets: &[Value], output: &Value, correct: bool) -> Result<bool, EvalError> {
    if output
        .get("next_recommendation")
        .is_some_and(|r| r.as_str().is_none_or(|s| s.len() > 4096))
    {
        return Err(EvalError(
            "variant verifier recommendation must be a bounded string".into(),
        ));
    }
    let entries = output
        .get("rubrics")
        .and_then(Value::as_array)
        .ok_or_else(|| EvalError("variant verifier omitted rubrics".into()))?;
    if entries.len() != targets.len() {
        return Err(EvalError("variant verifier rubric count mismatch".into()));
    }
    let mut all = correct && !targets.is_empty();
    for target in targets {
        let id = target
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| EvalError("target rubric id missing".into()))?;
        let matching = entries
            .iter()
            .filter(|r| r.get("id").and_then(Value::as_str) == Some(id))
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(EvalError(format!(
                "variant verifier missing or duplicated rubric: {id}"
            )));
        }
        let entry = matching[0];
        let passed = entry
            .get("pass")
            .and_then(Value::as_bool)
            .ok_or_else(|| EvalError("variant verifier pass must be boolean".into()))?;
        if entry
            .get("evidence")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty() || s.len() > 4096)
        {
            return Err(EvalError("variant verifier evidence missing".into()));
        }
        all &= passed;
    }
    Ok(all)
}
/// Aggregate retained CSVs with explicit variant, evaluation and source attribution.
pub fn aggregate(root: &Path, variants: &[Value]) -> Result<(), EvalError> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record(["variant", "evaluation", "source", "row"])
        .map_err(|e| EvalError(e.to_string()))?;
    fn visit(
        path: &Path,
        root: &Path,
        id: &str,
        writer: &mut csv::Writer<Vec<u8>>,
    ) -> Result<(), EvalError> {
        if !path.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(EvalError("symlink in analysis CSV tree".into()));
            }
            if kind.is_dir() {
                visit(&entry.path(), root, id, writer)?;
            } else if entry.path().extension().is_some_and(|e| e == "csv") {
                let source = entry.path();
                let source = source
                    .strip_prefix(root)
                    .map_err(|e| EvalError(e.to_string()))?
                    .to_string_lossy();
                let evaluation = source
                    .split('/')
                    .find(|s| s.starts_with("program_"))
                    .unwrap_or("unknown");
                for (index, line) in fs::read_to_string(entry.path())?.lines().enumerate() {
                    writer
                        .write_record([id, evaluation, &source, &format!("{index}:{line}")])
                        .map_err(|e| EvalError(e.to_string()))?;
                }
            }
        }
        Ok(())
    }
    for variant in variants {
        if let Some(id) = variant.get("id").and_then(Value::as_str) {
            visit(
                &root.join("ablations/variants").join(id),
                root,
                id,
                &mut writer,
            )?;
        }
    }
    let bytes = writer.into_inner().map_err(|e| EvalError(e.to_string()))?;
    files::atomic_write(
        &root.join("ablations/aggregated/circpack/analysis.csv"),
        &bytes,
    )
}
/// Read a candidate bundle, preserving missing-output fallback and JSON errors.
pub fn proposals(path: &Path, key: &str) -> Result<Vec<Value>, EvalError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = read_json(path)?;
    Ok(raw
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}
