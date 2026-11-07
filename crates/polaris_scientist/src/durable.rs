//! Small file helpers for records that must survive a crash: atomic writes,
//! JSON records, relative-path checks, UTC timestamps and read-only snapshots.
use polaris_cosmos::flow::{FlowError, FlowResult};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP: AtomicU64 = AtomicU64::new(0);

/// Writes a file by rename from a unique sibling, syncing its contents first.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> FlowResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    Ok(())
}

/// Atomically writes a JSON record.
pub fn write_json(path: &Path, value: &impl Serialize) -> FlowResult<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}

/// Reads a JSON record.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> FlowResult<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// Rejects absolute paths, traversal, empty names and platform prefixes.
pub fn relative(path: &str) -> FlowResult<&Path> {
    let p = Path::new(path);
    if path.is_empty()
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || !p.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(FlowError::invalid(format!("unsafe relative path: {path}")));
    }
    Ok(p)
}

/// UTC timestamp with milliseconds, without a clock or formatting dependency.
pub fn timestamp() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let days = (ms / 86_400_000) as i64;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    let seconds = ms / 1000;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60,
        ms % 1000
    )
}

/// Copies a file or directory tree without following symbolic links.
///
/// With `read_only` set, every copied entry is made read-only; otherwise every
/// copied entry is made writable by its owner.
pub fn copy_snapshot(source: &Path, dest: &Path, read_only: bool) -> FlowResult<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return Err(FlowError::invalid(format!(
            "symlink input refused: {}",
            source.display()
        )));
    }
    if meta.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_snapshot(&entry.path(), &dest.join(entry.file_name()), read_only)?;
        }
    } else if meta.is_file() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, dest)?;
    } else {
        return Err(FlowError::invalid("special file refused during staging"));
    }
    let mut permissions = fs::metadata(dest)?.permissions();
    if read_only {
        permissions.set_readonly(true);
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() | if meta.is_dir() { 0o700 } else { 0o600 });
        }
        #[cfg(not(unix))]
        permissions.set_readonly(false);
    }
    fs::set_permissions(dest, permissions)?;
    Ok(())
}
