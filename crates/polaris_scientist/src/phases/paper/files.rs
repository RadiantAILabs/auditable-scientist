//! Moving files between the folders of agent calls and the paper folder.
//!
//! An agent writes into the `outputs/` folder of its own workspace. The phase
//! then promotes those files into the paper folder: regular files only, no
//! links, within fixed bounds, and never the files the phase's own systems
//! write. Nothing an agent produced is trusted to stay where it was put.

use std::path::{Component, Path, PathBuf};

use polaris_cosmos::flow::{StepKey, latest_workspace};
use serde_json::Value;

/// Most files one promotion copies.
const MAX_FILES: usize = 400;
/// Most bytes one promotion copies.
const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Top-level names the phase's systems own. An agent's copy of them is ignored.
pub const OWNED: [&str; 13] = [
    "paper_data.json",
    "references.bib",
    "references_provenance.json",
    "validation_report.json",
    "paper_manifest.json",
    "paper_evidence_audit.json",
    "paper_evidence_audit.md",
    "claims_check.json",
    "ai_review_status.json",
    "paper.pdf",
    "dossier",
    "rounds",
    "audit_response.txt",
];

/// Extensions of files a TeX run leaves behind; the phase's own build makes them.
const BUILD_EXTENSIONS: [&str; 9] = [
    "aux",
    "log",
    "bbl",
    "blg",
    "out",
    "toc",
    "fls",
    "fdb_latexmk",
    "gz",
];

/// Writes `value` as indented JSON with a final newline.
///
/// # Errors
///
/// Returns the I/O error from writing the file.
pub fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    let mut body = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    body.push('\n');
    crate::durable::atomic_write(path, body.as_bytes()).map_err(std::io::Error::other)
}

/// The `outputs/` folder of the latest workspace of a step, when the step ran.
#[must_use]
pub fn outputs_of(root: &Path, step: &StepKey) -> Option<PathBuf> {
    let workspace = latest_workspace(root, step).ok().flatten()?;
    let outputs = workspace.join("outputs");
    outputs.is_dir().then_some(outputs)
}

/// The workspace folder of the latest run of a step, when there is one.
#[must_use]
pub fn workspace_of(root: &Path, step: &StepKey) -> Option<PathBuf> {
    latest_workspace(root, step).ok().flatten()
}

fn is_build_product(relative: &Path) -> bool {
    relative
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| BUILD_EXTENSIONS.contains(&e))
        && relative
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().starts_with("paper"))
}

/// Copies every regular file under `outputs` into `paper_dir`, keeping
/// relative paths, and returns the relative names copied in sorted order.
///
/// Names in [`OWNED`] at the top level and TeX build products are left out.
/// Links and special files are refused.
///
/// # Errors
///
/// Returns a sentence when a link or special file is found, a bound is
/// exceeded, or a copy fails.
pub fn promote_all(outputs: &Path, paper_dir: &Path) -> Result<Vec<String>, String> {
    let mut copied = Vec::new();
    let mut bytes = 0_u64;
    walk(outputs, outputs, paper_dir, &mut copied, &mut bytes)?;
    copied.sort();
    Ok(copied)
}

fn walk(
    base: &Path,
    dir: &Path,
    paper_dir: &Path,
    copied: &mut Vec<String>,
    bytes: &mut u64,
) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .map_err(|e| e.to_string())?
            .to_path_buf();
        let kind = entry
            .file_type()
            .map_err(|e| format!("cannot inspect {}: {e}", relative.display()))?;
        if kind.is_symlink() {
            return Err(format!(
                "{} is a symbolic link; the agent may only leave regular files",
                relative.display()
            ));
        }
        let top = relative
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or_default();
        if relative.components().count() >= 1 && OWNED.contains(&top) {
            continue;
        }
        if kind.is_dir() {
            walk(base, &path, paper_dir, copied, bytes)?;
        } else if kind.is_file() {
            if is_build_product(&relative) {
                continue;
            }
            if relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(format!(
                    "{} is not a plain relative path",
                    relative.display()
                ));
            }
            let size = entry.metadata().map_err(|e| e.to_string())?.len();
            *bytes += size;
            if copied.len() >= MAX_FILES || *bytes > MAX_BYTES {
                return Err("the agent left more files than the paper folder accepts".into());
            }
            let target = paper_dir.join(&relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            std::fs::copy(&path, &target)
                .map_err(|e| format!("cannot copy {}: {e}", relative.display()))?;
            copied.push(relative.to_string_lossy().replace('\\', "/"));
        } else {
            return Err(format!("{} is not a regular file", relative.display()));
        }
    }
    Ok(())
}

/// Top-level names of the manuscript that an agent's copy replaces as a whole.
const MANUSCRIPT: [&str; 4] = [
    "paper.tex",
    "claims_grounding.json",
    "figure_manifest.json",
    "figures",
];

/// Promotes the manuscript an agent wrote.
///
/// When `outputs` holds a `paper.tex`, the manuscript files already in
/// `paper_dir` are removed first, so a file the agent dropped does not
/// survive. Without a `paper.tex` the files are added to what is there.
///
/// # Errors
///
/// Returns the sentence of [`promote_all`], or one for a failed removal.
pub fn promote_manuscript(outputs: &Path, paper_dir: &Path) -> Result<Vec<String>, String> {
    if outputs.join("paper.tex").is_file() {
        for name in MANUSCRIPT {
            let path = paper_dir.join(name);
            let result = match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
                Ok(_) => std::fs::remove_file(&path),
                Err(_) => Ok(()),
            };
            result.map_err(|e| format!("cannot replace {name}: {e}"))?;
        }
    }
    promote_all(outputs, paper_dir)
}

/// Copies the named top-level files of `outputs` into `paper_dir`. A name that
/// is missing is skipped; the rest are returned.
///
/// # Errors
///
/// Returns a sentence when a named file is a link or not a regular file, or a
/// copy fails.
pub fn promote_named(
    outputs: &Path,
    paper_dir: &Path,
    names: &[&str],
) -> Result<Vec<String>, String> {
    let mut copied = Vec::new();
    for name in names {
        let source = outputs.join(name);
        match std::fs::symlink_metadata(&source) {
            Err(_) => continue,
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(format!("{name} is not a regular file")),
        }
        std::fs::copy(&source, paper_dir.join(name))
            .map_err(|e| format!("cannot copy {name}: {e}"))?;
        copied.push((*name).to_string());
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, name: &str, text: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn promotion_copies_files_and_skips_owned_names_and_build_products() {
        let outputs = tempfile::tempdir().unwrap();
        let paper = tempfile::tempdir().unwrap();
        for name in [
            "paper.tex",
            "figures/a.png",
            "sections/intro.tex",
            "references.bib",
            "paper_data.json",
            "paper.pdf",
            "paper.aux",
            "paper.log",
            "dossier/task.md",
        ] {
            write(outputs.path(), name, "x");
        }
        let copied = promote_all(outputs.path(), paper.path()).unwrap();
        assert_eq!(copied, ["figures/a.png", "paper.tex", "sections/intro.tex"]);
        assert!(paper.path().join("sections/intro.tex").is_file());
        assert!(!paper.path().join("references.bib").exists());
        assert!(!paper.path().join("paper.pdf").exists());
    }

    #[test]
    fn a_link_in_the_outputs_is_refused() {
        let outputs = tempfile::tempdir().unwrap();
        let paper = tempfile::tempdir().unwrap();
        write(outputs.path(), "paper.tex", "x");
        std::os::unix::fs::symlink("/etc/passwd", outputs.path().join("link.tex")).unwrap();
        let error = promote_all(outputs.path(), paper.path()).unwrap_err();
        assert!(error.contains("link.tex"), "{error}");
        assert!(error.contains("symbolic link"), "{error}");
    }

    #[test]
    fn named_promotion_skips_missing_files() {
        let outputs = tempfile::tempdir().unwrap();
        let paper = tempfile::tempdir().unwrap();
        write(outputs.path(), "a.json", "1");
        let copied = promote_named(outputs.path(), paper.path(), &["a.json", "b.json"]).unwrap();
        assert_eq!(copied, ["a.json"]);
    }
}
