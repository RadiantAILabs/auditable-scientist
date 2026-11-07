//! Figure contracts: the manifest, its files, the strategy's figure specs and
//! the `\includegraphics` calls of the paper must agree.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::report::{FigureSection, Section};

/// Figure kinds the manifest may name.
pub(super) const FIGURE_TYPES: [&str; 11] = [
    "grouped_bar",
    "bar",
    "line",
    "box",
    "violin",
    "scatter",
    "heatmap",
    "table_matrix",
    "table",
    "composite",
    "diagram",
];

/// Most categories a panel may show before the figure is called crowded.
fn category_budget(kind: &str) -> u64 {
    match kind {
        "grouped_bar" | "box" | "violin" | "composite" => 6,
        "diagram" => 1,
        _ => 8,
    }
}

/// Most series a panel may show before the figure is called crowded.
fn series_budget(kind: &str) -> u64 {
    match kind {
        "heatmap" | "table_matrix" | "table" => 8,
        "diagram" => 1,
        _ => 4,
    }
}

/// Smallest font sizes, in points, for each text role in a figure.
const FONT_MINIMUMS: [(&str, f64); 4] = [
    ("title", 10.0),
    ("axis", 9.0),
    ("ticks", 8.0),
    ("legend", 8.0),
];

/// Strategy figure names that mean another figure type.
pub(super) fn strategy_type(raw: &str) -> &str {
    match raw.trim() {
        "flow_diagram" => "diagram",
        "horizontal_bar" => "bar",
        other => other,
    }
}

static INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\includegraphics\s*(?:\[[^\]]*\])?\s*\{([^{}]*)\}").expect("pattern is valid")
});
static INCLUDE_ANY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\includegraphics(?:[^A-Za-z@]|$)").expect("pattern is valid"));
static GRAPHICS_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\graphicspath\s*\{((?:\s*\{[^{}]*\}\s*)*)\}").expect("pattern is valid")
});
static BRACED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([^{}]*)\}").expect("pattern is valid"));

/// Ends the message about an `\includegraphics` call whose target is not text.
const NOT_LITERAL: &str = " with a target that is not literal";

/// Ends the message about a figure named by a path that TeX builds.
const BUILT_PATH: &str = " by a built path";

/// Whether `message` reports a figure file that the paper names in a way the
/// check cannot follow to a file inside the paper folder.
#[must_use]
pub fn is_path_error(message: &str) -> bool {
    message.contains(NOT_LITERAL) || message.contains(BUILT_PATH)
}

/// Everything the figure check reads besides the paper directory.
pub struct FigureInput<'a> {
    /// The paper directory.
    pub dir: &'a Path,
    /// The text of `paper.tex`.
    pub tex: &'a str,
    /// Ids a figure may cite as its sources.
    pub source_ids: &'a BTreeSet<String>,
}

/// Checks the figures of the paper in `input.dir`.
#[must_use]
pub fn check(input: &FigureInput<'_>) -> FigureSection {
    let mut errors: Vec<String> = Vec::new();
    let graphics = graphics_references(input, &mut errors);
    let (specs, spec_errors) = read_specs(input.dir);
    errors.extend(spec_errors);
    let manifest_path = input.dir.join("figure_manifest.json");
    let required =
        !graphics.is_empty() || !specs.is_empty() || manifest_path.exists() || !errors.is_empty();
    if !required {
        return FigureSection::default();
    }

    let mut rows: Vec<Value> = Vec::new();
    match std::fs::read_to_string(&manifest_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            errors.push("figure_manifest.json is missing".to_string());
        }
        Err(e) => errors.push(format!("figure_manifest.json cannot be read: {e}")),
        Ok(body) => match serde_json::from_str::<Value>(&body) {
            Err(e) => errors.push(format!("figure_manifest.json is not valid JSON: {e}")),
            Ok(value) => match value.get("figures").and_then(Value::as_array) {
                Some(list) => rows = list.clone(),
                None => errors.push("figure_manifest.json has no `figures` list".to_string()),
            },
        },
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut manifest_paths: BTreeSet<String> = BTreeSet::new();
    for (idx, row) in rows.iter().enumerate() {
        check_row(
            idx,
            row,
            input,
            &specs,
            &graphics,
            &mut seen,
            &mut manifest_paths,
            &mut errors,
        );
    }
    for (id, spec) in &specs {
        if spec.get("required").and_then(Value::as_bool) == Some(true) && !seen.contains(id) {
            errors.push(format!(
                "figure `{id}` is required by the strategy but missing"
            ));
        }
    }
    check_references(&graphics, &manifest_paths, &mut errors);

    let (hard_errors, soft_warnings) = classify(errors);
    FigureSection {
        section: Section {
            checked: true,
            hard_errors,
            soft_warnings,
        },
        required,
        figure_count: rows.len(),
    }
}

/// A figure file the paper names, with the manifest paths it could mean.
struct Graphic {
    raw: String,
    candidates: BTreeSet<String>,
    explicit_png: bool,
}

fn graphics_references(input: &FigureInput<'_>, errors: &mut Vec<String>) -> Vec<Graphic> {
    let text = super::masked(input.tex);
    let mut bases = vec![String::new()];
    for caps in GRAPHICS_PATH.captures_iter(&text) {
        for dir in BRACED.captures_iter(&caps[1]) {
            let dir = dir[1].trim();
            if !dir.is_empty() {
                bases.push(dir.trim_end_matches('/').to_string());
            }
        }
    }
    let parsed: BTreeSet<usize> = INCLUDE.find_iter(&text).map(|m| m.start()).collect();
    if INCLUDE_ANY
        .find_iter(&text)
        .any(|m| !parsed.contains(&m.start()))
    {
        errors.push(format!(
            "paper.tex has an \\includegraphics call{NOT_LITERAL}"
        ));
    }
    let mut graphics = Vec::new();
    for caps in INCLUDE.captures_iter(&text) {
        let raw = caps[1].trim().to_string();
        if raw.is_empty() || raw.contains('\\') || raw.contains('#') {
            errors.push(format!("paper.tex includes a figure{BUILT_PATH}: `{raw}`"));
            continue;
        }
        let explicit_ext = Path::new(&raw).extension().is_some();
        let names: Vec<String> = if explicit_ext {
            vec![raw.clone()]
        } else {
            ["pdf", "png"]
                .iter()
                .map(|e| format!("{raw}.{e}"))
                .collect()
        };
        let mut candidates = BTreeSet::new();
        for base in &bases {
            for name in &names {
                let joined = if base.is_empty() {
                    name.clone()
                } else {
                    format!("{base}/{name}")
                };
                if let Some(clean) = clean_relative(&joined) {
                    candidates.insert(clean);
                }
            }
        }
        graphics.push(Graphic {
            explicit_png: raw.to_ascii_lowercase().ends_with(".png"),
            raw,
            candidates,
        });
    }
    graphics
}

/// `path` as a forward-slash relative path without `.` parts, or `None` when
/// it is absolute or climbs out of its root.
fn clean_relative(path: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn read_specs(dir: &Path) -> (BTreeMap<String, Value>, Vec<String>) {
    let mut errors = Vec::new();
    let mut specs = BTreeMap::new();
    let path = dir.join("paper_strategy.json");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return (specs, errors);
    };
    let Ok(value) = serde_json::from_str::<Value>(&body) else {
        errors.push("paper_strategy.json is not valid JSON".to_string());
        return (specs, errors);
    };
    match value.get("figure_specs") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            for (idx, spec) in list.iter().enumerate() {
                match spec.get("figure_id").and_then(Value::as_str).map(str::trim) {
                    Some(id) if !id.is_empty() => {
                        specs.insert(id.to_string(), spec.clone());
                    }
                    _ => errors.push(format!("figure_specs[{idx}] has no figure_id")),
                }
            }
        }
        Some(_) => errors.push("paper_strategy.json field figure_specs must be a list".to_string()),
    }
    (specs, errors)
}

#[allow(clippy::too_many_arguments)]
fn check_row(
    idx: usize,
    row: &Value,
    input: &FigureInput<'_>,
    specs: &BTreeMap<String, Value>,
    graphics: &[Graphic],
    seen: &mut BTreeSet<String>,
    manifest_paths: &mut BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    let text = |key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
    };
    let number = |key: &str| row.get(key).and_then(Value::as_u64);
    let list = |key: &str| -> Vec<u64> {
        row.get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default()
    };

    let id = text("figure_id");
    if id.is_empty() {
        errors.push(format!("figures[{idx}] has no figure_id"));
    } else if !seen.insert(id.to_string()) {
        errors.push(format!("duplicate figure_id: {id}"));
    }
    let kind = text("figure_type");
    if !FIGURE_TYPES.contains(&kind) {
        errors.push(format!("figures[{idx}] unsupported figure_type: {kind:?}"));
    }

    let generator = text("generator");
    let png = text("path_png");
    let pdf = text("path_pdf");
    let png_header = artifact(input.dir, png, ".png", 24);
    if png.is_empty() || png_header.is_none() {
        errors.push(format!("figures[{idx}] missing PNG artifact: {png}"));
    }
    if let Some(clean) = clean_relative(png) {
        manifest_paths.insert(clean);
    }
    let mut pdf_is_vector = false;
    if pdf.is_empty() {
        if generator != "generate-concept-figure" {
            errors.push(format!("figures[{idx}] has no PDF path"));
        }
    } else {
        let header = artifact(input.dir, pdf, ".pdf", 5);
        pdf_is_vector = header.as_deref().is_some_and(|h| h.starts_with(b"%PDF-"));
        if !pdf_is_vector {
            errors.push(format!("figures[{idx}] missing PDF artifact: {pdf}"));
        }
        if let Some(clean) = clean_relative(pdf) {
            manifest_paths.insert(clean);
        }
    }

    let panels = number("n_panels").unwrap_or(0);
    let per_row = number("panels_per_row").unwrap_or(0);
    if panels == 0 || per_row == 0 {
        errors.push(format!(
            "figures[{idx}] needs n_panels and panels_per_row above 0"
        ));
    }
    let categories = list("categories_per_panel");
    let series = list("series_per_panel");
    if categories.len() as u64 != panels || series.len() as u64 != panels {
        errors.push(format!(
            "figures[{idx}] categories_per_panel and series_per_panel must have one entry per panel"
        ));
    }
    if panels > 4 {
        errors.push(format!(
            "figures[{idx}] exceeds max total panels: {panels} > 4"
        ));
    }
    if per_row > 3 {
        errors.push(format!(
            "figures[{idx}] exceeds max panels per row: {per_row} > 3"
        ));
    } else if per_row > panels {
        errors.push(format!("figures[{idx}] panels_per_row exceeds n_panels"));
    }

    let spec = specs.get(id);
    let mut cat_budget = category_budget(kind);
    let mut ser_budget = series_budget(kind);
    if let Some(spec) = spec {
        cat_budget = spec
            .get("max_categories_per_panel")
            .and_then(Value::as_u64)
            .unwrap_or(cat_budget);
        ser_budget = spec
            .get("max_series_per_panel")
            .and_then(Value::as_u64)
            .unwrap_or(ser_budget);
        let wanted = strategy_type(
            spec.get("figure_type")
                .and_then(Value::as_str)
                .unwrap_or(kind),
        );
        if wanted != kind {
            errors.push(format!(
                "figures[{idx}] figure_type does not match strategy for {id}"
            ));
        }
        if panels > spec.get("max_panels").and_then(Value::as_u64).unwrap_or(4) {
            errors.push(format!(
                "figures[{idx}] exceeds strategy max_panels for {id}"
            ));
        }
        let progression = row.get("uses_progression").and_then(Value::as_bool) == Some(true);
        if progression && spec.get("allow_progression").and_then(Value::as_bool) != Some(true) {
            errors.push(format!(
                "figures[{idx}] progression forbidden by strategy for {id}"
            ));
        }
    }
    for (panel, value) in categories.iter().enumerate() {
        if *value > cat_budget {
            errors.push(format!(
                "figures[{idx}] exceeds category budget in panel {panel}: {value} > {cat_budget}"
            ));
        }
    }
    for (panel, value) in series.iter().enumerate() {
        if *value > ser_budget {
            errors.push(format!(
                "figures[{idx}] exceeds series budget in panel {panel}: {value} > {ser_budget}"
            ));
        }
    }
    if number("max_label_chars").is_some_and(|n| n > 18)
        && row.get("uses_abbrev_labels").and_then(Value::as_bool) == Some(false)
    {
        errors.push(format!(
            "figures[{idx}] labels too long without abbreviation"
        ));
    }

    check_fonts(idx, row, kind, errors);

    if let Some(header) = png_header.as_deref().filter(|h| h.len() >= 24) {
        let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
        let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
        let min_width = match text("placement") {
            "full_width" => 2400,
            "column" => 1200,
            _ if panels > 2 => 1600,
            _ => 1200,
        };
        let min_height = if panels > 2 { 900 } else { 700 };
        if width < min_width {
            errors.push(format!(
                "figures[{idx}] PNG width too small: {width} < {min_width}"
            ));
        }
        if height < min_height {
            errors.push(format!(
                "figures[{idx}] PNG height too small: {height} < {min_height}"
            ));
        }
    }
    if pdf_is_vector
        && clean_relative(png).is_some_and(|p| {
            graphics
                .iter()
                .any(|g| g.explicit_png && g.candidates.contains(&p))
        })
    {
        errors.push(format!(
            "figures[{idx}] paper.tex references raster PNG despite available vector PDF"
        ));
    }

    let sources: Vec<&str> = row
        .get("source_ids")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if sources.is_empty() {
        if kind != "diagram" {
            errors.push(format!(
                "figures[{idx}].source_ids must be a non-empty list"
            ));
        }
    } else {
        for source in sources {
            if !input.source_ids.contains(source) {
                errors.push(format!("figures[{idx}] unknown source id: {source:?}"));
            }
        }
    }
}

fn check_fonts(idx: usize, row: &Value, kind: &str, errors: &mut Vec<String>) {
    if let Some(sizes) = row.get("font_sizes").and_then(Value::as_object) {
        for (key, minimum) in FONT_MINIMUMS {
            match sizes.get(key).and_then(Value::as_f64) {
                None => errors.push(format!("figures[{idx}].font_sizes.{key} missing")),
                Some(size) if size < minimum => errors.push(format!(
                    "figures[{idx}].font_sizes.{key} below minimum: {size} < {minimum}"
                )),
                Some(_) => {}
            }
        }
    } else if let Some(size) = row.get("min_font_size_pt").and_then(Value::as_f64) {
        let minimum = if kind == "diagram" { 7.0 } else { 8.0 };
        if size < minimum {
            errors.push(format!(
                "figures[{idx}].min_font_size_pt below minimum: {size} < {minimum}"
            ));
        }
    } else {
        errors.push(format!("figures[{idx}] missing font size metadata"));
    }
}

/// Reads up to `len` bytes of the figure file at `rel` when it is a regular
/// file with the wanted extension inside `dir`: no symlink at any part of the
/// path, no `..`, not absolute.
fn artifact(dir: &Path, rel: &str, extension: &str, len: usize) -> Option<Vec<u8>> {
    if !rel.to_ascii_lowercase().ends_with(extension) {
        return None;
    }
    let clean = clean_relative(rel)?;
    let mut path = PathBuf::from(dir);
    for part in clean.split('/') {
        path.push(part);
        if std::fs::symlink_metadata(&path)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
    }
    if !std::fs::metadata(&path).ok()?.is_file() {
        return None;
    }
    let mut buf = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take(len as u64)
        .read_to_end(&mut buf)
        .ok()?;
    let signature_ok = match extension {
        ".png" => buf.starts_with(b"\x89PNG\r\n\x1a\n") && buf.len() >= len,
        _ => !buf.is_empty(),
    };
    signature_ok.then_some(buf)
}

fn check_references(
    graphics: &[Graphic],
    manifest_paths: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    let mut referenced: BTreeSet<&String> = BTreeSet::new();
    for graphic in graphics {
        let matched: Vec<&String> = graphic
            .candidates
            .iter()
            .filter(|c| manifest_paths.contains(*c))
            .collect();
        if matched.is_empty() {
            errors.push(format!(
                "paper.tex references figure missing from figure_manifest.json: {}",
                graphic.raw
            ));
        }
        referenced.extend(matched);
    }
    if graphics.is_empty() {
        return;
    }
    for path in manifest_paths {
        let twin = if let Some(stem) = path.strip_suffix(".png") {
            Some(format!("{stem}.pdf"))
        } else {
            path.strip_suffix(".pdf").map(|stem| format!("{stem}.png"))
        };
        if referenced.contains(path) || twin.is_some_and(|t| referenced.iter().any(|r| **r == t)) {
            continue;
        }
        errors.push(format!(
            "figure_manifest entry not referenced by paper.tex: {path}"
        ));
    }
}

/// Splits figure problems into those that stop the paper and readability
/// warnings that do not.
fn classify(errors: Vec<String>) -> (Vec<String>, Vec<String>) {
    static SOFT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"^figures\[\d+\](?:",
            r" exceeds max total panels:",
            r"| exceeds max panels per row:",
            r"| panels_per_row exceeds n_panels",
            r"| exceeds category budget in panel",
            r"| exceeds series budget in panel",
            r"| labels too long without abbreviation",
            r"|\.font_sizes\.",
            r"|\.min_font_size_pt below minimum:",
            r"| missing font size metadata",
            r"| PNG width too small:",
            r"| PNG height too small:",
            r"| paper\.tex references raster PNG despite available vector PDF$",
            r")"
        ))
        .expect("pattern is valid")
    });
    errors.into_iter().partition(|e| !SOFT.is_match(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 2, 0, 0, 0]);
        bytes
    }

    fn row(id: &str, kind: &str, extra: &str) -> String {
        format!(
            r#"{{"figure_id":"{id}","figure_type":"{kind}","path_png":"figures/{id}.png",
            "path_pdf":"figures/{id}.pdf","n_panels":1,"panels_per_row":1,
            "categories_per_panel":[3],"series_per_panel":[1],"max_label_chars":10,
            "uses_abbrev_labels":true,"uses_progression":false,"placement":"column",
            "font_sizes":{{"title":10,"axis":9,"ticks":8,"legend":8}},
            "source_ids":["s1"]{extra}}}"#
        )
    }

    struct Paper {
        dir: tempfile::TempDir,
    }

    impl Paper {
        fn new(tex_graphics: &[&str], rows: &[String]) -> Self {
            let dir = tempfile::tempdir().expect("dir");
            std::fs::create_dir(dir.path().join("figures")).expect("figures");
            for r in rows {
                let value: Value = serde_json::from_str(r).expect("row");
                let id = value["figure_id"].as_str().expect("id");
                std::fs::write(dir.path().join(format!("figures/{id}.png")), png(1300, 800))
                    .expect("png");
                std::fs::write(dir.path().join(format!("figures/{id}.pdf")), b"%PDF-1.5 x")
                    .expect("pdf");
            }
            std::fs::write(
                dir.path().join("figure_manifest.json"),
                format!(r#"{{"figures":[{}]}}"#, rows.join(",")),
            )
            .expect("manifest");
            let tex: String = tex_graphics
                .iter()
                .map(|g| format!("\\includegraphics[width=\\linewidth]{{{g}}}\n"))
                .collect();
            std::fs::write(dir.path().join("paper.tex"), tex).expect("tex");
            Self { dir }
        }

        fn check(&self) -> FigureSection {
            let tex = std::fs::read_to_string(self.dir.path().join("paper.tex")).expect("tex");
            let ids: BTreeSet<String> = ["s1".to_string()].into();
            check(&FigureInput {
                dir: self.dir.path(),
                tex: &tex,
                source_ids: &ids,
            })
        }
    }

    #[test]
    fn a_consistent_paper_has_no_hard_errors() {
        let paper = Paper::new(&["figures/a"], &[row("a", "bar", "")]);
        let report = paper.check();
        assert!(
            report.section.hard_errors.is_empty(),
            "{:?}",
            report.section
        );
        assert_eq!(report.figure_count, 1);
        assert!(report.required);
    }

    #[test]
    fn a_paper_without_figures_is_not_checked() {
        let dir = tempfile::tempdir().expect("dir");
        let ids = BTreeSet::new();
        let report = check(&FigureInput {
            dir: dir.path(),
            tex: "no figures",
            source_ids: &ids,
        });
        assert!(!report.section.checked);
        assert!(!report.required);
    }

    #[test]
    fn a_figure_missing_from_the_manifest_is_a_hard_error() {
        let paper = Paper::new(&["figures/a", "figures/b"], &[row("a", "bar", "")]);
        let errors = paper.check().section.hard_errors;
        assert_eq!(
            errors,
            ["paper.tex references figure missing from figure_manifest.json: figures/b"]
        );
    }

    #[test]
    fn a_manifest_entry_the_paper_never_uses_is_a_hard_error() {
        let paper = Paper::new(&["figures/a"], &[row("a", "bar", ""), row("b", "bar", "")]);
        let errors = paper.check().section.hard_errors;
        assert!(
            errors
                .iter()
                .any(|e| e
                    .starts_with("figure_manifest entry not referenced by paper.tex: figures/b")),
            "{errors:?}"
        );
    }

    #[test]
    fn small_fonts_and_crowded_panels_are_warnings_only() {
        let crowded = row("a", "bar", "").replace(r#""title":10"#, r#""title":7"#);
        let paper = Paper::new(&["figures/a"], &[crowded]);
        let report = paper.check().section;
        assert!(report.hard_errors.is_empty(), "{:?}", report.hard_errors);
        assert_eq!(
            report.soft_warnings,
            ["figures[0].font_sizes.title below minimum: 7 < 10"]
        );
    }

    #[test]
    fn a_strategy_type_that_differs_from_the_manifest_is_hard() {
        let paper = Paper::new(&["figures/a"], &[row("a", "line", "")]);
        std::fs::write(
            paper.dir.path().join("paper_strategy.json"),
            r#"{"figure_specs":[{"figure_id":"a","figure_type":"bar","required":true}]}"#,
        )
        .expect("strategy");
        let errors = paper.check().section.hard_errors;
        assert_eq!(
            errors,
            ["figures[0] figure_type does not match strategy for a"]
        );
    }

    #[test]
    fn a_required_strategy_figure_must_exist() {
        let paper = Paper::new(&["figures/a"], &[row("a", "bar", "")]);
        std::fs::write(
            paper.dir.path().join("paper_strategy.json"),
            r#"{"figure_specs":[{"figure_id":"z","figure_type":"bar","required":true}]}"#,
        )
        .expect("strategy");
        let errors = paper.check().section.hard_errors;
        assert_eq!(
            errors,
            ["figure `z` is required by the strategy but missing"]
        );
    }

    #[test]
    fn unknown_sources_and_missing_files_are_hard_errors() {
        let paper = Paper::new(&["figures/a"], &[row("a", "bar", r#","source_ids_x":1"#)]);
        std::fs::remove_file(paper.dir.path().join("figures/a.pdf")).expect("remove");
        let manifest = std::fs::read_to_string(paper.dir.path().join("figure_manifest.json"))
            .expect("manifest")
            .replace(r#""source_ids":["s1"]"#, r#""source_ids":["nope"]"#);
        std::fs::write(paper.dir.path().join("figure_manifest.json"), manifest).expect("write");
        let errors = paper.check().section.hard_errors;
        assert!(errors.contains(&"figures[0] missing PDF artifact: figures/a.pdf".to_string()));
        assert!(errors.contains(&"figures[0] unknown source id: \"nope\"".to_string()));
    }

    #[test]
    fn a_symlinked_artifact_is_refused() {
        let paper = Paper::new(&["figures/a"], &[row("a", "bar", "")]);
        let outside = tempfile::NamedTempFile::new().expect("file");
        std::fs::write(outside.path(), png(1300, 800)).expect("png");
        let target = paper.dir.path().join("figures/a.png");
        std::fs::remove_file(&target).expect("remove");
        std::os::unix::fs::symlink(outside.path(), &target).expect("symlink");
        let errors = paper.check().section.hard_errors;
        assert_eq!(errors, ["figures[0] missing PNG artifact: figures/a.png"]);
    }

    #[test]
    fn a_built_include_path_is_refused() {
        let paper = Paper::new(&["figures/\\jobname"], &[row("a", "bar", "")]);
        let errors = paper.check().section.hard_errors;
        assert!(
            errors.iter().any(|e| e.contains("by a built path")),
            "{errors:?}"
        );
    }

    #[test]
    fn graphicspath_adds_search_directories() {
        let paper = Paper::new(&["a"], &[row("a", "bar", "")]);
        std::fs::write(
            paper.dir.path().join("paper.tex"),
            "\\graphicspath{{figures/}}\n\\includegraphics{a}\n",
        )
        .expect("tex");
        assert!(paper.check().section.hard_errors.is_empty());
    }

    #[test]
    fn relative_paths_cannot_leave_the_directory() {
        assert_eq!(clean_relative("a/./b.png").as_deref(), Some("a/b.png"));
        assert_eq!(clean_relative("../x.png"), None);
        assert_eq!(clean_relative("/etc/passwd"), None);
        assert_eq!(clean_relative("a/../b.png").as_deref(), Some("b.png"));
    }
}
