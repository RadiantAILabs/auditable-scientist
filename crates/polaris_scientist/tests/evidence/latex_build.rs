//! The TeX build against stand-in `pdflatex` and `bibtex` programs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use polaris_scientist::latex::{LatexTools, build};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/paper")
        .join(name)
        .display()
        .to_string()
}

fn tools(timeout: Duration) -> LatexTools {
    LatexTools {
        pdflatex: fixture("fake_pdflatex.sh"),
        bibtex: fixture("fake_bibtex.sh"),
        timeout,
    }
}

fn paper(markers: &str, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("paper.tex"),
        format!("{markers}\n\\documentclass{{article}}\n{body}\n"),
    )
    .expect("write paper");
    dir
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("fake_calls.txt"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.split_whitespace().next().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn a_paper_with_a_bibliography_runs_four_passes_and_reports_pages() {
    let dir = paper(
        "% fake: pages 11\n% fake: refs 9 300 400",
        "\\bibliography{references}\n\\appendix",
    );
    let result = build(dir.path(), &tools(Duration::from_secs(30))).await;
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        calls(dir.path()),
        ["pdflatex", "bibtex", "pdflatex", "pdflatex"]
    );
    assert_eq!(result.pdf, Some(dir.path().join("paper.pdf")));
    let pages = result.pages.expect("pages");
    assert_eq!(pages.total_pages, 11);
    assert_eq!(pages.reference_start_page, Some(9));
    assert_eq!(pages.body_pages, Some(9));
    assert_eq!(pages.appendix_start_page, Some(11));
    assert!(result.log_tail.contains("Output written on paper.pdf"));
    assert_eq!(result.first_error, None);
    assert!(
        !dir.path().join("paper.build.tex").exists(),
        "the wrapper is removed"
    );
}

#[tokio::test]
async fn the_first_pass_is_given_the_safe_arguments() {
    let dir = paper("", "\\bibliography{references}");
    build(dir.path(), &tools(Duration::from_secs(30))).await;
    let first = std::fs::read_to_string(dir.path().join("fake_calls.txt")).expect("calls");
    let first = first.lines().next().expect("a call");
    for flag in [
        "-no-shell-escape",
        "-halt-on-error",
        "-interaction=nonstopmode",
        "-jobname=paper",
        "paper.build.tex",
    ] {
        assert!(first.contains(flag), "{flag} missing from {first}");
    }
}

#[tokio::test]
async fn a_paper_without_a_bibliography_skips_bibtex() {
    let dir = paper("", "no citations here");
    let result = build(dir.path(), &tools(Duration::from_secs(30))).await;
    assert!(result.ok);
    assert_eq!(calls(dir.path()), ["pdflatex", "pdflatex", "pdflatex"]);
    assert!(result.passes.iter().any(|p| p.skipped));
}

#[tokio::test]
async fn a_tex_error_stops_the_build_and_is_reported() {
    let dir = paper("% fake: fail", "\\bibliography{references}");
    let result = build(dir.path(), &tools(Duration::from_secs(30))).await;
    assert!(!result.ok);
    assert_eq!(result.pdf, None);
    assert_eq!(calls(dir.path()), ["pdflatex"]);
    assert_eq!(
        result.first_error.as_deref(),
        Some("./paper.tex:7: Undefined control sequence.\nl.7 \\nonexistent")
    );
    assert!(result.error.expect("error").contains("exited with code 1"));
}

#[tokio::test]
async fn a_stale_pdf_does_not_survive_a_failed_build() {
    let dir = paper("% fake: fail", "x");
    std::fs::write(dir.path().join("paper.pdf"), "old").expect("write");
    let result = build(dir.path(), &tools(Duration::from_secs(30))).await;
    assert!(!result.ok);
    assert!(!dir.path().join("paper.pdf").exists());
}

#[tokio::test]
async fn a_build_that_writes_no_pdf_is_not_ok() {
    let dir = paper("% fake: nopdf", "x");
    let result = build(dir.path(), &tools(Duration::from_secs(30))).await;
    assert!(!result.ok);
    assert!(result.error.expect("error").contains("wrote no paper.pdf"));
}

#[tokio::test]
async fn the_programs_see_a_scrubbed_environment() {
    let dir = paper("", "x");
    build(dir.path(), &tools(Duration::from_secs(30))).await;
    let names: Vec<String> = std::fs::read_to_string(dir.path().join("fake_env.txt"))
        .expect("env")
        .lines()
        .map(str::to_string)
        .collect();
    let allowed = [
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "DOCKER_HOST",
        "XDG_RUNTIME_DIR",
        "openin_any",
        "openout_any",
        "shell_escape",
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
    ];
    let stray: Vec<&String> = names
        .iter()
        .filter(|n| !allowed.contains(&n.as_str()))
        .collect();
    assert!(stray.is_empty(), "unexpected variables: {stray:?}");
    for required in ["openin_any", "openout_any", "shell_escape"] {
        assert!(names.iter().any(|n| n == required), "{required} not set");
    }
}

#[tokio::test]
async fn the_deadline_stops_the_program_and_its_children() {
    let dir = paper("% fake: hang", "x");
    let started = std::time::Instant::now();
    let result = build(dir.path(), &tools(Duration::from_millis(500))).await;
    assert!(result.timed_out);
    assert!(!result.ok);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the build waited for the program"
    );
    let pid: u32 = std::fs::read_to_string(dir.path().join("fake_child.pid"))
        .expect("child pid")
        .trim()
        .parse()
        .expect("pid number");
    let proc = PathBuf::from(format!("/proc/{pid}"));
    // The child is gone, or only a zombie that nothing has collected yet.
    let state = std::fs::read_to_string(proc.join("stat")).unwrap_or_default();
    assert!(
        state.is_empty() || state.split(") ").nth(1).is_some_and(|s| s.starts_with('Z')),
        "child {pid} is still running: {state}"
    );
}

#[tokio::test]
async fn a_missing_program_is_reported_without_a_panic() {
    let dir = paper("", "x");
    let tools = LatexTools {
        pdflatex: "/nonexistent/pdflatex".to_string(),
        ..tools(Duration::from_secs(5))
    };
    let result = build(dir.path(), &tools).await;
    assert!(!result.ok);
    assert!(result.error.expect("error").contains("cannot start"));
}
