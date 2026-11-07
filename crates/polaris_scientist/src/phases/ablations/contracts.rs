//! Trusted variant rubrics and mechanical analysis checks.
//!
//! The rubric families and CSV columns follow AI-Scientist-v3's
//! `src/variants_pipeline/{constants,rubric_contract}.py` at df83138.
use crate::eval::{self, EvalError, EvalResult};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

const SCHEMAS: [(&str, &[&str]); 3] = [
    (
        "run_summary.csv",
        &[
            "evaluation_id",
            "variant_id",
            "variant_kind",
            "base_program",
            "agent_name",
            "correct",
            "combined_score",
            "overall_pass",
            "primary_metric_name",
            "primary_metric_value",
            "program_dir",
            "results_dir",
        ],
    ),
    (
        "metric_values.csv",
        &[
            "evaluation_id",
            "variant_id",
            "metric_name",
            "metric_value",
            "is_primary_metric",
        ],
    ),
    (
        "timeseries.csv",
        &[
            "evaluation_id",
            "variant_id",
            "series_name",
            "step",
            "value",
        ],
    ),
];
const RUBRICS: [(&str, &str); 9] = [
    (
        "analysis_files_present",
        "All required analysis files (run_summary.csv, metric_values.csv, timeseries.csv) exist under analysis/.",
    ),
    (
        "run_summary_schema_valid",
        "run_summary.csv exists and contains all required columns.",
    ),
    (
        "metric_values_schema_valid",
        "metric_values.csv exists and contains all required columns.",
    ),
    (
        "timeseries_schema_valid",
        "timeseries.csv exists and contains all required columns, even if the file contains only headers.",
    ),
    (
        "metrics_consistent_with_metrics_json",
        "combined_score and primary metric in run_summary.csv agree with the values in metrics.json produced by the evaluator.",
    ),
    (
        "correctness_recorded",
        "The 'correct' field is present in run_summary.csv and matches correct.json.",
    ),
    (
        "variant_intent_preserved",
        "The implementation matches the declared baseline/ablation intent described in variant_spec.json.",
    ),
    (
        "single_change_scope",
        "For ablations and extra baselines, only the declared comparison axis was changed relative to the control.",
    ),
    (
        "paper_analysis_ready",
        "Analysis files contain the declared evaluator metrics, correctness, and variant identity. State which evidence is available and missing, including per-seed observations, geometry, runtime, and timeseries. A header-only timeseries is valid; do not require undeclared diagnostics or infer support for claims whose evidence is unavailable.",
    ),
];

/// The complete trusted contract; proposal-supplied rubric IDs cannot replace it.
pub fn targets(spec: &Value) -> Vec<Value> {
    let mut rubrics = RUBRICS.iter().map(|(id, description)| json!({"id":id,"description":description,"type":if id.contains("schema") || id.contains("files") {"structural"}else{"semantic"}})).collect::<Vec<_>>();
    if spec["base_program"] == "reference" && spec["variant_id"] != "baseline_reference" {
        for (id, description) in [
            (
                "hyperparameter_audit_complete",
                "A hyperparameter_comparison.md lists all reference and control hyperparameters, categorizing every difference as align or preserve with reasoning.",
            ),
            (
                "reference_conditions_aligned",
                "Experimental conditions are aligned with the control outside the reference's core contribution.",
            ),
            (
                "reference_performance_check",
                "Compare adapted performance with documented reference performance; investigate large degradation and revert unnecessary adaptations.",
            ),
        ] {
            rubrics.push(json!({"id":id,"description":description,"type":"semantic"}));
        }
    }
    rubrics
}

/// Standard analysis schemas rendered for the instrumentation role.
pub fn csv_schemas() -> String {
    SCHEMAS
        .iter()
        .map(|(name, columns)| format!("{name}: {}", columns.join(",")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reject a candidate whose executable source is identical to the control.
/// Documentation and scorer artifacts do not establish an intervention.
pub fn require_source_change(control: &Path, candidate: &Path) -> Result<(), EvalError> {
    fn source(program: &Path) -> Result<BTreeMap<std::path::PathBuf, String>, EvalError> {
        let spec: toml::Value = fs::read_to_string(program.join("program_spec.toml"))?
            .parse()
            .map_err(|e: toml::de::Error| EvalError(e.to_string()))?;
        let relative = spec
            .get("code_root")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| EvalError("missing code_root".into()))?;
        if relative != "." && !super::files::safe_relative(relative) {
            return Err(EvalError("unsafe code_root".into()));
        }
        let entrypoint = eval::entrypoint(program)?;
        let code = program.join(relative);
        let mut sources = BTreeMap::new();
        let mut bytes = 0;
        for entry in walkdir::WalkDir::new(&code) {
            let entry = entry.map_err(|e| EvalError(e.to_string()))?;
            if entry.file_type().is_symlink() {
                return Err(EvalError("symlink in candidate source".into()));
            }
            if entry.file_type().is_file()
                && (entry.path().extension().is_some_and(|e| e == "py")
                    || entry.path().canonicalize()? == entrypoint)
            {
                bytes += entry
                    .metadata()
                    .map_err(|e| EvalError(e.to_string()))?
                    .len();
                if bytes > 4 * 1024 * 1024 || sources.len() >= 128 {
                    return Err(EvalError("candidate source exceeds size limit".into()));
                }
                let name = entry
                    .path()
                    .strip_prefix(&code)
                    .map_err(|e| EvalError(e.to_string()))?
                    .to_path_buf();
                sources.insert(name, eval::file_digest(entry.path())?);
            }
        }
        if sources.is_empty() {
            return Err(EvalError("candidate has no executable source".into()));
        }
        Ok(sources)
    }
    if source(control)? == source(candidate)? {
        return Err(EvalError("variant source is unchanged from control; implement the declared comparison axis before evaluation".into()));
    }
    Ok(())
}

type Rows = Vec<BTreeMap<String, String>>;
fn table(path: &Path, columns: &[&str]) -> Result<Rows, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("analysis CSV must be an ordinary file".into());
    }
    if meta.len() > 8 * 1024 * 1024 {
        return Err("analysis CSV exceeds size limit".into());
    }
    let mut reader = csv::Reader::from_path(path).map_err(|e| e.to_string())?;
    let headers = reader.headers().map_err(|e| e.to_string())?.clone();
    if columns
        .iter()
        .any(|c| headers.iter().filter(|h| h == c).count() != 1)
    {
        return Err(format!(
            "{} missing or duplicate required columns",
            path.display()
        ));
    }
    reader
        .records()
        .map(|row| {
            row.map(|r| {
                headers
                    .iter()
                    .zip(r.iter())
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect()
            })
            .map_err(|e| e.to_string())
        })
        .collect()
}
fn boolean(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}
fn same_number(value: Option<&String>, expected: Option<f64>) -> bool {
    value
        .and_then(|v| v.parse::<f64>().ok())
        .zip(expected)
        .is_some_and(|(v, e)| {
            v.is_finite() && e.is_finite() && (v - e).abs() <= 1e-9 * e.abs().max(1.0)
        })
}
/// Host assessments for file presence, CSV schemas, metric agreement and correctness.
/// Semantic intent and scope remain the verifier role's responsibility.
pub fn mechanical(program: &Path, evaluation: &EvalResult) -> Result<Vec<Value>, EvalError> {
    let entry = eval::entrypoint(program)?;
    let analysis = [
        program.join("results/analysis"),
        program.join("analysis"),
        entry
            .parent()
            .ok_or_else(|| EvalError("entrypoint parent missing".into()))?
            .join("analysis"),
    ]
    .into_iter()
    .find(|p| p.is_dir())
    .unwrap_or_else(|| program.join("analysis"));
    if analysis.exists() {
        for path in analysis.ancestors().take_while(|p| *p != program) {
            if fs::symlink_metadata(path)?.file_type().is_symlink() {
                return Err(EvalError("symlink in analysis path".into()));
            }
        }
        if !analysis
            .canonicalize()?
            .starts_with(program.canonicalize()?)
        {
            return Err(EvalError("analysis path escapes candidate".into()));
        }
    }
    let tables = SCHEMAS
        .iter()
        .map(|(name, columns)| table(&analysis.join(name), columns))
        .collect::<Vec<_>>();
    let present = SCHEMAS
        .iter()
        .all(|(name, _)| analysis.join(name).is_file());
    let mut checks = vec![
        json!({"id":"analysis_files_present","pass":present,"evidence":format!("Required CSV files under {}: {present}",analysis.display())}),
    ];
    for (index, id) in [
        "run_summary_schema_valid",
        "metric_values_schema_valid",
        "timeseries_schema_valid",
    ]
    .into_iter()
    .enumerate()
    {
        checks.push(json!({"id":id,"pass":tables[index].is_ok(),"evidence":tables[index].as_ref().map(|rows|format!("Required columns present; {} data rows",rows.len())).unwrap_or_else(Clone::clone)}));
    }
    let rows = tables[0].as_ref().ok().filter(|rows| !rows.is_empty());
    let score = evaluation.metrics["combined_score"].as_f64();
    let metrics_ok = rows.is_some_and(|rows| {
        rows.iter().all(|r| {
            same_number(r.get("combined_score"), score)
                && r.get("primary_metric_name").is_some_and(|name| {
                    // A name the evaluator does not report (such as the quantity the score sums)
                    // can only stand for the score it reports.
                    let expected = match evaluation.metrics.get(name) {
                        Some(value) => value.as_f64(),
                        None => score,
                    };
                    same_number(r.get("primary_metric_value"), expected)
                })
        })
    });
    let correct_ok = rows.is_some_and(|rows| {
        rows.iter().all(|r| {
            r.get("correct").and_then(|s| boolean(s)) == evaluation.correctness["correct"].as_bool()
        })
    });
    checks.push(json!({"id":"metrics_consistent_with_metrics_json","pass":metrics_ok,"evidence":format!("Every run summary score and named primary metric agrees with evaluator metrics.json: {metrics_ok}")}));
    checks.push(json!({"id":"correctness_recorded","pass":correct_ok,"evidence":format!("Every run summary correctness matches evaluator correct.json: {correct_ok}")}));
    Ok(checks)
}
