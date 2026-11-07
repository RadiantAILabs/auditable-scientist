//! Reproducible task-local Python runtime, provisioned from wheels first.

use super::files::write_json;
use crate::eval::{EvalError, file_digest};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::process::Command;

/// Verified task runtime, including exact dependency versions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeRecord {
    /// Task-local interpreter path.
    pub python: PathBuf,
    /// Interpreter version from Python itself.
    pub interpreter_version: String,
    /// Exact required package versions.
    pub pins: BTreeMap<String, String>,
    /// Actual installed versions of required packages.
    pub installed: BTreeMap<String, String>,
    /// Versions of the extra analysis packages the runtime provides; a package
    /// the wheelhouse could not supply is absent.
    #[serde(default)]
    pub extras: BTreeMap<String, String>,
    /// Immutable scorer digest.
    pub evaluator_sha256: String,
}
/// Parse exact package pins; floating requirements and pip directives are refused.
pub fn parse_pins(text: &str) -> Result<BTreeMap<String, String>, EvalError> {
    let mut pins = BTreeMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (name, version) = line
            .split_once("==")
            .ok_or_else(|| EvalError(format!("dependency is not exactly pinned: {line}")))?;
        if name.is_empty()
            || version.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            || !version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".+-_!".contains(c))
        {
            return Err(EvalError(format!("invalid dependency pin: {line}")));
        }
        if pins
            .insert(name.to_lowercase().replace('_', "-"), version.into())
            .is_some()
        {
            return Err(EvalError(format!("duplicate dependency pin: {name}")));
        }
    }
    Ok(pins)
}

async fn execute(
    python: &Path,
    args: &[String],
    cwd: &Path,
) -> Result<std::process::Output, EvalError> {
    let mut command = Command::new(python);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .kill_on_drop(true)
        .process_group(0);
    let child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| EvalError("runtime process has no PID".into()))?;
    struct Group(u32);
    impl Drop for Group {
        fn drop(&mut self) {
            if self.0 > 1 {
                /* SAFETY: this is the spawned child's process group. */
                unsafe {
                    libc::kill(-(self.0 as i32), libc::SIGKILL);
                }
            }
        }
    }
    let _group = Group(pid);
    let output = tokio::time::timeout(Duration::from_secs(600), child.wait_with_output())
        .await
        .map_err(|_| EvalError("task runtime command timed out".into()))??;
    Ok(output)
}

/// Create or revalidate the task environment, installing offline before fallback.
///
/// Only a missing wheel permits the package index fallback. An existing runtime
/// is checked against the pins every time; mismatches fail without reinstalling.
pub async fn provision(
    task: &Path,
    bootstrap_python: &Path,
    wheelhouse: &Path,
) -> Result<RuntimeRecord, EvalError> {
    provision_with_extras(task, bootstrap_python, wheelhouse, &[]).await
}

/// Check that an extra package name is a plain distribution name, so that it
/// cannot be read as a pip option.
fn valid_extra(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// Installed versions of the named packages in the task runtime; a package that
/// is not installed is left out.
async fn installed_versions(
    python: &Path,
    task: &Path,
    names: &[String],
) -> Result<BTreeMap<String, String>, EvalError> {
    let script = "import importlib.metadata as m, json, sys\nfound = {}\nfor name in json.loads(sys.argv[1]):\n    try:\n        found[name] = m.version(name)\n    except m.PackageNotFoundError:\n        pass\nprint(json.dumps(found))";
    let output = execute(
        python,
        &[
            "-I".into(),
            "-c".into(),
            script.into(),
            serde_json::to_string(names)?,
        ],
        task,
    )
    .await?;
    if !output.status.success() {
        return Err(EvalError(format!(
            "runtime version check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Make the extra analysis packages available in the task runtime.
///
/// A package that is missing is installed from the wheelhouse alone, with the
/// task's pins as constraints so that it can never change a pinned version.
/// There is no index fallback: a package the wheelhouse does not hold is left
/// out and logged, because the task's own results do not depend on it.
async fn provide_extras(
    task: &Path,
    python: &Path,
    wheelhouse: &Path,
    extras: &[String],
) -> Result<BTreeMap<String, String>, EvalError> {
    if let Some(bad) = extras.iter().find(|name| !valid_extra(name)) {
        return Err(EvalError(format!("invalid extra package name: {bad}")));
    }
    if extras.is_empty() {
        return Ok(BTreeMap::new());
    }
    let present = installed_versions(python, task, extras).await?;
    let missing: Vec<String> = extras
        .iter()
        .filter(|name| !present.contains_key(*name))
        .cloned()
        .collect();
    if missing.is_empty() {
        return Ok(present);
    }
    // One package at a time: pip installs nothing when any requirement in a
    // call cannot be met, and one absent package must not hold back the others.
    for name in &missing {
        let args = vec![
            "-m".into(),
            "pip".into(),
            "install".into(),
            "--disable-pip-version-check".into(),
            "--no-index".into(),
            "--find-links".into(),
            wheelhouse.to_string_lossy().into_owned(),
            "-c".into(),
            task.join("requirements.txt").to_string_lossy().into_owned(),
            name.clone(),
        ];
        let installed = execute(python, &args, task).await?;
        if !installed.status.success() {
            tracing::warn!(
                package = %name,
                error = %String::from_utf8_lossy(&installed.stderr).trim(),
                "extra analysis package could not be installed from the wheelhouse"
            );
        }
    }
    installed_versions(python, task, extras).await
}

/// Create or revalidate the task environment, as [`provision`] does, and make
/// the extra analysis packages available in it.
///
/// The extras are installed from the wheelhouse only, under the task's pins, and
/// are recorded in the returned [`RuntimeRecord`]. They are installed whether the
/// runtime is new or already exists, so a runtime made before a package was added
/// to the list gains it on the next call.
pub async fn provision_with_extras(
    task: &Path,
    bootstrap_python: &Path,
    wheelhouse: &Path,
    extras: &[String],
) -> Result<RuntimeRecord, EvalError> {
    let task = task.canonicalize()?;
    let receipt = task.join("snapshot.json");
    if receipt.is_file() {
        let snapshot: serde_json::Value = serde_json::from_slice(&fs::read(receipt)?)?;
        for (field, name) in [
            ("evaluator_sha256", "evaluate.py"),
            ("requirements_sha256", "requirements.txt"),
            ("task_md_sha256", "task.md"),
            ("capture_sha256", "capture.py"),
        ] {
            if let Some(expected) = snapshot.get(field).and_then(serde_json::Value::as_str)
                && file_digest(&task.join(name))? != expected
            {
                return Err(EvalError(format!("task snapshot changed: {name}")));
            }
        }
        if let Some(expected) = snapshot
            .get("program_0_sha256")
            .and_then(serde_json::Value::as_str)
            && crate::eval::fingerprint(&task.join("program_0"))? != expected
        {
            return Err(EvalError("task snapshot changed: program_0".into()));
        }
    }
    let pins = parse_pins(&fs::read_to_string(task.join("requirements.txt"))?)?;
    let python = task.join(".venv/bin/python");
    let marker = task.join(".venv/.scientist-managed");
    if python.exists() && !marker.is_file() {
        return Err(EvalError(
            "task runtime is not managed by this application".into(),
        ));
    }
    if !python.exists() {
        if task.join(".venv").exists() {
            return Err(EvalError(
                "task .venv exists without a Python interpreter".into(),
            ));
        }
        let args = vec![
            "-m".into(),
            "venv".into(),
            task.join(".venv").to_string_lossy().into_owned(),
        ];
        let created = execute(bootstrap_python, &args, &task).await?;
        if !created.status.success() {
            return Err(EvalError(format!(
                "venv creation failed: {}",
                String::from_utf8_lossy(&created.stderr)
            )));
        }
        fs::write(&marker, b"managed task runtime\n")?;
        if !pins.is_empty() {
            let args = vec![
                "-m".into(),
                "pip".into(),
                "install".into(),
                "--disable-pip-version-check".into(),
                "--no-index".into(),
                "--find-links".into(),
                wheelhouse.to_string_lossy().into_owned(),
                "-r".into(),
                task.join("requirements.txt").to_string_lossy().into_owned(),
            ];
            let offline = execute(&python, &args, &task).await?;
            if !offline.status.success() {
                let error = String::from_utf8_lossy(&offline.stderr);
                if !error.contains("No matching distribution found")
                    && !error.contains("Could not find a version that satisfies")
                {
                    return Err(EvalError(format!(
                        "wheelhouse installation failed: {error}"
                    )));
                }
                let online = execute(
                    &python,
                    &[
                        "-m".into(),
                        "pip".into(),
                        "install".into(),
                        "--disable-pip-version-check".into(),
                        "-r".into(),
                        task.join("requirements.txt").to_string_lossy().into_owned(),
                    ],
                    &task,
                )
                .await?;
                if !online.status.success() {
                    return Err(EvalError(format!(
                        "dependency installation failed: {}",
                        String::from_utf8_lossy(&online.stderr)
                    )));
                }
            }
        }
    }
    let script = "import importlib.metadata,json,sys; pins=json.loads(sys.argv[1]); print(json.dumps({'version':sys.version,'installed':{name:importlib.metadata.version(name) for name in pins}}))";
    let output = execute(
        &python,
        &[
            "-I".into(),
            "-c".into(),
            script.into(),
            serde_json::to_string(&pins)?,
        ],
        &task,
    )
    .await?;
    if !output.status.success() {
        return Err(EvalError(format!(
            "runtime version check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let installed: BTreeMap<String, String> = serde_json::from_value(actual["installed"].clone())?;
    if installed != pins {
        return Err(EvalError(format!(
            "task dependency pin mismatch: expected {pins:?}, installed {installed:?}"
        )));
    }
    let extras = provide_extras(&task, &python, wheelhouse, extras).await?;
    let record = RuntimeRecord {
        python,
        interpreter_version: actual["version"].as_str().unwrap_or("").into(),
        pins,
        installed,
        extras,
        evaluator_sha256: file_digest(&task.join("evaluate.py"))?,
    };
    write_json(&task.join("runtime.json"), &record)?;
    Ok(record)
}
