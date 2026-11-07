//! Mechanical rubric evaluation over actual scores and executable Python tokens.

use crate::eval::EvalError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

/// One rubric's canonical assessment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RubricAssessment {
    /// Stable identifier from the accepted idea.
    pub id: String,
    /// `passed`, `failed`, or `not_assessed`.
    pub status: String,
    /// Mechanical evidence or the reason no assessment was possible.
    pub evidence: String,
}
/// Canonical report independent of candidate promotion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RubricReport {
    /// Assessments in contract order.
    pub rubrics: Vec<RubricAssessment>,
    /// True only when every rubric was assessed and passed.
    pub overall_pass: bool,
}

fn sources(root: &Path, output: &mut Vec<String>, bytes: &mut usize) -> Result<(), EvalError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(EvalError("symlink in rubric source".into()));
        }
        if kind.is_dir() {
            if entry.file_name() != "__pycache__" {
                sources(&entry.path(), output, bytes)?;
            }
        } else if entry.path().extension().is_some_and(|e| e == "py") {
            let source = fs::read_to_string(entry.path())?;
            *bytes += source.len();
            if output.len() >= 128 || *bytes > 4 * 1024 * 1024 {
                return Err(EvalError("rubric source size limit exceeded".into()));
            }
            output.push(source);
        }
    }
    Ok(())
}

/// Evaluate mechanical rubrics without importing or executing candidate source.
///
/// The Python parser is the upstream token/AST checker. Comments and docstrings
/// cannot satisfy `source_contains`; semantic assertions remain unassessed.
pub async fn mechanical(
    python: &Path,
    program: &Path,
    rubrics: &[Value],
    metrics: &Value,
    correct: bool,
) -> Result<RubricReport, EvalError> {
    let mut texts = Vec::new();
    sources(&program.join("src"), &mut texts, &mut 0)?;
    let payload = serde_json::to_vec(
        &serde_json::json!({"sources":texts,"rubrics":rubrics,"metrics":metrics,"correct":correct}),
    )?;
    let driver = r#"
import json,sys
payload=json.load(sys.stdin)
tokens=candidate_source_tokens(payload['sources'])
results=[]
for rubric in payload['rubrics']:
    status='not_assessed'; evidence='requires semantic judgement'
    if not payload['correct']:
        evidence='candidate is not correct'
    else:
        check=rubric_metric_check(rubric)
        verification=rubric.get('verification',{})
        if check:
            metric,comparator,threshold=check
            value=_numeric_value(payload['metrics'].get(metric))
            if value is not None:
                status='passed' if COMPARATORS[comparator](value,threshold) else 'failed'
                evidence=f'{metric}={value} {comparator} {threshold}'
            else:
                evidence='metric missing or non-numeric'
        elif verification.get('kind')=='source_contains' or verification.get('type')=='source_contains':
            patterns=verification.get('patterns',verification.get('tokens',[]))
            if isinstance(patterns,list) and patterns and all(isinstance(p,str) and p for p in patterns):
                missing=missing_source_patterns(tokens,patterns)
                status='failed' if missing else 'passed'
                evidence='missing executable patterns: '+repr(missing) if missing else 'all executable patterns present'
    results.append({'id':rubric['id'],'status':status,'evidence':evidence})
print(json.dumps({'rubrics':results,'overall_pass':bool(results) and all(r['status']=='passed' for r in results)}))
"#;
    let script = format!("{}\n{driver}", include_str!("rubric_checks.py"));
    let mut child = Command::new(python)
        .args(["-I", "-c", &script])
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| EvalError("rubric checker stdin missing".into()))?
        .write_all(&payload)
        .await?;
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .map_err(|_| EvalError("rubric parser timed out".into()))??;
    if !output.status.success() {
        return Err(EvalError(format!(
            "rubric parser failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Merge a semantic judge's findings only for previously unassessed rubrics.
pub fn merge_judgement(report: &mut RubricReport, judgement: &Value) -> Result<(), EvalError> {
    let entries = judgement
        .get("rubrics")
        .and_then(Value::as_array)
        .ok_or_else(|| EvalError("judge output missing rubrics".into()))?;
    for rubric in &mut report.rubrics {
        if rubric.status != "not_assessed" {
            continue;
        }
        let entry = entries
            .iter()
            .find(|r| r.get("id").and_then(Value::as_str) == Some(&rubric.id))
            .ok_or_else(|| EvalError(format!("judge omitted rubric {}", rubric.id)))?;
        let passed = entry
            .get("pass")
            .and_then(Value::as_bool)
            .ok_or_else(|| EvalError("judge pass must be boolean".into()))?;
        let evidence = entry
            .get("evidence")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| EvalError("judge evidence missing".into()))?;
        rubric.status = if passed { "passed" } else { "failed" }.into();
        rubric.evidence = evidence.into();
    }
    report.overall_pass =
        !report.rubrics.is_empty() && report.rubrics.iter().all(|r| r.status == "passed");
    Ok(())
}
