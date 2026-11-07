//! Records which source revision, which `polaris_code` version and which target
//! built the binary, for `--version`.

use std::path::Path;
use std::process::Command;

/// The workspace lock file, relative to this crate's folder.
const LOCK_FILE: &str = "../../Cargo.lock";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    // The lock file names the `polaris_code` version that gets built. Like the git paths
    // below, it is watched only when it exists, because cargo reruns the script on every
    // build for a path it cannot find.
    if Path::new(LOCK_FILE).exists() {
        println!("cargo:rerun-if-changed={LOCK_FILE}");
    }
    println!("cargo:rerun-if-env-changed=POLARIS_SCIENTIST_BUILD_ID");
    // A commit moves the branch file that HEAD points to; HEAD itself stays the same. Only
    // paths that exist are watched, because cargo reruns the script on every build for a
    // path it cannot find.
    let branch = git(&["symbolic-ref", "-q", "HEAD"]);
    for name in ["HEAD"].into_iter().chain(branch.as_deref()) {
        if let Some(path) = git(&["rev-parse", "--git-path", name])
            && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let id = std::env::var("POLARIS_SCIENTIST_BUILD_ID")
        .ok()
        .filter(|id| !id.trim().is_empty())
        .or_else(|| git(&["describe", "--always", "--dirty", "--tags"]))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=POLARIS_SCIENTIST_BUILD_ID={id}");
    println!(
        "cargo:rustc-env=POLARIS_SCIENTIST_CODE_VERSION={}",
        polaris_code_version()
    );
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=POLARIS_SCIENTIST_TARGET={target}");
}

/// The trimmed output of `git <args>`, or `None` when git is missing, the
/// source is not a checkout, or the command fails.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The version of the `polaris_code` this crate is built against, as the
/// workspace lock file records it. "unknown" when the lock file is missing or
/// does not list the package.
fn polaris_code_version() -> String {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    std::fs::read_to_string(Path::new(&manifest).join(LOCK_FILE))
        .ok()
        .and_then(|lock| locked_version(&lock, "polaris_code"))
        .unwrap_or_else(|| "unknown".to_string())
}

/// The `version = "x"` line that follows `name = "<package>"` in a lock file.
fn locked_version(lock: &str, package: &str) -> Option<String> {
    let name_line = format!("name = \"{package}\"");
    let mut lines = lock.lines().map(str::trim);
    lines.find(|line| *line == name_line)?;
    quoted_version(lines.next().filter(|line| line.starts_with("version"))?)
}

/// The text of the first `version = "..."` in `line`.
fn quoted_version(line: &str) -> Option<String> {
    let rest = &line[line.find("version")? + "version".len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}
