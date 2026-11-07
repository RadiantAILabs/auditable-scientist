//! Page facts read from a TeX log.
//!
//! The build wraps the paper in a small file that makes TeX print one
//! `PSPROBE` line where the bibliography and the appendix begin. Reading them
//! needs no PDF tool: whatever runs `pdflatex` also produces them. The total
//! page count comes from the log's `Output written on` line.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// A page position is "at the top of the page" below this many points.
const EMPTY_PAGE_POINTS: f64 = 2.0;

/// Page goal TeX reports for a page that holds nothing yet (`\maxdimen`).
const UNSET_GOAL_POINTS: f64 = 16_000.0;

/// What the log says about the pages of the compiled paper.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageReport {
    /// Pages in the PDF.
    pub total_pages: u32,
    /// Page where the bibliography begins, when the paper has one.
    pub reference_start_page: Option<u32>,
    /// Page where `\appendix` is called, when the paper calls it.
    pub appendix_start_page: Option<u32>,
    /// Pages before the bibliography, counting the page it starts on when
    /// text precedes it there. `None` when the bibliography was not found.
    pub body_pages: Option<u32>,
    /// How full the last body page is, from 0 to 1. A bibliography that starts
    /// at the top of a page counts the page before it as full.
    pub last_body_page_fill_ratio: Option<f64>,
}

#[derive(Clone, Copy)]
struct Mark {
    page: u32,
    total: f64,
    goal: f64,
}

impl Mark {
    fn at_page_top(self) -> bool {
        self.total < EMPTY_PAGE_POINTS || self.goal > UNSET_GOAL_POINTS
    }
}

/// TeX code that prints the `PSPROBE` lines; the build puts it in front of the
/// paper.
pub const PROBE_PRELUDE: &str = "\\makeatletter\n\
\\AtBeginDocument{%\n\
\\AddToHook{env/thebibliography/begin}{\\par\\typeout{PSPROBE refs page=\\thepage; total=\\the\\pagetotal; goal=\\the\\pagegoal;}}%\n\
\\AddToHook{cmd/appendix/before}{\\typeout{PSPROBE appendix page=\\thepage; total=\\the\\pagetotal; goal=\\the\\pagegoal;}}%\n\
}\n\
\\makeatother\n";

/// Reads the page facts from the text of a TeX log; `None` when the log does
/// not say how many pages were written.
#[must_use]
pub fn read_log(log: &str) -> Option<PageReport> {
    static OUTPUT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"Output written on .*?\((\d+) pages?").expect("output pattern is valid")
    });
    static PROBE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"PSPROBE (refs|appendix) page=(\d+); total=(-?[\d.]+)pt; goal=(-?[\d.]+)pt;")
            .expect("probe pattern is valid")
    });
    let total_pages = OUTPUT.captures_iter(log).last()?[1].parse::<u32>().ok()?;
    let mut refs = None;
    let mut appendix = None;
    for caps in PROBE.captures_iter(log) {
        let (Ok(page), Ok(total), Ok(goal)) = (
            caps[2].parse::<u32>(),
            caps[3].parse::<f64>(),
            caps[4].parse::<f64>(),
        ) else {
            continue;
        };
        let mark = Mark { page, total, goal };
        let slot = if &caps[1] == "refs" {
            &mut refs
        } else {
            &mut appendix
        };
        slot.get_or_insert(mark);
    }
    let (body_pages, fill) = match refs {
        None => (None, None),
        Some(mark) if mark.at_page_top() => {
            let body = mark.page.saturating_sub(1);
            (Some(body), (body > 0).then_some(1.0))
        }
        Some(mark) => (
            Some(mark.page),
            Some((mark.total / mark.goal).clamp(0.0, 1.0)),
        ),
    };
    Some(PageReport {
        total_pages,
        reference_start_page: refs.map(|m| m.page),
        appendix_start_page: appendix.map(|m| m.page),
        body_pages,
        last_body_page_fill_ratio: fill,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(lines: &[&str]) -> String {
        let mut text = lines.join("\n");
        text.push_str("\nOutput written on paper.pdf (11 pages, 348798 bytes).\n");
        text
    }

    #[test]
    fn bibliography_in_the_middle_of_a_page_counts_that_page() {
        let report = read_log(&log(&[
            "PSPROBE refs page=9; total=300.5pt; goal=400.0pt;",
            "PSPROBE appendix page=11; total=0.0pt; goal=16383.99998pt;",
        ]))
        .expect("report");
        assert_eq!(report.total_pages, 11);
        assert_eq!(report.reference_start_page, Some(9));
        assert_eq!(report.appendix_start_page, Some(11));
        assert_eq!(report.body_pages, Some(9));
        let fill = report.last_body_page_fill_ratio.expect("fill");
        assert!((fill - 0.751_25).abs() < 1e-9);
    }

    #[test]
    fn bibliography_at_the_top_of_a_page_leaves_that_page_out() {
        let report = read_log(&log(&[
            "PSPROBE refs page=10; total=0.0pt; goal=16383.99998pt;",
        ]))
        .expect("report");
        assert_eq!(report.body_pages, Some(9));
        assert_eq!(report.last_body_page_fill_ratio, Some(1.0));
    }

    #[test]
    fn missing_marks_and_missing_output_line() {
        let report = read_log(&log(&[])).expect("report");
        assert_eq!(
            (report.body_pages, report.reference_start_page),
            (None, None)
        );
        assert_eq!(report.total_pages, 11);
        assert!(read_log("! Emergency stop.").is_none());
    }

    #[test]
    fn only_the_first_mark_of_each_kind_counts() {
        let report = read_log(&log(&[
            "PSPROBE refs page=8; total=100pt; goal=400pt;",
            "PSPROBE refs page=12; total=100pt; goal=400pt;",
        ]))
        .expect("report");
        assert_eq!(report.reference_start_page, Some(8));
    }
}
