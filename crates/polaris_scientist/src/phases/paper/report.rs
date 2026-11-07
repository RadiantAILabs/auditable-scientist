//! The validation report the paper check writes to `validation_report.json`.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::claims::NumberFinding;
use crate::latex::PageReport;

/// File name of the report.
pub const REPORT_FILE: &str = "validation_report.json";

/// Errors and warnings of one check.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// False when the check had nothing to look at.
    pub checked: bool,
    /// Problems the paper must fix.
    pub hard_errors: Vec<String>,
    /// Problems worth fixing that do not stop the paper.
    pub soft_warnings: Vec<String>,
}

impl Section {
    /// A section for a check that ran and found nothing yet.
    #[must_use]
    pub fn ran() -> Self {
        Self {
            checked: true,
            ..Self::default()
        }
    }
}

/// Result of tracing the `@evidence[[…]]` tokens of the paper.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSection {
    /// Errors and warnings.
    #[serde(flatten)]
    pub section: Section,
    /// Tokens found in the TeX sources.
    pub paper_token_count: usize,
    /// Tokens whose file and locator exist.
    pub resolved: usize,
    /// Tokens that point at nothing, written as `file:line token: reason`.
    pub dangling: Vec<String>,
}

/// Result of checking the figure manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FigureSection {
    /// Errors and warnings.
    #[serde(flatten)]
    pub section: Section,
    /// True when the paper or its strategy asks for figures.
    pub required: bool,
    /// Rows in `figure_manifest.json`.
    pub figure_count: usize,
}

/// Result of the page budget check.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PageSection {
    /// Errors; a page error is a hard error.
    #[serde(flatten)]
    pub section: Section,
    /// Pages in the PDF.
    pub total_pages: Option<u32>,
    /// Page where the bibliography starts.
    pub reference_start_page: Option<u32>,
    /// Body pages before the bibliography, as the rules count them.
    pub body_pages: Option<u32>,
    /// Pages the body has before the last one is judged by how full it is.
    pub raw_body_pages: Option<u32>,
    /// How full the last body page is (0 to 1).
    pub last_body_page_fill_ratio: Option<f64>,
    /// Whether the last body page counts toward the minimum.
    pub last_body_page_counted: Option<bool>,
    /// Lowest body page count the venue allows.
    pub min_body_pages: Option<u32>,
    /// Highest body page count the venue allows.
    pub max_body_pages: Option<u32>,
}

/// What the TeX build did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LatexSection {
    /// True when the build ran.
    pub ran: bool,
    /// True when it produced a PDF.
    pub ok: bool,
    /// Why it failed, in one line.
    pub error: Option<String>,
    /// The first error in the TeX log.
    pub first_error: Option<String>,
    /// The end of the TeX log.
    pub log_tail: String,
    /// Page facts read from the log.
    pub pages: Option<PageReport>,
}

/// Everything the paper check found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// True when there is no hard error.
    pub ok: bool,
    /// The first hard error, or `None`.
    pub error: Option<String>,
    /// Every hard error, in the order the checks found them.
    pub hard_errors: Vec<String>,
    /// Every soft warning.
    pub soft_warnings: Vec<String>,
    /// `paper_profile` from `paper_data.json`.
    pub paper_profile: String,
    /// Required sections the TeX does not have.
    pub missing_sections: Vec<String>,
    /// Distinct keys the TeX cites.
    pub citation_count: usize,
    /// Keys in `references.bib`.
    pub bibtex_key_count: usize,
    /// Claims in `claims_grounding.json`.
    pub claim_count: usize,
    /// Numbers the manuscript prints, in the body, captions, tables and
    /// imported files.
    pub numbers_printed: usize,
    /// Printed numbers that no claim and recorded value or derivation backs,
    /// each with its place and the reason.
    pub unbacked_numbers: Vec<NumberFinding>,
    /// Reference provenance against the bibliography.
    pub reference_validation: Section,
    /// Claims file against the known sources.
    pub claims_validation: Section,
    /// Evidence tokens.
    pub evidence_validation: EvidenceSection,
    /// Files the TeX imports.
    pub dependency_validation: Section,
    /// Template markers.
    pub venue_validation: Section,
    /// Figure manifest.
    pub figure_validation: FigureSection,
    /// The TeX build.
    pub latex: LatexSection,
    /// Page budget.
    pub page_validation: PageSection,
    /// The compiled PDF, when there is one.
    pub pdf_path: Option<String>,
}

impl ValidationReport {
    /// Records a hard error.
    pub fn error(&mut self, message: impl Into<String>) {
        self.hard_errors.push(message.into());
    }

    /// Fills `ok` and `error` from the recorded hard errors.
    pub fn finish(&mut self) {
        self.ok = self.hard_errors.is_empty();
        self.error = self.hard_errors.first().cloned();
    }

    /// Writes the report to `path` as indented JSON.
    ///
    /// # Errors
    ///
    /// Returns the I/O error from writing the file.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let mut body = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        body.push('\n');
        std::fs::write(path, body)
    }
}
