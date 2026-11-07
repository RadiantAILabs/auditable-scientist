//! Canonical idea contracts and matching ordered rubric arrays.
use crate::eval::EvalError;
use serde_json::{Value, json};
use std::collections::BTreeSet;
/// Filesystem-safe identifier derived from a title or supplied stable id.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if separator && !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if out.is_empty() {
        "idea".into()
    } else {
        out.chars().take(72).collect()
    }
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, EvalError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| EvalError(format!("idea {key} must be a non-empty string")))
}
fn rubrics(value: &Value) -> Result<Vec<Value>, EvalError> {
    let raw = value
        .as_array()
        .or_else(|| value.get("target_rubrics").and_then(Value::as_array))
        .ok_or_else(|| EvalError("target_rubrics must be an array".into()))?;
    if raw.is_empty() || raw.len() > 32 {
        return Err(EvalError(
            "target_rubrics must contain 1 to 32 entries".into(),
        ));
    }
    let mut used = BTreeSet::new();
    let mut normalized = Vec::new();
    for item in raw {
        let description = required(item, "description")?;
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("implementation")
            .trim()
            .to_lowercase();
        if !matches!(kind.as_str(), "implementation" | "metric") {
            return Err(EvalError("unknown rubric type".into()));
        }
        let id = slug(
            item.get("id")
                .and_then(Value::as_str)
                .unwrap_or(description),
        );
        let mut unique = id.clone();
        let mut suffix = 2;
        while !used.insert(unique.clone()) {
            unique = format!("{id}_{suffix}");
            suffix += 1;
        }
        let mut normalized_item = json!({"id":unique,"description":description,"type":kind});
        if let Some(verification) = item.get("verification") {
            if kind != "implementation"
                || verification.get("type").and_then(Value::as_str) != Some("source_contains")
                || !verification
                    .get("patterns")
                    .and_then(Value::as_array)
                    .is_some_and(|p| {
                        !p.is_empty()
                            && p.iter()
                                .all(|p| p.as_str().is_some_and(|s| !s.trim().is_empty()))
                    })
            {
                return Err(EvalError("invalid source_contains verification".into()));
            }
            normalized_item["verification"] = verification.clone();
        }
        normalized.push(normalized_item);
    }
    if !normalized.iter().any(|r| r["type"] == "metric")
        || !normalized.iter().any(|r| r["type"] == "implementation")
    {
        return Err(EvalError(
            "idea requires both metric and implementation rubrics".into(),
        ));
    }
    Ok(normalized)
}
/// Normalize a proposal only if its separate rubric contract agrees exactly.
pub fn normalize(bundle: &Value, targets: &Value, cycle: u32) -> Result<Value, EvalError> {
    let title = required(bundle, "title")?;
    let hypothesis = required(bundle, "hypothesis")?;
    let plan = bundle
        .get("implementation_plan")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| EvalError("implementation_plan must be non-empty".into()))?;
    let plan = plan
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if plan.is_empty() {
        return Err(EvalError(
            "implementation_plan must contain non-empty strings".into(),
        ));
    }
    let target = rubrics(bundle.get("target_rubrics").unwrap_or(&Value::Null))?;
    if target != rubrics(targets)? {
        return Err(EvalError(
            "idea and separate target_rubrics disagree".into(),
        ));
    }
    let id = format!(
        "c{cycle:02}_{}",
        slug(
            bundle
                .get("idea_id")
                .and_then(Value::as_str)
                .unwrap_or(title)
        )
    );
    Ok(
        json!({"idea_id":id,"title":title,"hypothesis":hypothesis,"implementation_plan":plan,"target_rubrics":target,"notes_for_agent":bundle.get("notes_for_agent").and_then(Value::as_str).unwrap_or("")}),
    )
}
