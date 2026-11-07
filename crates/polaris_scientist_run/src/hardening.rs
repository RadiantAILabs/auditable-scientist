//! Process protections applied before any credential is read.

use std::io;
use std::path::PathBuf;

/// Mark the process as not dumpable.
///
/// A process that is not dumpable cannot be attached to by a debugger, writes
/// no core file, and its `/proc/<pid>/environ` and `/proc/<pid>/mem` are owned
/// by root, so another command running as the same user cannot read the
/// credentials the process holds. The setting applies to this process only;
/// the commands it starts are dumpable again.
///
/// Call it first, before the credentials are read.
///
/// # Errors
///
/// The operating system's error if the setting is refused.
pub fn make_undumpable() -> io::Result<()> {
    // SAFETY: `prctl` with `PR_SET_DUMPABLE` takes plain integers and touches
    // no memory.
    let result = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Whether the process is dumpable, as the kernel sees it.
#[must_use]
pub fn is_dumpable() -> bool {
    // SAFETY: `prctl` with `PR_GET_DUMPABLE` takes plain integers and touches
    // no memory.
    unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) == 1 }
}

/// Create the directory `HOME` names if it does not exist, with access for
/// its owner only.
///
/// A container started with a read-only root and `HOME` on a temporary file
/// system has no home until something makes it. Returns the directory, or
/// `None` when `HOME` is unset or empty.
///
/// # Errors
///
/// The error from creating the directory.
pub fn ensure_home() -> io::Result<Option<PathBuf>> {
    let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
        return Ok(None);
    };
    let home = PathBuf::from(home);
    if !home.is_dir() {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&home)?;
    }
    Ok(Some(home))
}
