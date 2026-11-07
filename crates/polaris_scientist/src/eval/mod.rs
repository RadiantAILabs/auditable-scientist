//! Immutable-candidate evaluation through the task's unchanged evaluator.
//!
//! Each invocation owns a process group, starts with an empty environment, and
//! records candidate and evaluator fingerprints. Existing matching results are
//! adopted; partial or mismatched results are never silently scored again.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;

mod sandbox;

/// A request to score one immutable candidate.
#[derive(Clone, Debug)]
pub struct EvalRequest {
    /// Task-local interpreter, normally `task/.venv/bin/python`.
    pub python: PathBuf,
    /// The task's unmodified evaluator.
    pub evaluator: PathBuf,
    /// Candidate folder containing `program_spec.toml`.
    pub program_dir: PathBuf,
    /// External evaluator output folder.
    pub results_dir: PathBuf,
    /// Evaluator limit, 300 seconds for the shipped task.
    pub timeout: Duration,
    /// Additional cleanup grace, normally five seconds.
    pub grace: Duration,
    /// Explicit non-secret task variables, such as `NUM_REPLICATIONS`.
    pub environment: BTreeMap<String, String>,
}

/// Recorded scorer output and integrity evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalResult {
    /// Scorer-authored metrics, preserved without interpretation.
    pub metrics: Value,
    /// Scorer-authored correctness record.
    pub correctness: Value,
    /// Invocation and integrity record.
    pub manifest: EvaluationManifest,
    /// Whether existing matching results were adopted.
    pub reused: bool,
}
impl EvalResult {
    /// A usable score must be correct, finite and numeric (never boolean).
    pub fn score(&self) -> Option<f64> {
        (self.correctness.get("correct").and_then(Value::as_bool) == Some(true))
            .then(|| self.metrics.get("combined_score").and_then(Value::as_f64))
            .flatten()
            .filter(|score| score.is_finite())
    }
}

/// Durable integrity record accompanying the evaluator's files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvaluationManifest {
    /// Candidate digest before invocation.
    pub program_sha256: String,
    /// Evaluator digest before invocation.
    pub evaluator_sha256: String,
    /// Evaluator digest after invocation.
    pub evaluator_fingerprint_after: String,
    /// Whether both immutable inputs retained their original fingerprints.
    pub integrity_valid: bool,
    /// Unix epoch seconds at launch.
    pub started: u64,
    /// Unix epoch seconds at completion.
    pub ended: u64,
    /// Exit status, absent on signal termination.
    pub exit_code: Option<i32>,
    /// Whether the outer deadline expired.
    pub timed_out: bool,
    /// Candidate digest before invocation.
    pub candidate_fingerprint_before: String,
    /// Candidate digest after invocation.
    pub candidate_fingerprint_after: String,
}

/// Failure to launch, validate, or safely complete an evaluation.
#[derive(Debug)]
pub struct EvalError(pub String);
impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for EvalError {}
impl From<io::Error> for EvalError {
    fn from(e: io::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<serde_json::Error> for EvalError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}

/// SHA-256 of one file.
pub fn file_digest(path: &Path) -> Result<String, EvalError> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

/// Names a retrofitted program writes next to its own code each time it is scored: the standard
/// result file and the folder of CSV files that the ablation protocol asks every program to emit.
const PROGRAM_OUTPUTS: [&str; 2] = ["_results.json", "analysis"];

/// The folder that holds a program's code, from its `program_spec.toml`, when the spec is readable.
fn code_dir(program: &Path) -> Option<PathBuf> {
    let spec: toml::Value = fs::read_to_string(program.join("program_spec.toml"))
        .ok()?
        .parse()
        .ok()?;
    Some(program.join(spec.get("code_root")?.as_str()?))
}

/// Hash a tree's relative names and bytes, rejecting symlinks.
///
/// Transient Python caches and evaluator-owned result files are excluded. So are the files a
/// program writes about its own run (`_results.json` and `analysis/`) when they sit in the
/// program folder or in its code folder: they are outputs of scoring, not part of what is scored.
pub fn fingerprint(root: &Path) -> Result<String, EvalError> {
    fn visit(
        root: &Path,
        code: Option<&Path>,
        path: &Path,
        hash: &mut Sha256,
    ) -> Result<(), EvalError> {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        let outputs_here = path == root || code == Some(path);
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if (path == root && name == "results")
                || (outputs_here && PROGRAM_OUTPUTS.contains(&name.as_ref()))
                || matches!(name.as_ref(), "__pycache__" | ".venv" | ".git")
                || name.ends_with(".pyc")
            {
                continue;
            }
            let p = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(EvalError(format!("symlink in candidate: {}", p.display())));
            }
            let relative = p.strip_prefix(root).map_err(|e| EvalError(e.to_string()))?;
            let bytes = relative.as_os_str().as_encoded_bytes();
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
            if kind.is_dir() {
                hash.update(b"directory");
                visit(root, code, &p, hash)?;
            } else if kind.is_file() {
                let data = fs::read(p)?;
                hash.update((data.len() as u64).to_le_bytes());
                hash.update(data);
            } else {
                return Err(EvalError("non-regular candidate file".into()));
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    visit(root, code_dir(root).as_deref(), root, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

/// Resolve and contain a Python entrypoint declared by `program_spec.toml`.
pub fn entrypoint(program: &Path) -> Result<PathBuf, EvalError> {
    let spec: toml::Value = fs::read_to_string(program.join("program_spec.toml"))?
        .parse()
        .map_err(|e: toml::de::Error| EvalError(e.to_string()))?;
    let code = spec
        .get("code_root")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| EvalError("missing code_root".into()))?;
    let entry = spec
        .get("entrypoint")
        .and_then(|e| e.get("path"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| EvalError("missing entrypoint.path".into()))?;
    let root = program.canonicalize()?;
    let path = root.join(code).join(entry).canonicalize()?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(EvalError(
            "entrypoint escapes candidate or is not a file".into(),
        ));
    }
    Ok(path)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn read_json(path: &Path) -> Result<Value, EvalError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
struct ProcessGroup(i32);
impl ProcessGroup {
    fn terminate(&self) {
        if self.0 > 1 {
            // SAFETY: this is the newly spawned supervisor's process group.
            unsafe {
                libc::kill(-self.0, libc::SIGTERM);
            }
        }
    }
    fn kill(&mut self) {
        if self.0 > 1 {
            // SAFETY: a newly spawned child owns this positive process-group id.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
            self.0 = 0;
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

async fn cleanup(
    child: &mut tokio::process::Child,
    group: &mut ProcessGroup,
) -> Result<std::process::ExitStatus, io::Error> {
    group.terminate();
    match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
        Ok(result) => result,
        Err(_) => {
            group.kill();
            child.wait().await
        }
    }
}

/// Run the unchanged evaluator once, with a scrubbed environment and group timeout.
///
/// A Linux subreaper supervises the unchanged scorer. Cancellation and timeout
/// terminate its group, then allow one second to reap descendants before a hard kill. A timeout or integrity failure
/// leaves the manifest as evidence and returns an error, never a usable score.
pub async fn run_evaluator(req: EvalRequest) -> Result<EvalResult, EvalError> {
    evaluate(req, None).await
}

/// Score an advisory candidate in a Linux sandbox with no sockets and with
/// filesystem writes restricted to `scratch` and Python's runtime devices
/// (`/dev/shm` semaphores and `/dev/null`). The unchanged evaluator and task
/// remain outside that folder. Refuse to launch if isolation is unavailable.
pub async fn test_evaluator(req: EvalRequest, scratch: &Path) -> Result<EvalResult, EvalError> {
    let scratch = scratch.canonicalize()?;
    fs::create_dir_all(&req.results_dir)?;
    if req.evaluator.canonicalize()?.starts_with(&scratch)
        || !req.program_dir.canonicalize()?.starts_with(&scratch)
        || !req.results_dir.canonicalize()?.starts_with(&scratch)
    {
        return Err(EvalError(
            "test inputs and outputs violate sandbox containment".into(),
        ));
    }
    evaluate(req, Some(scratch)).await
}

async fn evaluate(req: EvalRequest, scratch: Option<PathBuf>) -> Result<EvalResult, EvalError> {
    let program = req.program_dir.canonicalize()?;
    let evaluator = req.evaluator.canonicalize()?;
    let entry = entrypoint(&program)?;
    let before = fingerprint(&program)?;
    let eval_before = file_digest(&evaluator)?;
    fs::create_dir_all(&req.results_dir)?;
    let results = req.results_dir.canonicalize()?;
    if results == program || results.starts_with(&program) && results != program.join("results") {
        return Err(EvalError(
            "evaluator outputs must be external or in the reserved program/results directory"
                .into(),
        ));
    }

    let lock_path = results.join(".evaluation.lock");
    let _lock_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|e| EvalError(format!("evaluation already active or locked: {e}")))?;
    let _lock = Lock(lock_path);
    let manifest_path = results.join("evaluation_manifest.json");
    if manifest_path.exists() {
        let manifest: EvaluationManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
        if manifest.program_sha256 != before
            || manifest.evaluator_sha256 != eval_before
            || manifest.candidate_fingerprint_after != before
            || !manifest.integrity_valid
            || manifest.timed_out
            || manifest.exit_code != Some(0)
        {
            return Err(EvalError(
                "existing evaluation is failed or fingerprint mismatched".into(),
            ));
        }
        return Ok(EvalResult {
            metrics: read_json(&results.join("metrics.json"))?,
            correctness: read_json(&results.join("correct.json"))?,
            manifest,
            reused: true,
        });
    }
    if results.join("metrics.json").exists()
        || results.join("correct.json").exists()
        || results.join("invocation.json").exists()
    {
        return Err(EvalError(
            "program already has results or an unfinished invocation".into(),
        ));
    }
    // Never give caller-supplied variables an opportunity to load code or expose credentials.
    for key in req.environment.keys() {
        if !matches!(
            key.as_str(),
            "NUM_REPLICATIONS"
                | "VARIANT_ID"
                | "VARIANT_KIND"
                | "BASE_PROGRAM"
                | "EVALUATION_ID"
                | "AGENT_NAME"
        ) {
            return Err(EvalError(format!(
                "unsupported evaluator environment variable: {key}"
            )));
        }
    }
    let started = now();
    fs::write(
        results.join("invocation.json"),
        serde_json::to_vec(
            &serde_json::json!({"started":started,"program_sha256":before,"evaluator_sha256":eval_before}),
        )?,
    )?;
    let mut command = Command::new(&req.python);
    command
        .args(["-I", "-c", include_str!("supervisor.py")])
        .arg(results.join("supervisor.json"))
        .arg(&evaluator)
        .arg("--program_path")
        .arg(entry)
        .arg("--results_dir")
        .arg(&results)
        .arg("--quiet")
        .current_dir(
            evaluator
                .parent()
                .ok_or_else(|| EvalError("evaluator has no parent".into()))?,
        )
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .envs(req.environment)
        .stdin(std::process::Stdio::null())
        .stdout(fs::File::create(results.join("wrapper_stdout.log"))?)
        .stderr(fs::File::create(results.join("wrapper_stderr.log"))?)
        .kill_on_drop(true)
        .process_group(0);
    if let Some(scratch) = scratch {
        command
            .env("TMPDIR", &scratch)
            .env("TEMP", &scratch)
            .env("TMP", &scratch);
        sandbox::restrict(&mut command, &scratch)?;
    }
    let mut child = command.spawn()?;
    let mut group = ProcessGroup(
        child
            .id()
            .ok_or_else(|| EvalError("missing evaluator PID".into()))? as i32,
    );
    let deadline = req.timeout.saturating_add(req.grace);
    let (mut sender, receiver) = tokio::sync::oneshot::channel();
    let _watcher = tokio::spawn(async move {
        let outcome = tokio::select! {
            waited=tokio::time::timeout(deadline,child.wait())=>match waited {
                Ok(result)=>result.map(|status|(status,false)),
                Err(_)=>cleanup(&mut child,&mut group).await.map(|status|(status,true)),
            },
            _=sender.closed()=>{let _=cleanup(&mut child,&mut group).await;return;}
        };
        group.kill();
        let _ = sender.send(outcome);
    });
    let (status, timed_out) = receiver
        .await
        .map_err(|_| EvalError("evaluator supervisor stopped without a result".into()))??;
    let exit_code = match read_json(&results.join("supervisor.json"))
        .ok()
        .and_then(|value| value.get("returncode").and_then(Value::as_i64))
    {
        Some(code) if code < 0 => None,
        Some(code) => i32::try_from(code).ok(),
        None => status.code(),
    };
    let after = fingerprint(&program)?;
    let eval_after = file_digest(&evaluator)?;
    let manifest = EvaluationManifest {
        program_sha256: before.clone(),
        evaluator_sha256: eval_before.clone(),
        evaluator_fingerprint_after: eval_after.clone(),
        integrity_valid: before == after && eval_before == eval_after,
        started,
        ended: now(),
        exit_code,
        timed_out,
        candidate_fingerprint_before: before.clone(),
        candidate_fingerprint_after: after.clone(),
    };
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    if before != after {
        return Err(EvalError("candidate changed during evaluation".into()));
    }
    if eval_before != eval_after {
        return Err(EvalError("evaluator changed during evaluation".into()));
    }
    if timed_out {
        return Err(EvalError(
            "evaluator timed out; process group killed".into(),
        ));
    }
    if !status.success() {
        return Err(EvalError(format!("evaluator exited {status}")));
    }
    Ok(EvalResult {
        metrics: read_json(&results.join("metrics.json"))?,
        correctness: read_json(&results.join("correct.json"))?,
        manifest,
        reused: false,
    })
}

impl From<EvalError> for polaris::system::system::SystemError {
    fn from(error: EvalError) -> Self {
        Self::ExecutionError(error.to_string())
    }
}

impl From<polaris_cosmos::flow::FlowError> for EvalError {
    fn from(error: polaris_cosmos::flow::FlowError) -> Self {
        Self(error.to_string())
    }
}
