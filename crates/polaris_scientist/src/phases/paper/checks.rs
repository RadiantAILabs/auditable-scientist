//! The paper check: every rule a manuscript must meet before the audit sees it.
//!
//! [`validate`] reads the paper directory and returns a [`ValidationReport`].
//! It collects every problem it can find without compiling, so one repair can
//! fix them all, and it compiles the paper unless a static check shows that the
//! paper reads files from outside its folder. A paper with other errors still
//! compiles, so a reader can see the PDF; the report keeps the errors and is
//! not `ok`. The paper directory holds model-written files, so each read
//! refuses symbolic links and caps the size.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::figures::{self, FigureInput};
use super::report::{EvidenceSection, LatexSection, PageSection, Section, ValidationReport};
use super::venue::Venue;
pub use crate::claims::manuscript::read_regular;
use crate::claims::manuscript::{MAX_TEX_FILES, contained};
use crate::claims::{self, Manuscript, SourceValues};
use crate::latex::text::{self, EvidenceToken};
use crate::latex::{self, LatexTools};

/// What the check needs besides the paper directory.
#[derive(Clone, Debug)]
pub struct CheckSettings {
    /// Venue name (`paper.venue`).
    pub venue: String,
    /// TeX programs and their time limit.
    pub tools: LatexTools,
    /// False skips the compile and the page budget.
    pub compile: bool,
}

impl CheckSettings {
    /// Settings for `venue` with default tools; the check compiles.
    #[must_use]
    pub fn new(venue: &str) -> Self {
        Self {
            venue: venue.to_string(),
            tools: LatexTools::default(),
            compile: true,
        }
    }
}

/// Checks the paper in `dir`, writes `validation_report.json` there and
/// returns the report.
pub async fn validate_and_write(dir: &Path, settings: &CheckSettings) -> ValidationReport {
    let report = validate(dir, settings).await;
    if let Err(e) = report.write(&dir.join(super::report::REPORT_FILE)) {
        tracing::warn!(error = %e, "cannot write the validation report");
    }
    report
}

/// Checks the paper in `dir` and returns what it found.
pub async fn validate(dir: &Path, settings: &CheckSettings) -> ValidationReport {
    let mut report = ValidationReport::default();
    static_checks(dir, settings, &mut report);
    if settings.compile && may_compile(&report) {
        compile_and_count_pages(dir, settings, &mut report).await;
    }
    report.finish();
    report
}

/// Whether the static checks leave TeX safe to run.
///
/// TeX runs when the paper's file references are known to stay inside the paper
/// folder, even if other checks found errors: the reader still gets a PDF to
/// look at, and the errors stay in the report. It does not run when
/// `paper.tex` could not be read, when an import or bibliography declaration
/// leaves the folder or cannot be checked, or when a figure is named by a path
/// the check cannot follow.
fn may_compile(report: &ValidationReport) -> bool {
    report.dependency_validation.checked
        && report.dependency_validation.hard_errors.is_empty()
        && !report
            .figure_validation
            .section
            .hard_errors
            .iter()
            .any(|e| figures::is_path_error(e))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let body = read_regular(path)?;
    serde_json::from_str(&body).map_err(|e| format!("{name} is not valid JSON: {e}"))
}

fn static_checks(dir: &Path, settings: &CheckSettings, report: &mut ValidationReport) {
    let tex = match read_regular(&dir.join("paper.tex")) {
        Ok(tex) => tex,
        Err(e) => {
            report.error(e);
            return;
        }
    };
    let masked = super::masked(&tex);

    let missing = text::missing_sections(&masked);
    report.missing_sections = missing.iter().map(|s| (*s).to_string()).collect();
    if !missing.is_empty() {
        report.error(format!(
            "paper.tex is missing required sections: {}",
            missing.join(", ")
        ));
    }

    let bib = match read_regular(&dir.join("references.bib")) {
        Ok(bib) => Some(bib),
        Err(e) => {
            report.error(e);
            None
        }
    };
    let bib_keys = bib.as_deref().map(text::bib_keys).unwrap_or_default();
    report.bibtex_key_count = bib_keys.len();
    if bib.is_some() && bib_keys.is_empty() {
        report.error("references.bib has no BibTeX keys");
    }
    let cited = text::cited_keys(&masked);
    report.citation_count = cited.len();
    let unresolved: Vec<&String> = cited.iter().filter(|k| !bib_keys.contains(*k)).collect();
    if bib.is_some() && !unresolved.is_empty() {
        report.error(format!(
            "paper.tex cites keys that references.bib does not define: {}",
            join_quoted(unresolved.into_iter())
        ));
    }

    let data = load(dir, "paper_data.json", report);
    let claims = load(dir, "claims_grounding.json", report);
    let provenance = load(dir, "references_provenance.json", report);

    let sources = data.as_ref().and_then(|d| {
        match crate::claims::grounding_sources(d, &dir.join("paper_data.json")) {
            Ok(map) => Some(map),
            Err(_) => {
                report.error("paper_data.json has no object field `grounding_sources`");
                None
            }
        }
    });
    if let Some(d) = &data {
        report.paper_profile = d
            .get("paper_profile")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_string();
    }

    let mut allowed: BTreeSet<String> = sources
        .as_ref()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();

    if let Some(provenance) = &provenance {
        let (ids, section) = reference_section(provenance, &bib_keys, bib.is_some());
        report
            .hard_errors
            .extend(prefixed("references", &section.hard_errors));
        report.reference_validation = section;
        allowed.extend(ids);
    }

    if let Some(claims) = &claims {
        let section = claims_section(claims, &allowed, &bib_keys);
        report.claim_count = claims
            .get("claims")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        report
            .hard_errors
            .extend(prefixed("claims", &section.hard_errors));
        report.claims_validation = section;
    }

    if let (Some(claims), Some(sources)) = (&claims, &sources) {
        number_check(dir, claims, sources, &tex, report);
    }

    let evidence = evidence_section(dir, &tex);
    report
        .hard_errors
        .extend(prefixed("evidence", &evidence.section.hard_errors));
    report.evidence_validation = evidence;

    let dependency = dependency_section(dir, &tex);
    report
        .hard_errors
        .extend(prefixed("dependency", &dependency.hard_errors));
    report.dependency_validation = dependency;

    let venue = venue_section(&settings.venue, &masked);
    report
        .hard_errors
        .extend(prefixed("venue", &venue.hard_errors));
    report.soft_warnings.extend(venue.soft_warnings.clone());
    report.venue_validation = venue;

    let figure = figures::check(&FigureInput {
        dir,
        tex: &tex,
        source_ids: &allowed,
    });
    report
        .hard_errors
        .extend(prefixed("figures", &figure.section.hard_errors));
    report
        .soft_warnings
        .extend(figure.section.soft_warnings.clone());
    report.figure_validation = figure;
}

fn load(dir: &Path, name: &str, report: &mut ValidationReport) -> Option<Value> {
    match read_json(&dir.join(name)) {
        Ok(value) => Some(value),
        Err(e) => {
            report.error(e);
            None
        }
    }
}

fn prefixed(area: &str, errors: &[String]) -> Vec<String> {
    errors.iter().map(|e| format!("{area}: {e}")).collect()
}

fn join_quoted<'a>(items: impl Iterator<Item = &'a String>) -> String {
    items
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Checks `references_provenance.json` against the bibliography. Returns the
/// ids the provenance lists with the section.
fn reference_section(
    provenance: &Value,
    bib_keys: &BTreeSet<String>,
    bib_present: bool,
) -> (BTreeSet<String>, Section) {
    let mut section = Section::ran();
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let Some(rows) = provenance.get("references").and_then(Value::as_array) else {
        section
            .hard_errors
            .push("references_provenance.json has no `references` list".to_string());
        return (ids, section);
    };
    for (index, row) in rows.iter().enumerate() {
        if !row.is_object() {
            section
                .hard_errors
                .push(format!("references[{index}] is not an object"));
            continue;
        }
        match row.get("id").and_then(Value::as_str).map(str::trim) {
            Some(id) if !id.is_empty() => {
                if !ids.insert(id.to_string()) {
                    section
                        .hard_errors
                        .push(format!("references[{index}].id repeats `{id}`"));
                }
            }
            _ => section
                .hard_errors
                .push(format!("references[{index}].id is missing")),
        }
        match row
            .get("citation_key")
            .and_then(Value::as_str)
            .map(str::trim)
        {
            Some(key) if !key.is_empty() => {
                if !keys.insert(key.to_string()) {
                    section
                        .hard_errors
                        .push(format!("references[{index}].citation_key repeats `{key}`"));
                }
            }
            _ => section
                .hard_errors
                .push(format!("references[{index}].citation_key is missing")),
        }
    }
    if bib_present {
        let only_bib: Vec<&String> = bib_keys.difference(&keys).collect();
        if !only_bib.is_empty() {
            section.hard_errors.push(format!(
                "references.bib keys missing from references_provenance.json: {}",
                join_quoted(only_bib.into_iter())
            ));
        }
        let only_provenance: Vec<&String> = keys.difference(bib_keys).collect();
        if !only_provenance.is_empty() {
            section.hard_errors.push(format!(
                "references_provenance.json keys missing from references.bib: {}",
                join_quoted(only_provenance.into_iter())
            ));
        }
    }
    (ids, section)
}

/// Checks the shape of `claims_grounding.json` and that each claim cites
/// known sources and bibliography keys.
fn claims_section(
    claims: &Value,
    allowed: &BTreeSet<String>,
    bib_keys: &BTreeSet<String>,
) -> Section {
    let mut section = Section::ran();
    let rows = match claims.get("claims").and_then(Value::as_array) {
        Some(rows) if !rows.is_empty() => rows,
        _ => {
            section
                .hard_errors
                .push("claims_grounding.json needs a non-empty `claims` list".to_string());
            return section;
        }
    };
    for (index, row) in rows.iter().enumerate() {
        if !row.is_object() {
            section
                .hard_errors
                .push(format!("claims[{index}] is not an object"));
            continue;
        }
        let text = row.get("claim").and_then(Value::as_str).unwrap_or("");
        if text.trim().is_empty() {
            section
                .hard_errors
                .push(format!("claims[{index}].claim is empty or missing"));
        }
        match row.get("source_ids").and_then(Value::as_array) {
            Some(ids) if !ids.is_empty() => {
                for id in ids {
                    match id.as_str() {
                        Some(id) if allowed.contains(id) => {}
                        Some(id) => section
                            .hard_errors
                            .push(format!("claims[{index}].source_ids has unknown id `{id}`")),
                        None => section.hard_errors.push(format!(
                            "claims[{index}].source_ids has an id that is not text"
                        )),
                    }
                }
            }
            _ => section.hard_errors.push(format!(
                "claims[{index}].source_ids must be a non-empty list"
            )),
        }
        match row.get("citation_keys") {
            None | Some(Value::Null) => {}
            Some(Value::Array(keys)) => {
                for key in keys {
                    match key.as_str() {
                        Some(key) if bib_keys.contains(key) => {}
                        Some(key) => section.hard_errors.push(format!(
                            "claims[{index}].citation_keys has unknown key `{key}`"
                        )),
                        None => section.hard_errors.push(format!(
                            "claims[{index}].citation_keys has a key that is not text"
                        )),
                    }
                }
            }
            Some(_) => section
                .hard_errors
                .push(format!("claims[{index}].citation_keys must be a list")),
        }
    }
    section
}

/// Most number errors one check reports; the rest are counted in the last one.
const MAX_NUMBER_ERRORS: usize = 30;

/// Checks that every number the manuscript prints is backed by a claim and a
/// recorded value or derivation (see [`claims::check`]). The numbers that are
/// not backed become hard errors, one for each sentence or table row, so the
/// repair step sees where each one is printed.
fn number_check(
    dir: &Path,
    claims: &Value,
    sources: &Map<String, Value>,
    tex: &str,
    report: &mut ValidationReport,
) {
    let Ok(inputs) = claims::claim_inputs(claims, &dir.join("claims_grounding.json")) else {
        return;
    };
    let manuscript = Manuscript::from_main(dir, tex);
    for command in &manuscript.unread {
        report.error(command.clone());
    }
    let printed = manuscript.printed_numbers();
    let outcome = claims::check::check(&printed, &inputs, &SourceValues::from_sources(sources));
    report.numbers_printed = outcome.printed();
    for message in claims::explain(&outcome, MAX_NUMBER_ERRORS) {
        report.error(message);
    }
    report.unbacked_numbers = outcome.unbacked().cloned().collect();
}

// ---------------------------------------------------------------------------
// Evidence tokens

/// Traces every `@evidence[[file#locator]]` token of the paper's TeX files to
/// the file and the value it names.
fn evidence_section(dir: &Path, tex: &str) -> EvidenceSection {
    let mut out = EvidenceSection {
        section: Section::ran(),
        ..EvidenceSection::default()
    };
    let mut cache: BTreeMap<PathBuf, Option<Value>> = BTreeMap::new();
    for (file, body) in tex_files(dir, tex).0 {
        for token in text::evidence_tokens(&body) {
            out.paper_token_count += 1;
            match resolve_token(dir, &token, &mut cache) {
                Ok(()) => out.resolved += 1,
                Err(reason) => {
                    let note =
                        format!("{file}:{} @evidence[[{}]]: {reason}", token.line, token.raw);
                    out.section
                        .hard_errors
                        .push(format!("dangling token {note}"));
                    out.dangling.push(note);
                }
            }
        }
    }
    if out.paper_token_count == 0 {
        out.section
            .soft_warnings
            .push("the paper has no @evidence tokens".to_string());
    }
    out
}

fn resolve_token(
    dir: &Path,
    token: &EvidenceToken,
    cache: &mut BTreeMap<PathBuf, Option<Value>>,
) -> Result<(), String> {
    let path = contained(dir, &token.target)?;
    let meta =
        std::fs::symlink_metadata(&path).map_err(|_| "the file does not exist".to_string())?;
    if !meta.is_file() {
        return Err("the target is not a regular file".to_string());
    }
    let Some(locator) = token.locator.as_deref().filter(|l| !l.is_empty()) else {
        return Ok(());
    };
    if path.extension().is_none_or(|e| e != "json") {
        return Ok(());
    }
    let value = cache
        .entry(path.clone())
        .or_insert_with(|| read_json(&path).ok());
    let Some(value) = value else {
        return Err("the file is not valid JSON".to_string());
    };
    if locate(value, locator) {
        Ok(())
    } else {
        Err(format!("`{locator}` is not in the file"))
    }
}

/// True when the dotted `locator` names a value of `value`. A key may itself
/// contain dots, so every key that starts the remaining text is tried.
fn locate(value: &Value, locator: &str) -> bool {
    if locator.is_empty() {
        return true;
    }
    match value {
        Value::Object(map) => map.iter().any(|(key, child)| {
            if locator == key {
                true
            } else {
                locator
                    .strip_prefix(key.as_str())
                    .and_then(|rest| rest.strip_prefix('.'))
                    .is_some_and(|rest| locate(child, rest))
            }
        }),
        Value::Array(items) => {
            let (head, rest) = locator.split_once('.').unwrap_or((locator, ""));
            head.parse::<usize>()
                .ok()
                .and_then(|i| items.get(i))
                .is_some_and(|child| locate(child, rest))
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// TeX dependencies

/// The paper's own TeX files, read in import order, with the problems met
/// while following imports.
fn tex_files(dir: &Path, main: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut files = vec![("paper.tex".to_string(), main.to_string())];
    let mut errors = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::from(["paper.tex".to_string()]);
    let mut next = 0;
    while next < files.len() {
        let imported = text::imports(&files[next].1).inputs;
        next += 1;
        for target in imported {
            let name = if Path::new(&target).extension().is_some() {
                target.clone()
            } else {
                format!("{target}.tex")
            };
            if !seen.insert(name.clone()) {
                continue;
            }
            if files.len() >= MAX_TEX_FILES {
                errors.push(format!(
                    "the paper imports more than {MAX_TEX_FILES} TeX files"
                ));
                return (files, errors);
            }
            match contained(dir, &name) {
                Err(reason) => errors.push(format!("`{target}`: {reason}")),
                Ok(path) => match read_regular(&path) {
                    Ok(body) => files.push((name, body)),
                    Err(e) => errors.push(format!("imported file `{target}`: {e}")),
                },
            }
        }
    }
    (files, errors)
}

/// Checks that everything the TeX imports lies inside the paper directory.
fn dependency_section(dir: &Path, main: &str) -> Section {
    let mut section = Section::ran();
    let (files, errors) = tex_files(dir, main);
    section.hard_errors.extend(errors);
    for (name, body) in &files {
        let imports = text::imports(body);
        for dynamic in imports.dynamic {
            section.hard_errors.push(format!(
                "{name}: an import is built by a macro and cannot be checked: `{dynamic}`"
            ));
        }
        for target in imports.support {
            if (target.contains('/') || target.contains('\\'))
                && let Err(reason) = contained(dir, &target)
            {
                section
                    .hard_errors
                    .push(format!("{name}: `{target}`: {reason}"));
            }
        }
        let bibliography = text::bibliography(body);
        for dynamic in bibliography.dynamic {
            section.hard_errors.push(format!(
                "{name}: a bibliography declaration is built by a macro and cannot be checked: `{dynamic}`"
            ));
        }
        for database in bibliography.databases {
            let stem = database.trim().trim_end_matches(".bib");
            if stem != "references" {
                section.hard_errors.push(format!(
                    "{name}: the bibliography database must be `references`, not `{database}`"
                ));
            }
        }
        for style in bibliography.styles {
            let style = style.trim();
            if style.contains('/') || style.contains('\\') || style.contains("..") {
                section.hard_errors.push(format!(
                    "{name}: the bibliography style `{style}` is not a plain name"
                ));
            }
        }
    }
    section
}

// ---------------------------------------------------------------------------
// Venue and pages

fn venue_section(name: &str, masked: &str) -> Section {
    let mut section = Section::ran();
    match Venue::named(name) {
        None => section
            .soft_warnings
            .push(format!("no rules are known for venue `{name}`; the template was not checked")),
        Some(venue) if !venue.matches(masked) => section.hard_errors.push(format!(
            "paper.tex does not use the {} template (expected \\usepackage{{paper,times}} or \\iclrfinalcopy)",
            venue.name
        )),
        Some(_) => {}
    }
    section
}

async fn compile_and_count_pages(
    dir: &Path,
    settings: &CheckSettings,
    report: &mut ValidationReport,
) {
    let built = latex::build(dir, &settings.tools).await;
    report.latex = LatexSection {
        ran: true,
        ok: built.ok,
        error: built.error.clone(),
        first_error: built.first_error.clone(),
        log_tail: built.log_tail.clone(),
        pages: built.pages,
    };
    if !built.ok {
        let reason = built
            .error
            .clone()
            .unwrap_or_else(|| "the build failed".to_string());
        let mut message = format!("TeX build failed: {reason}");
        if let Some(first) = &built.first_error {
            message.push_str(&format!("; first error: {first}"));
        }
        report.error(message);
        return;
    }
    report.pdf_path = built.pdf.map(|p| p.display().to_string());
    if let Some(venue) = Venue::named(&settings.venue) {
        report.page_validation = page_section(venue, built.pages.as_ref());
        report.hard_errors.extend(prefixed(
            "pages",
            &report.page_validation.section.hard_errors.clone(),
        ));
    }
}

/// Applies the venue's page budget to the facts read from the TeX log.
fn page_section(venue: &Venue, pages: Option<&latex::PageReport>) -> PageSection {
    let mut out = PageSection {
        section: Section::ran(),
        ..PageSection::default()
    };
    let Some(budget) = venue.budget else {
        return out;
    };
    out.min_body_pages = Some(budget.min_body_pages);
    out.max_body_pages = Some(budget.max_body_pages);
    let Some(pages) = pages else {
        out.section
            .hard_errors
            .push("the TeX log has no page facts".to_string());
        return out;
    };
    out.total_pages = Some(pages.total_pages);
    out.reference_start_page = pages.reference_start_page;
    let Some(raw) = pages.body_pages else {
        out.section
            .hard_errors
            .push("the TeX log does not show where the bibliography starts".to_string());
        return out;
    };
    out.raw_body_pages = Some(raw);
    out.last_body_page_fill_ratio = pages.last_body_page_fill_ratio;
    let counted = pages
        .last_body_page_fill_ratio
        .is_none_or(|ratio| ratio >= budget.min_last_page_fill);
    out.last_body_page_counted = Some(counted);
    let body = if counted { raw } else { raw.saturating_sub(1) };
    out.body_pages = Some(body);
    if raw > budget.max_body_pages {
        out.section.hard_errors.push(format!(
            "the body has {raw} pages; {} allows at most {}",
            venue.name, budget.max_body_pages
        ));
    } else if body < budget.min_body_pages {
        let fill = pages
            .last_body_page_fill_ratio
            .map_or_else(String::new, |r| {
                format!(" (the last page is {:.0}% full)", r * 100.0)
            });
        out.section.hard_errors.push(format!(
            "the body has {body} full pages; {} needs at least {}{fill}",
            venue.name, budget.min_body_pages
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::latex::PageReport;

    fn pages(raw: u32, fill: Option<f64>) -> PageReport {
        PageReport {
            total_pages: raw + 2,
            reference_start_page: Some(raw + 1),
            appendix_start_page: None,
            body_pages: Some(raw),
            last_body_page_fill_ratio: fill,
        }
    }

    fn iclr() -> &'static Venue {
        Venue::named("iclr").expect("iclr")
    }

    #[test]
    fn locator_walks_keys_that_contain_dots() {
        let value: Value = serde_json::json!({
            "grounding_sources": {"cfg.acc.mean": {"value": 1}},
            "list": [{"a": 1}]
        });
        assert!(locate(&value, "grounding_sources.cfg.acc.mean"));
        assert!(locate(&value, "grounding_sources.cfg.acc.mean.value"));
        assert!(locate(&value, "list.0.a"));
        assert!(!locate(&value, "grounding_sources.cfg.acc"));
        assert!(!locate(&value, "list.1"));
        assert!(!locate(&value, "missing"));
    }

    #[test]
    fn paths_must_stay_inside_the_directory() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(contained(dir.path(), "figures/a.pdf").is_ok());
        assert!(contained(dir.path(), "./a.tex").is_ok());
        assert!(contained(dir.path(), "../a.tex").is_err());
        assert!(contained(dir.path(), "/etc/passwd").is_err());
        assert!(contained(dir.path(), ".claude/skills/x.tex").is_err());
        assert!(contained(dir.path(), "").is_err());
    }

    #[test]
    fn symbolic_links_are_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.tex"), "x").expect("write");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("link");
        std::os::unix::fs::symlink(outside.path().join("secret.tex"), dir.path().join("s.tex"))
            .expect("link");
        assert!(contained(dir.path(), "link/secret.tex").is_err());
        assert!(read_regular(&dir.path().join("s.tex")).is_err());
    }

    #[test]
    fn eight_full_pages_pass_and_a_thin_eighth_page_does_not() {
        let ok = page_section(iclr(), Some(&pages(8, Some(0.9))));
        assert!(
            ok.section.hard_errors.is_empty(),
            "{:?}",
            ok.section.hard_errors
        );
        assert_eq!(ok.body_pages, Some(8));
        let thin = page_section(iclr(), Some(&pages(8, Some(0.5))));
        assert_eq!(thin.last_body_page_counted, Some(false));
        assert_eq!(thin.body_pages, Some(7));
        assert!(thin.section.hard_errors[0].contains("at least 8"));
        let nine = page_section(iclr(), Some(&pages(9, Some(0.2))));
        assert!(nine.section.hard_errors.is_empty());
    }

    #[test]
    fn a_body_over_the_limit_is_an_error() {
        let over = page_section(iclr(), Some(&pages(10, Some(0.9))));
        assert!(over.section.hard_errors[0].contains("at most 9"));
        let none = page_section(iclr(), None);
        assert!(none.section.hard_errors[0].contains("page facts"));
    }

    #[test]
    fn provenance_and_bibliography_are_compared_both_ways() {
        let provenance = serde_json::json!({"references": [
            {"id": "ref.a", "citation_key": "A"},
            {"id": "ref.a", "citation_key": "C"},
            {"id": "ref.b"}
        ]});
        let keys: BTreeSet<String> = ["A", "B"].iter().map(|s| (*s).to_string()).collect();
        let (ids, section) = reference_section(&provenance, &keys, true);
        assert_eq!(ids.len(), 2);
        let joined = section.hard_errors.join("\n");
        assert!(joined.contains("repeats `ref.a`"), "{joined}");
        assert!(joined.contains("citation_key is missing"), "{joined}");
        assert!(
            joined.contains("references.bib keys missing from references_provenance.json: `B`")
        );
        assert!(
            joined.contains("references_provenance.json keys missing from references.bib: `C`")
        );
    }

    #[test]
    fn claims_must_name_known_sources_and_keys() {
        let claims = serde_json::json!({"claims": [
            {"id": "c1", "claim": "x", "source_ids": ["s1", "nope"], "citation_keys": ["A", "Z"]},
            {"id": "c2", "claim": " ", "source_ids": []}
        ]});
        let allowed: BTreeSet<String> = ["s1".to_string()].into();
        let keys: BTreeSet<String> = ["A".to_string()].into();
        let section = claims_section(&claims, &allowed, &keys);
        let joined = section.hard_errors.join("\n");
        assert!(joined.contains("unknown id `nope`"), "{joined}");
        assert!(joined.contains("unknown key `Z`"), "{joined}");
        assert!(joined.contains("claims[1].claim is empty"), "{joined}");
        assert!(
            joined.contains("claims[1].source_ids must be a non-empty list"),
            "{joined}"
        );
        let empty = claims_section(&serde_json::json!({"claims": []}), &allowed, &keys);
        assert_eq!(empty.hard_errors.len(), 1);
    }
}
