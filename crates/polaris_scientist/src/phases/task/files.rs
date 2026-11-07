//! Contained, symlink-free artifact copying and atomic record writes.

use crate::eval::EvalError;
use serde::Serialize;
use std::{
    fs,
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Atomically replace an artifact within its parent directory.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), EvalError> {
    let parent = path
        .parent()
        .ok_or_else(|| EvalError("artifact has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".artifact-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    Ok(result?)
}
/// Atomically write a JSON record.
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<(), EvalError> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}
/// Copy an artifact tree without following symlinks or copying runtime caches.
pub fn copy_tree(source: &Path, target: &Path) -> Result<(), EvalError> {
    copy(source, target, false)
}
/// Copy an editable tree, restoring owner access without changing frozen inputs.
pub fn copy_editable_tree(source: &Path, target: &Path) -> Result<(), EvalError> {
    copy(source, target, true)
}
fn copy(source: &Path, target: &Path, editable: bool) -> Result<(), EvalError> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return Err(EvalError(format!("refusing symlink: {}", source.display())));
    }
    if meta.is_file() {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target)?;
        if editable {
            owner_writable(target)?;
        }
        return Ok(());
    }
    if !meta.is_dir() {
        return Err(EvalError("unsupported artifact file kind".into()));
    }
    fs::create_dir_all(target)?;
    if editable {
        owner_writable(target)?;
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_str(), Some(".venv" | ".git" | "__pycache__")) {
            continue;
        }
        copy(&entry.path(), &target.join(name), editable)?;
    }
    Ok(())
}
/// Append one JSON line to a durable history.
pub fn append_json(path: &Path, value: &impl Serialize) -> Result<(), EvalError> {
    let mut bytes = if path.exists() {
        fs::read(path)?
    } else {
        Vec::new()
    };
    bytes.extend(serde_json::to_vec(value)?);
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}
/// Reject absolute, parent-traversing, or empty artifact paths.
pub fn safe_relative(raw: &str) -> bool {
    !raw.is_empty()
        && Path::new(raw)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Restore owner access on an editable clone without changing its frozen source.
pub fn owner_writable(path: &Path) -> Result<(), EvalError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(EvalError("refusing writable symlink".into()));
    }
    let mut permissions = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | if meta.is_dir() { 0o700 } else { 0o600 });
    }
    #[cfg(not(unix))]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
    Ok(())
}
