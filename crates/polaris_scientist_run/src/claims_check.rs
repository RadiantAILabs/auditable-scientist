//! The offline check of a finished paper against its recorded claims.
//!
//! The check itself lives outside this crate. The command reaches it through
//! [`ClaimsChecker`], so the command can be built, and its behaviour tested,
//! with any checker, and so the check never needs a model, a network or a
//! credential.

use std::path::Path;

/// Checks the paper in a directory.
pub trait ClaimsChecker: Send + Sync {
    /// Check the paper in `paper_dir` and return the report.
    ///
    /// The check must use no model and no network, and must give the same
    /// report for the same directory.
    ///
    /// # Errors
    ///
    /// Plain words saying why the directory could not be checked, for example
    /// a file it needs is missing.
    fn check(&self, paper_dir: &Path) -> Result<serde_json::Value, String>;
}

impl<F> ClaimsChecker for F
where
    F: Fn(&Path) -> Result<serde_json::Value, String> + Send + Sync,
{
    fn check(&self, paper_dir: &Path) -> Result<serde_json::Value, String> {
        self(paper_dir)
    }
}

/// The checker this build carries: [`polaris_scientist::claims::check_paper`],
/// with its report as JSON.
///
/// Always `Some`; the `Option` lets the command be built, and tested, without
/// a checker.
#[must_use]
pub fn linked() -> Option<std::sync::Arc<dyn ClaimsChecker>> {
    Some(std::sync::Arc::new(|dir: &Path| {
        polaris_scientist::claims::check_paper(dir)
            .map_err(|error| error.to_string())
            .and_then(|check| serde_json::to_value(check).map_err(|error| error.to_string()))
    }))
}
