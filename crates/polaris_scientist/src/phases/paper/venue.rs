//! Venue rules the paper checks apply: which template markers identify the
//! venue and how many body pages it allows.

use std::sync::LazyLock;

use regex::Regex;

/// Page budget of a venue, counted without the bibliography.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBudget {
    /// The body must fill at least this many pages.
    pub min_body_pages: u32,
    /// The body may not run past this many pages.
    pub max_body_pages: u32,
    /// When the body has exactly `min_body_pages` pages, the last one counts
    /// only if it is at least this full (0 to 1).
    pub min_last_page_fill: f64,
}

/// A conference template the paper is written against.
#[derive(Clone, Copy, Debug)]
pub struct Venue {
    /// Name used in configuration (`paper.venue`) and in messages.
    pub name: &'static str,
    /// Regular expressions; any one that matches the comment-free TeX source
    /// shows the paper uses this template.
    pub markers: &'static [&'static str],
    /// The page budget, when the venue has one.
    pub budget: Option<PageBudget>,
}

/// The venues the checks know.
pub const VENUES: [Venue; 1] = [Venue {
    name: "iclr",
    markers: &[
        r"\\usepackage\s*(?:\[[^\]]*\])?\s*\{paper,\s*times\}",
        r"\\iclrfinalcopy",
    ],
    budget: Some(PageBudget {
        min_body_pages: 8,
        max_body_pages: 9,
        min_last_page_fill: 0.75,
    }),
}];

impl Venue {
    /// The venue called `name` (case does not matter).
    #[must_use]
    pub fn named(name: &str) -> Option<&'static Venue> {
        VENUES
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(name.trim()))
    }

    /// True when `tex` (comments already removed) carries one of the venue's
    /// template markers.
    #[must_use]
    pub fn matches(&self, tex: &str) -> bool {
        static CACHE: LazyLock<Vec<(&'static str, Vec<Regex>)>> = LazyLock::new(|| {
            VENUES
                .iter()
                .map(|v| {
                    (
                        v.name,
                        v.markers
                            .iter()
                            .map(|m| Regex::new(m).expect("venue marker is valid"))
                            .collect(),
                    )
                })
                .collect()
        });
        CACHE
            .iter()
            .find(|(name, _)| *name == self.name)
            .is_some_and(|(_, regexes)| regexes.iter().any(|r| r.is_match(tex)))
    }

    /// The venue whose template markers `tex` carries, if any.
    #[must_use]
    pub fn detect(tex: &str) -> Option<&'static Venue> {
        VENUES.iter().find(|v| v.matches(tex))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iclr_is_found_by_name_and_by_template_marker() {
        assert_eq!(Venue::named(" ICLR ").map(|v| v.name), Some("iclr"));
        assert!(Venue::named("unknown").is_none());
        let tex = "\\documentclass{article}\n\\usepackage{paper,times}\n";
        assert_eq!(Venue::detect(tex).map(|v| v.name), Some("iclr"));
        assert!(Venue::detect("\\usepackage{times}").is_none());
        assert!(Venue::detect("\\usepackage[final]{paper, times}").is_some());
        assert!(Venue::detect("\\iclrfinalcopy").is_some());
    }

    #[test]
    fn iclr_budget_matches_the_original() {
        let budget = Venue::named("iclr").and_then(|v| v.budget).expect("budget");
        assert_eq!((budget.min_body_pages, budget.max_body_pages), (8, 9));
        assert!((budget.min_last_page_fill - 0.75).abs() < f64::EPSILON);
    }
}
