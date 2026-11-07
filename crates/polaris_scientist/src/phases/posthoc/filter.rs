//! Artifact-only analysis selection and frozen-source validation.
use crate::{
    eval::{EvalError, fingerprint},
    phases::task::files::{copy_editable_tree, safe_relative},
};
use polaris_cosmos::flow::StepKey;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};
/// Apply selector choices, artifact existence, diversity and analysis count caps.
pub fn select(
    candidates: &[Value],
    decisions: &Value,
    bundle: &Path,
    cap: usize,
) -> (Vec<Value>, Vec<Value>) {
    let decisions = decisions
        .get("decisions")
        .and_then(Value::as_array)
        .or_else(|| decisions.as_array());
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let mut ids = BTreeSet::new();
    let mut titles = BTreeSet::new();
    let mut families = BTreeSet::new();
    for candidate in candidates {
        let id = candidate
            .get("id")
            .or_else(|| candidate.get("analysis_id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let title = candidate
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let family = candidate
            .get("family")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let plan = candidate
            .get("implementation_plan")
            .map(ToString::to_string)
            .unwrap_or_default()
            .to_lowercase();
        let explicitly_rejected = decisions.is_some_and(|ds| {
            ds.iter().any(|d| {
                d.get("id")
                    .or_else(|| d.get("analysis_id"))
                    .and_then(Value::as_str)
                    == Some(id)
                    && d.get("accepted").and_then(Value::as_bool) == Some(false)
            })
        });
        let forbidden = [
            "retrain",
            "train from scratch",
            "fine-tun",
            "finetun",
            "evaluate.py",
            "rerun evaluation",
            "run evaluation",
            "evaluator rerun",
        ];
        let artifacts = rewrite_paths(candidate.get("required_artifacts"), |p| {
            bundle_relative(p, bundle)
        });
        let outputs = rewrite_paths(candidate.get("expected_outputs"), outputs_relative);
        let reason = if !safe_relative(id)
            || id.contains('/')
            || StepKey::new(format!("posthoc/analyses/{id}")).is_err()
        {
            Some("unsafe or missing analysis id")
        } else if title.is_empty() || family.is_empty() {
            Some("missing title or family")
        } else if explicitly_rejected {
            Some("rejected by selector")
        } else if candidate.get("requires_retrain").and_then(Value::as_bool) != Some(false)
            || forbidden.iter().any(|s| plan.contains(s))
            || ["ablation", "perturbation", "retrain", "fine-tun", "finetun"]
                .iter()
                .any(|s| family.contains(s))
        {
            Some("analysis must use existing artifacts without retraining or evaluator reruns")
        } else if artifacts.as_ref().is_none_or(|a| {
            a.iter().any(|p| {
                p.as_str()
                    .is_none_or(|p| !safe_relative(p) || !bundle.join(p).exists())
            })
        }) {
            Some("missing or unsafe required artifact")
        } else if outputs.as_ref().is_none_or(|a| {
            a.is_empty()
                || a.iter()
                    .any(|p| p.as_str().is_none_or(|p| !safe_relative(p)))
        }) {
            Some("missing or unsafe expected output")
        } else if ids.contains(id) || titles.contains(&title) || families.contains(&family) {
            Some("duplicate id, title or family")
        } else if accepted.len() >= cap {
            Some("max_selected_analyses cap reached")
        } else {
            None
        };
        if let Some(reason) = reason {
            rejected.push(json!({"id":id,"reason":reason}));
        } else {
            ids.insert(id.to_owned());
            titles.insert(title);
            families.insert(family);
            let mut accepted_candidate = candidate.clone();
            if let (Some(object), Some(artifacts), Some(outputs)) =
                (accepted_candidate.as_object_mut(), artifacts, outputs)
            {
                object.insert("required_artifacts".into(), Value::Array(artifacts));
                object.insert("expected_outputs".into(), Value::Array(outputs));
            }
            accepted.push(accepted_candidate);
        }
    }
    (accepted, rejected)
}
/// Workspace folder that holds the staged evidence, which a proposal may repeat in a path.
const BUNDLE_PREFIX: &str = "inputs/bundle/";
/// Workspace folder that holds an analysis's outputs, which a proposal may repeat in a path.
const OUTPUTS_PREFIX: &str = "outputs/";
/// Rewrite every string in a list of paths, leaving anything that is not a string unchanged.
fn rewrite_paths(list: Option<&Value>, rewrite: impl Fn(&str) -> String) -> Option<Vec<Value>> {
    let list = list?.as_array()?;
    Some(
        list.iter()
            .map(|path| {
                path.as_str()
                    .map_or_else(|| path.clone(), |path| Value::String(rewrite(path)))
            })
            .collect(),
    )
}
/// Path of a required artifact inside the bundle: a proposal may write the path it sees in the
/// analysis workspace (`inputs/bundle/...`), which only counts when no such file exists in the bundle.
fn bundle_relative(path: &str, bundle: &Path) -> String {
    match path.strip_prefix(BUNDLE_PREFIX) {
        Some(inner) if !bundle.join(path).exists() => inner.to_owned(),
        _ => path.to_owned(),
    }
}
/// Path of an expected output relative to the analysis's `outputs/` folder, as the manifest lists it.
fn outputs_relative(path: &str) -> String {
    path.strip_prefix(OUTPUTS_PREFIX).unwrap_or(path).to_owned()
}
/// Fixed guardrail rubric every artifact-only analysis must satisfy.
pub fn contract(spec: &Value) -> Value {
    json!({"analysis_id":spec.get("id").or_else(||spec.get("analysis_id")),"rubrics":[{"id":"artifact_only","description":"Use retained artifacts only; do not retrain, rerun the evaluator or create variants."}],"expected_outputs":spec["expected_outputs"]})
}
/// Restore a pristine bundle for every analysis attempt, checking the frozen digest.
pub fn fresh(frozen: &Path, expected_digest: &str, workspace: &Path) -> Result<(), EvalError> {
    if fingerprint(frozen)? != expected_digest {
        return Err(EvalError("frozen analysis bundle digest changed".into()));
    }
    if workspace.exists() {
        fs::remove_dir_all(workspace)?;
    }
    copy_editable_tree(frozen, workspace)?;
    if fingerprint(workspace)? != expected_digest {
        return Err(EvalError(
            "analysis copy does not match frozen digest".into(),
        ));
    }
    Ok(())
}
/// Require source integrity, the complete analysis receipt and declared outputs.
pub fn validate(
    bundle: &Path,
    digest: &str,
    outputs: &Path,
    spec: &Value,
    attempt: u32,
    tolerance: f64,
) -> Result<(), EvalError> {
    if fingerprint(bundle)? != digest {
        return Err(EvalError(
            "analysis changed frozen source or evidence".into(),
        ));
    }
    for file in [
        "analysis_summary.md",
        "interestingness_rubric.json",
        "analysis_manifest.json",
        "rubric_status.json",
    ] {
        if !outputs.join(file).is_file() {
            return Err(EvalError(format!("missing analysis output: {file}")));
        }
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(outputs.join("analysis_manifest.json"))?)?;
    if manifest.get("attempt_id").and_then(Value::as_str) != Some(&format!("attempt_{attempt}")) {
        return Err(EvalError("analysis manifest attempt_id mismatch".into()));
    }
    let analysis_id = spec
        .get("id")
        .or_else(|| spec.get("analysis_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| EvalError("analysis id missing".into()))?;
    if manifest.get("analysis_id").and_then(Value::as_str) != Some(analysis_id)
        || manifest.get("grounded").and_then(Value::as_bool) != Some(true)
        || manifest.get("reran_evaluator").and_then(Value::as_bool) != Some(false)
        || manifest
            .get("summary")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
        || manifest
            .get("claim")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err(EvalError(
            "analysis manifest identity or analysis-only guardrail failed".into(),
        ));
    }
    let declared_tolerance = manifest
        .get("primary_metric_tolerance")
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite() && *t >= 0.0)
        .ok_or_else(|| EvalError("analysis tolerance missing or invalid".into()))?;
    if declared_tolerance > tolerance {
        return Err(EvalError(
            "analysis exceeds configured primary metric tolerance".into(),
        ));
    }
    let listed = manifest
        .get("outputs")
        .and_then(Value::as_array)
        .ok_or_else(|| EvalError("analysis manifest outputs missing".into()))?;
    if listed.is_empty() {
        return Err(EvalError("analysis manifest lists no outputs".into()));
    }
    if let Some(bad) = listed.iter().find(|p| {
        p.as_str()
            .is_none_or(|p| !safe_relative(p) || !outputs.join(p).is_file())
    }) {
        return Err(EvalError(format!(
            "analysis manifest output {bad} is missing or unsafe; outputs are paths relative to outputs/"
        )));
    }
    let evidence = manifest
        .get("evidence_paths")
        .and_then(Value::as_array)
        .ok_or_else(|| EvalError("analysis evidence_paths missing".into()))?;
    let workspace = outputs
        .parent()
        .ok_or_else(|| EvalError("analysis outputs has no workspace parent".into()))?;
    if evidence.is_empty() {
        return Err(EvalError(
            "analysis manifest lists no evidence_paths".into(),
        ));
    }
    if let Some(bad) = evidence.iter().find(|p| {
        p.as_str().is_none_or(|p| {
            !safe_relative(p)
                || !(p.starts_with("inputs/bundle/") || p.starts_with("outputs/"))
                || !workspace.join(p).is_file()
        })
    }) {
        return Err(EvalError(format!(
            "analysis evidence path {bad} is missing, unsafe, or not under inputs/bundle/ or outputs/"
        )));
    }
    if manifest
        .get("checkpoint_paths")
        .and_then(Value::as_array)
        .is_none_or(|paths| {
            paths.iter().any(|p| {
                p.as_str()
                    .is_none_or(|p| !safe_relative(p) || !workspace.join(p).is_file())
            })
        })
    {
        return Err(EvalError(
            "analysis checkpoint_paths missing or unsafe".into(),
        ));
    }
    let status: Value = serde_json::from_slice(&fs::read(outputs.join("rubric_status.json"))?)?;
    if status.get("overall_pass").and_then(Value::as_bool) != Some(true) {
        return Err(EvalError("analysis rubrics did not pass".into()));
    }
    if status.get("analysis_id").and_then(Value::as_str) != Some(analysis_id) {
        return Err(EvalError("analysis rubric identity mismatch".into()));
    }
    let rubrics = status
        .get("rubrics")
        .and_then(Value::as_array)
        .ok_or_else(|| EvalError("analysis rubric list missing".into()))?;
    if rubrics.len() != 1
        || rubrics[0].get("id").and_then(Value::as_str) != Some("artifact_only")
        || rubrics[0].get("pass").and_then(Value::as_bool) != Some(true)
        || rubrics[0]
            .get("evidence")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err(EvalError(
            "analysis rubric contract mismatch: the list needs exactly one artifact_only entry with pass true and a non-empty evidence string".into(),
        ));
    }
    let interestingness: Value =
        serde_json::from_slice(&fs::read(outputs.join("interestingness_rubric.json"))?)?;
    if !interestingness.is_object() {
        return Err(EvalError("interestingness rubric must be an object".into()));
    }
    if fs::read_to_string(outputs.join("analysis_summary.md"))?
        .trim()
        .is_empty()
    {
        return Err(EvalError("analysis summary is empty".into()));
    }

    for output in spec
        .get("expected_outputs")
        .and_then(Value::as_array)
        .ok_or_else(|| EvalError("expected_outputs missing".into()))?
    {
        let path = output
            .as_str()
            .ok_or_else(|| EvalError("output path is not a string".into()))?;
        if !safe_relative(path)
            || !outputs.join(path).is_file()
            || !listed.iter().any(|listed| listed.as_str() == Some(path))
        {
            return Err(EvalError(format!(
                "missing declared analysis output: {path}"
            )));
        }
    }
    Ok(())
}
