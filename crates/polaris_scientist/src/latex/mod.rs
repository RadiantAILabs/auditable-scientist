//! Compiling the paper and reading TeX text.
//!
//! [`build`] runs `pdflatex`, `bibtex`, `pdflatex`, `pdflatex` on `paper.tex`
//! in the paper directory. Each program starts in its own process group with a
//! scrubbed environment and without shell escape, and the whole build stops at
//! one deadline. The result carries the end of the TeX log, the first error
//! the log shows, and the page facts that [`pages`] reads from the log.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

pub mod pages;
pub mod text;

pub use pages::PageReport;

/// Base name of the paper's TeX source, log, aux and PDF files.
pub const MAIN: &str = "paper";

/// File name of the wrapper the build writes next to the paper and removes
/// afterwards.
const WRAPPER: &str = "paper.build.tex";

/// Lines of the TeX log kept in [`BuildResult::log_tail`].
const LOG_TAIL_LINES: usize = 60;

/// Upper bound on the characters kept in [`BuildResult::log_tail`].
const LOG_TAIL_CHARS: usize = 6000;

/// Time the build gives a program to exit after it is asked to stop.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Variables a build program inherits from this process. Everything else,
/// including every credential variable, is dropped.
const INHERITED_ENV: [&str; 7] = [
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "DOCKER_HOST",
    "XDG_RUNTIME_DIR",
];

/// Programs and limit used to compile a paper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LatexTools {
    /// The `pdflatex` program (`paper.latex.command`).
    pub pdflatex: String,
    /// The `bibtex` program (`paper.latex.bibtex_command`).
    pub bibtex: String,
    /// Time allowed for the whole build (`paper.latex.timeout_seconds`).
    pub timeout: Duration,
}

impl Default for LatexTools {
    fn default() -> Self {
        Self {
            pdflatex: "pdflatex".to_string(),
            bibtex: "bibtex".to_string(),
            timeout: Duration::from_secs(300),
        }
    }
}

impl LatexTools {
    /// Reads the `paper.latex.*` settings from a resolved configuration table;
    /// a missing or mistyped setting keeps its default.
    #[must_use]
    pub fn from_values(values: &toml::Table) -> Self {
        let default = Self::default();
        let latex = values.get("paper").and_then(|p| p.get("latex"));
        let text = |key: &str, fallback: String| {
            latex
                .and_then(|l| l.get(key))
                .and_then(toml::Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map_or(fallback, str::to_string)
        };
        let seconds = latex
            .and_then(|l| l.get("timeout_seconds"))
            .and_then(toml::Value::as_integer)
            .and_then(|s| u64::try_from(s).ok())
            .filter(|s| *s > 0)
            .map_or(default.timeout, Duration::from_secs);
        Self {
            pdflatex: text("command", default.pdflatex),
            bibtex: text("bibtex_command", default.bibtex),
            timeout: seconds,
        }
    }
}

/// One program run during a build.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Pass {
    /// The program that ran.
    pub program: String,
    /// Exit code; `None` when the program did not start, was stopped, or was
    /// skipped.
    pub exit_code: Option<i32>,
    /// True when the program was left out because the paper has no bibliography.
    pub skipped: bool,
}

/// Outcome of [`build`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildResult {
    /// True when every pass exited 0 and `paper.pdf` exists.
    pub ok: bool,
    /// The compiled PDF, when there is one.
    pub pdf: Option<PathBuf>,
    /// The programs that ran, in order.
    pub passes: Vec<Pass>,
    /// True when the deadline stopped the build.
    pub timed_out: bool,
    /// Why the build failed, in one line; `None` on success.
    pub error: Option<String>,
    /// The end of the TeX log.
    pub log_tail: String,
    /// The first error line of the TeX log with the line that follows it.
    pub first_error: Option<String>,
    /// Page facts from the log, when the log reports them.
    pub pages: Option<PageReport>,
}

/// Compiles `paper.tex` in `dir`.
///
/// Files of an earlier build (`paper.pdf`, `.aux`, `.log`, `.bbl`, `.blg`,
/// `.out`, `.toc`) are removed first, so a failed build never leaves a PDF
/// that looks current.
pub async fn build(dir: &Path, tools: &LatexTools) -> BuildResult {
    for ext in ["pdf", "aux", "log", "bbl", "blg", "out", "toc"] {
        let _ = std::fs::remove_file(dir.join(format!("{MAIN}.{ext}")));
    }
    let wrapper = dir.join(WRAPPER);
    let wrapper_body = format!("{}\\input{{{MAIN}.tex}}\n", pages::PROBE_PRELUDE);
    let mut result = BuildResult::default();
    if let Err(e) = std::fs::write(&wrapper, wrapper_body) {
        result.error = Some(format!("cannot write {WRAPPER}: {e}"));
        return result;
    }
    let deadline = Instant::now() + tools.timeout;
    let tex_args = [
        "-interaction=nonstopmode",
        "-halt-on-error",
        "-file-line-error",
        "-no-shell-escape",
        "-jobname=paper",
        WRAPPER,
    ];

    let mut failed = run_pass(dir, &tools.pdflatex, &tex_args, deadline, &mut result)
        .await
        .is_err();
    if !failed {
        if has_bibliography_data(dir) {
            failed = run_pass(dir, &tools.bibtex, &[MAIN], deadline, &mut result)
                .await
                .is_err();
        } else {
            result.passes.push(Pass {
                program: tools.bibtex.clone(),
                exit_code: None,
                skipped: true,
            });
        }
    }
    for _ in 0..2 {
        if failed {
            break;
        }
        failed = run_pass(dir, &tools.pdflatex, &tex_args, deadline, &mut result)
            .await
            .is_err();
    }
    let _ = std::fs::remove_file(&wrapper);

    let log = std::fs::read(dir.join(format!("{MAIN}.log")))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    result.log_tail = tail(&log);
    result.first_error = first_error(&log);
    result.pages = pages::read_log(&log);
    let pdf = dir.join(format!("{MAIN}.pdf"));
    if pdf.is_file() {
        result.pdf = Some(pdf);
    }
    result.ok = !failed && result.pdf.is_some();
    if !failed && result.pdf.is_none() {
        result.error = Some(format!(
            "{} exited 0 but wrote no {MAIN}.pdf",
            tools.pdflatex
        ));
    }
    result
}

/// Runs one program. A program that fails to start, exits non-zero or is
/// stopped by the deadline records its error in `result` and returns `Err`.
/// bibtex warnings use a non-zero code, so a failing bibtex does not stop the
/// build: the next pdflatex pass reports any resulting problem.
async fn run_pass(
    dir: &Path,
    program: &str,
    args: &[&str],
    deadline: Instant,
    result: &mut BuildResult,
) -> Result<(), ()> {
    let is_bibtex = args == [MAIN];
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(
            INHERITED_ENV
                .iter()
                .filter_map(|k| Some((*k, std::env::var_os(k)?))),
        )
        .env("openin_any", "p")
        .env("openout_any", "p")
        .env("shell_escape", "f")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            result.passes.push(Pass {
                program: program.to_string(),
                exit_code: None,
                skipped: false,
            });
            result.error = Some(format!("cannot start {program}: {e}"));
            return Err(());
        }
    };
    let pid = child.id();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let status = match tokio::time::timeout(remaining, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            if let Some(pid) = pid {
                stop_group(pid).await;
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            result.passes.push(Pass {
                program: program.to_string(),
                exit_code: None,
                skipped: false,
            });
            result.timed_out = true;
            result.error = Some(format!(
                "{program} did not finish before the build time limit"
            ));
            return Err(());
        }
    };
    let code = status.ok().and_then(|s| s.code());
    result.passes.push(Pass {
        program: program.to_string(),
        exit_code: code,
        skipped: false,
    });
    if code == Some(0) || is_bibtex {
        return Ok(());
    }
    result.error = Some(match code {
        Some(code) => format!("{program} exited with code {code}"),
        None => format!("{program} was stopped by a signal"),
    });
    Err(())
}

/// Asks every process of the group started by `pid` to stop, then forces it
/// after a short grace period. Does nothing for a pid that cannot name a group.
async fn stop_group(pid: u32) {
    if pid <= 1 {
        return;
    }
    let target = format!("-{pid}");
    for signal in ["-TERM", "-KILL"] {
        let _ = Command::new("kill")
            .args([signal, "--", &target])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if signal == "-TERM" {
            tokio::time::sleep(STOP_GRACE).await;
        }
    }
}

fn has_bibliography_data(dir: &Path) -> bool {
    std::fs::read(dir.join(format!("{MAIN}.aux")))
        .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains("\\bibdata"))
}

fn tail(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    let start = lines.len().saturating_sub(LOG_TAIL_LINES);
    let joined = lines[start..].join("\n");
    let skip = joined.chars().count().saturating_sub(LOG_TAIL_CHARS);
    joined.chars().skip(skip).collect()
}

fn first_error(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let index = lines.iter().position(|l| {
        l.starts_with('!')
            || l.split_once(':').is_some_and(|(_, rest)| {
                rest.split_once(':')
                    .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
    })?;
    let mut out = lines[index].to_string();
    if let Some(next) = lines[index + 1..]
        .iter()
        .take(3)
        .find(|l| l.starts_with("l."))
    {
        out.push('\n');
        out.push_str(next);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_read_the_latex_settings_and_keep_defaults() {
        let values: toml::Table = toml::from_str(
            "[paper.latex]\ncommand = \"/opt/tex/pdflatex\"\ntimeout_seconds = 42\nbibtex_command = \" \"\n",
        )
        .expect("toml");
        let tools = LatexTools::from_values(&values);
        assert_eq!(tools.pdflatex, "/opt/tex/pdflatex");
        assert_eq!(tools.bibtex, "bibtex");
        assert_eq!(tools.timeout, Duration::from_secs(42));
        assert_eq!(
            LatexTools::from_values(&toml::Table::new()),
            LatexTools::default()
        );
    }

    #[test]
    fn log_tail_is_bounded_and_first_error_carries_its_context_line() {
        let mut log = String::new();
        for n in 0..200 {
            log.push_str(&format!("line {n}\n"));
        }
        log.push_str("./paper.tex:12: Undefined control sequence.\nl.12 \\foo\n");
        assert!(tail(&log).lines().count() <= LOG_TAIL_LINES);
        assert_eq!(
            first_error(&log).as_deref(),
            Some("./paper.tex:12: Undefined control sequence.\nl.12 \\foo")
        );
        assert_eq!(
            first_error("! Missing $ inserted.\nl.3 x_1\n").as_deref(),
            Some("! Missing $ inserted.\nl.3 x_1")
        );
        assert_eq!(first_error("all fine\n"), None);
    }
}
