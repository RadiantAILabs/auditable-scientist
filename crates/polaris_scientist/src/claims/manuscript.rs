//! The manuscript as a reader sees it, and the numbers it prints.
//!
//! [`Manuscript::read`] reads `paper.tex` and every TeX file it imports with
//! `\input` or `\include`, and places each imported file where its import
//! stands. Comments, text under a false conditional and verbatim text are
//! removed first, so what is left is what the compiler would typeset. A command
//! that prints another file in some other way (`\subfile`, `\lstinputlisting`)
//! is listed in [`Manuscript::unread`], because its numbers cannot be checked.
//!
//! [`Manuscript::printed_numbers`] then lists every number the text prints:
//! in the body, in captions, in the cells of tables, in the title and in the
//! macros the paper defines. Each number comes with the file and line where it
//! stands, the sentence or the table cell it belongs to, and the words of that
//! sentence or cell. The words are what ties a number to a claim: a claim
//! covers a printed number only when it shares words with the number's
//! sentence or cell (see [`super::check`]).
//!
//! Every read refuses symbolic links and paths that leave the paper folder,
//! because the folder holds files that a model wrote.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::numbers::{self, NumberToken};
use crate::latex::text;

/// Largest file the check reads.
pub(crate) const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Most TeX files one manuscript may import.
pub(crate) const MAX_TEX_FILES: usize = 64;

/// How deep imports may nest.
const MAX_IMPORT_DEPTH: usize = 6;

/// Directories of agent tooling that a paper must not depend on.
pub(crate) const TOOL_DIRS: [&str; 6] = [
    ".claude",
    ".codex",
    ".cursor",
    ".opencode",
    ".gemini",
    ".pi",
];

/// Reads a regular file of the paper directory as text.
///
/// # Errors
///
/// Returns a message when the file is missing, is a symbolic link, is not a
/// regular file, is too large, or cannot be read.
pub fn read_regular(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("{name} is missing")
        } else {
            format!("{name} cannot be read: {e}")
        }
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!("{name} is not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{name} is larger than {MAX_FILE_BYTES} bytes"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_FILE_BYTES).read_to_end(&mut bytes))
        .map_err(|e| format!("{name} cannot be read: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Joins `relative` onto `dir` when the result stays inside `dir`.
///
/// # Errors
///
/// Returns the reason when the path is empty, leaves the directory, goes
/// through a symbolic link or names an agent tooling directory.
pub(crate) fn contained(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = relative.trim();
    if relative.is_empty() {
        return Err("the path is empty".to_string());
    }
    let mut clean = PathBuf::new();
    for part in Path::new(relative).components() {
        match part {
            Component::Normal(name) => {
                if TOOL_DIRS.iter().any(|d| name == *d) {
                    return Err("the path is inside an agent tooling directory".to_string());
                }
                clean.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("the path leaves the paper directory".to_string());
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("the path is empty".to_string());
    }
    let mut walk = dir.to_path_buf();
    for part in clean.components() {
        walk.push(part);
        if std::fs::symlink_metadata(&walk).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("the path goes through a symbolic link".to_string());
        }
    }
    Ok(dir.join(clean))
}

/// The TeX source as the compiler would typeset it: comments and false
/// conditional branches removed, with the lines kept. Verbatim text stays,
/// because it is printed and its numbers count.
#[must_use]
pub fn visible(tex: &str) -> String {
    text::strip_false_conditionals(&text::strip_comments(tex))
}

static VERBATIM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)\\begin\{(?:verbatim\*?|lstlisting|minted)\}.*?\\end\{(?:verbatim\*?|lstlisting|minted)\}",
    )
    .expect("pattern is valid")
});

static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(?:input|include)(?:\s*\{([^{}]+)\}|\s+([^\s%{}]+))").expect("pattern is valid")
});

/// Commands that print the contents of another file but are not followed. A
/// number in such a file would pass unchecked, so the check reports the
/// command instead.
static UNFOLLOWED_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\\(subfile|subimport|import|includefrom|inputfrom|subincludefrom|subinputfrom|InputIfFileExists|verbatiminput|VerbatimInput|lstinputlisting|inputminted|csvautotabular|csvloop|csvreader|pgfplotstableread|pgfplotstabletypeset|DTLloaddb)\b",
    )
    .expect("pattern is valid")
});

/// One file's share of the flattened text.
#[derive(Clone, Debug)]
struct Segment {
    start: usize,
    file: String,
    /// The line of the file on which the segment starts, counted from 1.
    line: usize,
}

/// The kind of place a number is printed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A sentence of the body.
    Prose,
    /// A figure or table caption.
    Caption,
    /// A cell of a table.
    Cell,
    /// A macro or definition in the preamble.
    Macro,
    /// The title.
    Title,
}

/// A number the manuscript prints, with the place it is printed in.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintedNumber {
    /// The number.
    pub token: NumberToken,
    /// The file it is printed in.
    pub file: String,
    /// The line of that file, counted from 1.
    pub line: usize,
    /// The kind of place.
    pub kind: Kind,
    /// The sentence or table cell, shortened, for messages.
    pub excerpt: String,
    /// Key of the sentence or the cell; numbers that share it are reported
    /// together.
    pub unit: usize,
    /// The content words of the sentence or cell.
    pub words: BTreeSet<String>,
}

/// The manuscript: `paper.tex` with its imports placed inline.
#[derive(Clone, Debug)]
pub struct Manuscript {
    text: String,
    segments: Vec<Segment>,
    /// Files read, `paper.tex` first, in the order they are placed.
    pub files: Vec<String>,
    /// Imports that could not be followed, each with the reason.
    pub problems: Vec<String>,
    /// Commands that load a file the check does not read, each with its place.
    pub unread: Vec<String>,
}

impl Manuscript {
    /// Reads `paper.tex` in `dir` and the files it imports.
    ///
    /// # Errors
    ///
    /// Returns the reason when `paper.tex` itself cannot be read.
    pub fn read(dir: &Path) -> Result<Self, String> {
        let main = read_regular(&dir.join("paper.tex"))?;
        Ok(Self::from_main(dir, &main))
    }

    /// Builds the manuscript from the text of `paper.tex`, reading imports
    /// from `dir`.
    #[must_use]
    pub fn from_main(dir: &Path, main: &str) -> Self {
        let mut out = Self {
            text: String::new(),
            segments: Vec::new(),
            files: vec!["paper.tex".to_string()],
            problems: Vec::new(),
            unread: Vec::new(),
        };
        out.append(dir, "paper.tex", main, 0);
        out
    }

    fn append(&mut self, dir: &Path, name: &str, source: &str, depth: usize) {
        let visible = visible(source);
        // An import written inside verbatim text is shown, not followed.
        let verbatim: Vec<Span> = VERBATIM
            .find_iter(&visible)
            .map(|m| (m.start(), m.end()))
            .collect();
        let mut cursor = 0;
        for found in IMPORT.captures_iter(&visible) {
            let whole = found.get(0).expect("group 0 always exists");
            if verbatim
                .iter()
                .any(|(from, to)| *from <= whole.start() && whole.start() < *to)
            {
                continue;
            }
            self.push(name, &visible, cursor, whole.start());
            cursor = whole.end();
            let target = found
                .get(1)
                .or_else(|| found.get(2))
                .map_or("", |m| m.as_str().trim())
                .to_string();
            let file = if Path::new(&target).extension().is_some() {
                target.clone()
            } else {
                format!("{target}.tex")
            };
            if depth >= MAX_IMPORT_DEPTH || self.files.len() >= MAX_TEX_FILES {
                self.problems.push(format!(
                    "`{target}` was not read: too many or too deep imports"
                ));
                continue;
            }
            match contained(dir, &file).and_then(|path| read_regular(&path)) {
                Ok(body) => {
                    self.files.push(file.clone());
                    self.text.push('\n');
                    self.append(dir, &file, &body, depth + 1);
                    self.text.push('\n');
                }
                Err(reason) => self.problems.push(format!("`{target}`: {reason}")),
            }
        }
        self.push(name, &visible, cursor, visible.len());
        for found in UNFOLLOWED_IMPORT.captures_iter(&visible) {
            let whole = found.get(0).expect("group 0 always exists");
            if verbatim
                .iter()
                .any(|(from, to)| *from <= whole.start() && whole.start() < *to)
            {
                continue;
            }
            self.unread.push(format!(
                "`\\{}` in {name} line {} loads a file that the check does not read; use `\\input` so that its numbers are checked",
                &found[1],
                1 + visible[..whole.start()].matches('\n').count()
            ));
        }
    }

    fn push(&mut self, file: &str, visible: &str, from: usize, to: usize) {
        if from >= to {
            return;
        }
        self.segments.push(Segment {
            start: self.text.len(),
            file: file.to_string(),
            line: 1 + visible[..from].matches('\n').count(),
        });
        self.text.push_str(&visible[from..to]);
    }

    /// The file and line of a byte offset of the flattened text.
    fn locate(&self, offset: usize) -> (&str, usize) {
        let index = self
            .segments
            .partition_point(|s| s.start <= offset)
            .saturating_sub(1);
        let segment = &self.segments[index];
        let lines = self.text[segment.start..offset.min(self.text.len())]
            .matches('\n')
            .count();
        (&segment.file, segment.line + lines)
    }

    /// Every number the manuscript prints, in order.
    #[must_use]
    pub fn printed_numbers(&self) -> Vec<PrintedNumber> {
        let layout = Layout::new(&self.text);
        let counted = layout.counted_text(&self.text);
        let mut out = Vec::new();
        for token in numbers::extract(&counted) {
            let (file, line) = self.locate(token.start);
            let place = layout.place(&self.text, &token);
            if place.notation {
                continue;
            }
            out.push(PrintedNumber {
                token,
                file: file.to_string(),
                line,
                kind: place.kind,
                excerpt: place.excerpt,
                unit: place.unit,
                words: place.words,
            });
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Where a number is printed

/// A group of the TeX text: the byte range of its content.
type Span = (usize, usize);

struct Row {
    cells: Vec<Span>,
}

struct Table {
    body: Span,
    rows: Vec<Row>,
    caption: BTreeSet<String>,
}

struct Place {
    kind: Kind,
    excerpt: String,
    unit: usize,
    words: BTreeSet<String>,
    /// True when the number is notation, not a printed result: see
    /// [`NOTATION_INTEGERS`] and [`INDEX_HEADERS`].
    notation: bool,
}

/// Integers that formulas use as structure: bounds, unit and doubling. They
/// are not checked when they stand inside math. In text, in a caption or in a
/// table cell they are checked like every other number.
const NOTATION_INTEGERS: [&str; 3] = ["0", "1", "2"];

/// Column headers of a table that numbers its rows; the integers under such a
/// header are labels, not results.
const INDEX_HEADERS: [&str; 6] = ["i", "j", "idx", "index", "id", "#"];

/// The structure of the flattened text that the places are read from.
struct Layout {
    /// Ranges that are not counted: the preamble outside titles and macro
    /// definitions, the bibliography and what follows `\end{document}`.
    skipped: Vec<Span>,
    /// Titles and macro definitions of the preamble, with their kind.
    definitions: Vec<(Span, Kind)>,
    captions: Vec<Span>,
    tables: Vec<Table>,
    /// Inline and display math.
    math: Vec<Span>,
    /// Coordinates and options of drawings, which place shapes and print no
    /// result.
    tikz: Vec<Span>,
    /// Starts of the sentences of the body, ascending.
    boundaries: Vec<usize>,
}

static BEGIN_DOCUMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\begin\s*\{document\}").expect("pattern is valid"));
static END_DOCUMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\end\s*\{document\}").expect("pattern is valid"));
static BIBLIOGRAPHY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)\\begin\s*\{thebibliography\}.*?\\end\s*\{thebibliography\}")
        .expect("pattern is valid")
});
static DEFINITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(title|newcommand|renewcommand|providecommand|def)\b").expect("pattern is valid")
});
static CAPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\caption\*?\s*(?:\[[^\]]*\])?\s*\{").expect("pattern is valid"));
static TABLE_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\begin\s*\{(tabular\*?|tabularx|longtable)\}").expect("pattern is valid")
});
static FLOAT_BEGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\begin\s*\{table\*?\}").expect("pattern is valid"));
static BREAK_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\\(?:item|section|subsection|subsubsection|paragraph|caption|begin|end|maketitle|title|\\)\b|\\\\",
    )
    .expect("pattern is valid")
});

/// Words before a full stop that do not end a sentence.
const ABBREVIATIONS: [&str; 16] = [
    "e.g", "i.e", "al", "vs", "cf", "fig", "figs", "eq", "eqs", "sec", "approx", "no", "resp",
    "etc", "ca", "viz",
];

impl Layout {
    fn new(text: &str) -> Self {
        let mut skipped = Vec::new();
        let mut definitions = Vec::new();
        let document = BEGIN_DOCUMENT.find(text).map(|m| m.end());
        if let Some(body) = document {
            skipped.push((0, body));
            for found in DEFINITION.captures_iter(&text[..body]) {
                let whole = found.get(0).expect("group 0 always exists");
                let kind = if &found[1] == "title" {
                    Kind::Title
                } else {
                    Kind::Macro
                };
                let end = definition_end(text, whole.end(), body);
                definitions.push(((whole.start(), end), kind));
            }
        }
        if let Some(end) = END_DOCUMENT.find(text) {
            skipped.push((end.start(), text.len()));
        }
        for found in BIBLIOGRAPHY.find_iter(text) {
            skipped.push((found.start(), found.end()));
        }
        let captions = CAPTION
            .find_iter(text)
            .map(|m| (m.end(), group_close(text, m.end() - 1)))
            .collect();
        let tables = tables(text);
        let boundaries = boundaries(text);
        Self {
            skipped,
            definitions,
            captions,
            tables,
            math: math_spans(text),
            tikz: tikz_coordinates(text),
            boundaries,
        }
    }

    fn in_math(&self, at: usize) -> bool {
        self.math.iter().any(|(from, to)| *from <= at && at < *to)
    }

    /// The text with everything that is not counted replaced by spaces; the
    /// definitions of the preamble stay.
    fn counted_text(&self, text: &str) -> String {
        let mut keep: Vec<Span> = self.definitions.iter().map(|(span, _)| *span).collect();
        keep.sort_unstable();
        let mut blanks: Vec<Span> = self.tikz.clone();
        for (from, to) in &self.skipped {
            let mut cursor = *from;
            for (kept_from, kept_to) in &keep {
                if *kept_from >= cursor && *kept_to <= *to {
                    blanks.push((cursor, *kept_from));
                    cursor = *kept_to;
                }
            }
            blanks.push((cursor, *to));
        }
        blanks.sort_unstable();
        // Each blanked character becomes as many spaces as it had bytes, so
        // every offset of the text stays valid.
        let mut out = String::with_capacity(text.len());
        let mut next = 0;
        for (offset, c) in text.char_indices() {
            while next < blanks.len() && blanks[next].1 <= offset {
                next += 1;
            }
            let blanked = blanks[next..]
                .iter()
                .take_while(|(from, _)| *from <= offset)
                .any(|(_, to)| offset < *to);
            if blanked && c != '\n' {
                out.extend(std::iter::repeat_n(' ', c.len_utf8()));
            } else {
                out.push(c);
            }
        }
        out
    }

    fn place(&self, text: &str, token: &NumberToken) -> Place {
        let at = token.start;
        if let Some(((from, to), kind)) = self
            .definitions
            .iter()
            .find(|((from, to), _)| *from <= at && at < *to)
        {
            let clean = clean_tex(&text[*from..*to]);
            let mut words = content_words(&clean);
            words.extend(content_words(&split_name(&text[*from..*to])));
            return Place {
                kind: *kind,
                excerpt: shorten(&clean, 0, 200),
                unit: *from,
                words,
                notation: false,
            };
        }
        for table in &self.tables {
            if table.body.0 <= at && at < table.body.1 {
                return table.place(text, token);
            }
        }
        let caption = self
            .captions
            .iter()
            .any(|(from, to)| *from <= at && at < *to);
        let (from, to) = self.sentence(at, text.len());
        let clean = clean_tex(&text[from..to]);
        let position = clean_tex(&text[from..at]).chars().count();
        let mut words = content_words(&clean);
        if words.len() < 3 {
            // A display equation or a short item has few words of its own;
            // the sentences around it say what it is about.
            for (near_from, near_to) in self.neighbours(at, text.len()) {
                words.extend(content_words(&clean_tex(&text[near_from..near_to])));
            }
        }
        Place {
            kind: if caption { Kind::Caption } else { Kind::Prose },
            excerpt: shorten(&clean, position, 200),
            unit: from,
            words,
            notation: NOTATION_INTEGERS.contains(&token.text.as_str()) && self.in_math(at),
        }
    }

    /// The bounds of the sentence that holds `at`.
    fn sentence(&self, at: usize, len: usize) -> Span {
        let index = self.boundaries.partition_point(|b| *b <= at);
        let from = index.checked_sub(1).map_or(0, |i| self.boundaries[i]);
        let to = self.boundaries.get(index).copied().unwrap_or(len);
        (from, to)
    }

    /// The sentences before and after the one that holds `at`.
    fn neighbours(&self, at: usize, len: usize) -> Vec<Span> {
        let index = self.boundaries.partition_point(|b| *b <= at);
        let bound = |i: usize| self.boundaries.get(i).copied().unwrap_or(len);
        let mut out = Vec::new();
        if index >= 2 {
            out.push((self.boundaries[index - 2], self.boundaries[index - 1]));
        }
        if index < self.boundaries.len() {
            out.push((bound(index), bound(index + 1)));
        }
        out
    }
}

impl Table {
    fn place(&self, text: &str, token: &NumberToken) -> Place {
        let at = token.start;
        let row_index = self
            .rows
            .iter()
            .position(|row| row.cells.iter().any(|(from, to)| *from <= at && at < *to));
        let Some(row) = row_index.map(|i| &self.rows[i]) else {
            return Place {
                kind: Kind::Cell,
                excerpt: String::new(),
                unit: self.body.0,
                words: self.caption.clone(),
                notation: false,
            };
        };
        let col = row
            .cells
            .iter()
            .position(|(from, to)| *from <= at && at < *to)
            .unwrap_or(0);
        let mut words = self.caption.clone();
        let header = self.rows.first();
        let mut header_text = String::new();
        if let Some(cell) = header.and_then(|h| h.cells.get(col)) {
            header_text = clean_tex(&text[cell.0..cell.1.min(text.len())]);
            words.extend(content_words(&header_text));
        }
        if col > 0
            && let Some(cell) = row.cells.first()
        {
            words.extend(content_words(&clean_tex(
                &text[cell.0..cell.1.min(text.len())],
            )));
        }
        let row_text = row
            .cells
            .iter()
            .map(|(from, to)| clean_tex(&text[*from..(*to).min(text.len())]))
            .collect::<Vec<_>>()
            .join(" | ");
        let label = if header_text.is_empty() {
            String::new()
        } else {
            format!(" (column \"{header_text}\")")
        };
        // An integer under an index header (`i`, `j`, `index`) labels a row.
        let cell_text = row
            .cells
            .get(col)
            .map(|(from, to)| clean_tex(&text[*from..(*to).min(text.len())]))
            .unwrap_or_default();
        let is_index = INDEX_HEADERS.contains(&header_text.to_lowercase().as_str())
            && !token.text.contains(['.', 'e', '-'])
            && cell_text.trim() == token.text;
        Place {
            kind: Kind::Cell,
            excerpt: format!("{}{label}", shorten(&row_text, 0, 160)),
            unit: row.cells.first().map_or(self.body.0, |c| c.0),
            words,
            notation: is_index,
        }
    }
}

/// The end of a definition that starts after its command word at `from`.
fn definition_end(text: &str, from: usize, limit: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = from;
    let mut groups = 0;
    while i < limit && groups < 3 {
        while i < limit && (bytes[i].is_ascii_whitespace() || bytes[i] == b'*') {
            i += 1;
        }
        match bytes.get(i) {
            Some(b'{') => {
                i = group_close(text, i) + 1;
                groups += 1;
            }
            Some(b'[') => {
                i += text[i..].find(']').map_or(1, |c| c + 1);
            }
            Some(b'\\') if groups == 0 => {
                // `\def\name{body}` and `\newcommand\name{body}`.
                i += 1;
                while bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
                    i += 1;
                }
            }
            _ => break,
        }
    }
    i.min(limit)
}

/// The index of the brace that closes the group opened at `open`.
fn group_close(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\\' => i += 1,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    text.len()
}

/// The tables of `text`: their rows, cells and captions.
fn tables(text: &str) -> Vec<Table> {
    let floats: Vec<Span> = FLOAT_BEGIN
        .find_iter(text)
        .map(|m| {
            let end = text[m.end()..]
                .find("\\end{table")
                .map_or(text.len(), |e| m.end() + e);
            (m.start(), end)
        })
        .collect();
    let mut out = Vec::new();
    for found in TABLE_BEGIN.captures_iter(text) {
        let whole = found.get(0).expect("group 0 always exists");
        let end_marker = format!("\\end{{{}}}", &found[1]);
        let body_end = text[whole.end()..]
            .find(&end_marker)
            .map_or(text.len(), |e| whole.end() + e);
        // The column specification (`{lrr}`, and the width of `tabularx`)
        // belongs to the opening line, not to the first cell.
        let spec_groups = if found[1].ends_with('*') || &found[1] == "tabularx" {
            2
        } else {
            1
        };
        let body = (
            skip_table_spec(text, whole.end(), spec_groups).min(body_end),
            body_end,
        );
        let mut caption = BTreeSet::new();
        if let Some((from, to)) = floats
            .iter()
            .filter(|(from, to)| *from <= whole.start() && body_end <= *to)
            .min_by_key(|(from, to)| to - from)
        {
            for cap in CAPTION.find_iter(&text[*from..*to]) {
                let open = from + cap.end() - 1;
                let close = group_close(text, open);
                caption.extend(content_words(&clean_tex(
                    &text[open + 1..close.min(text.len())],
                )));
            }
        }
        out.push(Table {
            body,
            rows: rows(text, body),
            caption,
        });
    }
    out
}

/// The index after the optional placement and the `groups` specification
/// groups that follow `\begin{tabular}` at `from`.
fn skip_table_spec(text: &str, from: usize, groups: usize) -> usize {
    let bytes = text.as_bytes();
    let blanks = |mut i: usize| {
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        i
    };
    let mut i = blanks(from);
    if bytes.get(i) == Some(&b'[')
        && let Some(close) = text[i..].find(']')
    {
        i = blanks(i + close + 1);
    }
    for _ in 0..groups {
        if bytes.get(i) != Some(&b'{') {
            break;
        }
        i = blanks(group_close(text, i) + 1);
    }
    i
}

/// The coordinates `(x,y)` and the options `[...]` of a TikZ drawing, outside
/// braces. They place shapes; the text of a node, which stands in braces, is
/// still read. Plot data in a `pgfplots` axis is not exempt, because a plot
/// prints measurements.
fn tikz_coordinates(text: &str) -> Vec<Span> {
    static PICTURE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)\\begin\s*\{tikzpicture\}.*?\\end\s*\{tikzpicture\}")
            .expect("pattern is valid")
    });
    static AXIS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)\\begin\s*\{(?:axis|semilogxaxis|semilogyaxis|loglogaxis|groupplot)\}.*?\\end\s*\{(?:axis|semilogxaxis|semilogyaxis|loglogaxis|groupplot)\}")
            .expect("pattern is valid")
    });
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for picture in PICTURE.find_iter(text) {
        let axes: Vec<Span> = AXIS
            .find_iter(&text[picture.range()])
            .map(|m| (picture.start() + m.start(), picture.start() + m.end()))
            .collect();
        let mut depth = 0usize;
        let mut i = picture.start();
        while i < picture.end() {
            if let Some((_, to)) = axes.iter().find(|(from, to)| *from <= i && i < *to) {
                i = *to;
                continue;
            }
            match bytes[i] {
                b'\\' => i += 1,
                b'{' => depth += 1,
                b'}' => depth = depth.saturating_sub(1),
                open @ (b'(' | b'[') if depth == 0 => {
                    let close = if open == b'(' { b')' } else { b']' };
                    if let Some(length) = bytes[i..picture.end()].iter().position(|b| *b == close) {
                        out.push((i, i + length + 1));
                        i += length;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    out
}

/// The ranges of inline and display math: `$...$`, `$$...$$`, `\(...\)`,
/// `\[...\]` and the equation-like environments.
fn math_spans(text: &str) -> Vec<Span> {
    static ENVIRONMENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?s)\\begin\s*\{(?:equation|align|gather|multline|eqnarray|displaymath|math|flalign)\*?\}.*?\\end\s*\{(?:equation|align|gather|multline|eqnarray|displaymath|math|flalign)\*?\}",
        )
        .expect("pattern is valid")
    });
    let mut spans: Vec<Span> = ENVIRONMENT
        .find_iter(text)
        .map(|m| (m.start(), m.end()))
        .collect();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => match bytes.get(i + 1) {
                Some(b'(') => {
                    let end = text[i..].find("\\)").map_or(text.len(), |e| i + e + 2);
                    spans.push((i, end));
                    i = end;
                }
                Some(b'[') => {
                    let end = text[i..].find("\\]").map_or(text.len(), |e| i + e + 2);
                    spans.push((i, end));
                    i = end;
                }
                _ => i += 2,
            },
            b'$' => {
                let display = bytes.get(i + 1) == Some(&b'$');
                let open = if display { i + 2 } else { i + 1 };
                let close = if display { "$$" } else { "$" };
                let mut end = text.len();
                let mut j = open;
                while j < bytes.len() {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j..].starts_with(close.as_bytes()) {
                        end = j + close.len();
                        break;
                    }
                    j += 1;
                }
                spans.push((i, end));
                i = end;
            }
            _ => i += 1,
        }
    }
    spans
}

/// Splits a tabular body into rows and cells at top-level `\\` and `&`.
fn rows(text: &str, (from, to): Span) -> Vec<Row> {
    let bytes = text.as_bytes();
    let mut rows = Vec::new();
    let mut cells: Vec<Span> = Vec::new();
    let mut cell_start = from;
    let mut depth = 0usize;
    let mut i = from;
    let finish_row = |cells: &mut Vec<Span>, rows: &mut Vec<Row>| {
        if cells
            .iter()
            .any(|(a, b)| !clean_tex(&text[*a..*b]).trim().is_empty())
        {
            rows.push(Row {
                cells: std::mem::take(cells),
            });
        } else {
            cells.clear();
        }
    };
    while i < to {
        match bytes[i] {
            b'\\' if bytes.get(i + 1) == Some(&b'\\') && depth == 0 => {
                cells.push((cell_start, i));
                finish_row(&mut cells, &mut rows);
                i += 2;
                // `\\[2pt]`
                if bytes.get(i) == Some(&b'[')
                    && let Some(close) = text[i..to].find(']')
                {
                    i += close + 1;
                }
                cell_start = i;
                continue;
            }
            b'\\' => i += 1,
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b'&' if depth == 0 => {
                cells.push((cell_start, i));
                cell_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    cells.push((cell_start, to));
    finish_row(&mut cells, &mut rows);
    rows
}

/// The starts of the sentences of `text`.
fn boundaries(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut out = BTreeSet::new();
    for found in BREAK_COMMAND.find_iter(text) {
        out.insert(found.start());
    }
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                // A blank line ends a paragraph.
                let mut j = i + 1;
                while bytes.get(j).is_some_and(|b| *b == b' ' || *b == b'\t') {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'\n') {
                    while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
                        j += 1;
                    }
                    out.insert(j);
                    i = j;
                    continue;
                }
            }
            b'.' | b'?' | b'!' => {
                let after = i + 1;
                let mut j = after;
                while bytes
                    .get(j)
                    .is_some_and(|b| *b == b' ' || *b == b'\t' || *b == b'\n')
                {
                    j += 1;
                }
                let ends = j > after
                    && text[j..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_uppercase() || c == '\\' || c == '$' || c == '(')
                    && !abbreviation_before(text, i);
                if ends {
                    out.insert(j);
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// True when the full stop at `dot` closes an abbreviation or an initial.
fn abbreviation_before(text: &str, dot: usize) -> bool {
    let head = &text[..dot];
    let start = head
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace() || matches!(c, '(' | '{' | '~'))
        .map_or(0, |(p, c)| p + c.len_utf8());
    let word = head[start..].to_lowercase();
    ABBREVIATIONS.contains(&word.as_str())
        || (word.chars().count() == 1 && word.chars().all(char::is_alphabetic))
}

// ---------------------------------------------------------------------------
// Words

/// Words too common to tie a number to a sentence.
const STOPWORDS: [&str; 74] = [
    "the", "and", "for", "with", "that", "this", "are", "was", "were", "has", "have", "had",
    "from", "into", "over", "under", "than", "then", "also", "each", "both", "such", "can", "not",
    "its", "our", "their", "which", "these", "those", "been", "being", "one", "per", "via",
    "using", "used", "use", "between", "within", "about", "across", "after", "before", "only",
    "more", "most", "less", "any", "all", "some", "other", "here", "there", "where", "when",
    "while", "whose", "what", "how", "would", "could", "should", "may", "might", "will", "shall",
    "table", "figure", "section", "paper", "but", "does", "did",
];

/// The words of `text` that can tie a number to a sentence: letters only, at
/// least three of them, lower case, without the common words, with a trailing
/// plural `s` removed.
#[must_use]
pub fn content_words(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut BTreeSet<String>| {
        if word.chars().count() >= 3 {
            let lower = word.to_lowercase();
            let stem = match lower.strip_suffix('s') {
                Some(stem) if stem.chars().count() >= 3 => stem.to_string(),
                _ => lower,
            };
            if !STOPWORDS.contains(&stem.as_str()) {
                out.insert(stem);
            }
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphabetic() {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// A macro or definition name split into words: `\bestScore` gives
/// `best Score`, `\best_score` gives `best score`.
fn split_name(definition: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in definition.chars() {
        if c.is_alphabetic() {
            if c.is_uppercase() && prev_lower {
                out.push(' ');
            }
            prev_lower = c.is_lowercase();
            out.push(c);
        } else {
            prev_lower = false;
            out.push(' ');
        }
    }
    out
}

/// TeX turned into plain words for messages and for [`content_words`]:
/// commands, braces and math marks removed, references blanked, spaces
/// collapsed.
#[must_use]
pub fn clean_tex(tex: &str) -> String {
    static COMMAND: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\\[A-Za-z]+\*?").expect("pattern is valid"));
    // `\begin{document}` and `\end{figure}` name an environment, not a word of
    // the text.
    static ENVIRONMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\\(?:begin|end)\s*\{[^}]*\}").expect("pattern is valid"));
    let blanked = numbers::blank_layout_arguments(tex);
    let blanked = ENVIRONMENT.replace_all(&blanked, " ");
    let without = COMMAND.replace_all(&blanked, " ");
    let mut out = String::with_capacity(without.len());
    for c in without.chars() {
        match c {
            '{' | '}' | '$' | '\\' | '~' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `width` characters of `text` around the character at `position`.
fn shorten(text: &str, position: usize, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    let start = position.saturating_sub(width / 2).min(chars.len() - width);
    let mut out: String = chars[start..start + width].iter().collect();
    if start > 0 {
        out.insert(0, '\u{2026}');
    }
    if start + width < chars.len() {
        out.push('\u{2026}');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printed(tex: &str) -> Vec<PrintedNumber> {
        let dir = tempfile::tempdir().expect("dir");
        Manuscript::from_main(dir.path(), tex).printed_numbers()
    }

    fn texts(found: &[PrintedNumber]) -> Vec<&str> {
        found.iter().map(|p| p.token.text.as_str()).collect()
    }

    #[test]
    fn only_the_document_body_prints_numbers() {
        let tex = "\\documentclass{article}\n\\usepackage[12pt]{geometry}\n\\title{A 26-Circle Study}\n\\newcommand{\\best}{2.6298}\n\\author{Jane 5}\n\\begin{document}\nThe score is 2.6298.\n\\end{document}\n99 trailing\n";
        let found = printed(tex);
        assert_eq!(texts(&found), ["26", "2.6298", "2.6298"]);
        assert_eq!(
            found.iter().map(|p| p.kind).collect::<Vec<_>>(),
            [Kind::Title, Kind::Macro, Kind::Prose]
        );
    }

    #[test]
    fn each_number_knows_its_file_and_line() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::create_dir(dir.path().join("tables")).expect("dir");
        std::fs::write(
            dir.path().join("tables/t.tex"),
            "\\begin{tabular}{rr}\nA & B \\\\\n1.5 & 2.5 \\\\\n\\end{tabular}\n",
        )
        .expect("write");
        let tex = "\\begin{document}\nFirst 7 here.\n\\begin{table}\n\\input{tables/t.tex}\n\\caption{Two numbers.}\n\\end{table}\n\\end{document}\n";
        let found = Manuscript::from_main(dir.path(), tex).printed_numbers();
        let places: Vec<(&str, &str, usize)> = found
            .iter()
            .map(|p| (p.token.text.as_str(), p.file.as_str(), p.line))
            .collect();
        assert_eq!(
            places,
            [
                ("7", "paper.tex", 2),
                ("1.5", "tables/t.tex", 3),
                ("2.5", "tables/t.tex", 3)
            ]
        );
        assert!(found[1..].iter().all(|p| p.kind == Kind::Cell));
        // The caption of the enclosing table ties the cells.
        assert!(found[1].words.contains("number"), "{:?}", found[1].words);
    }

    #[test]
    fn a_cell_knows_its_row_and_column() {
        let tex = "\\begin{document}\n\\begin{table}\n\\caption{Contact sweep.}\n\\begin{tabular}{lrr}\nTolerance & Circle & Wall \\\\\n\\midrule\nloose & 57 & 21 \\\\\n\\end{tabular}\n\\end{table}\n\\end{document}\n";
        let found = printed(tex);
        let circle = found.iter().find(|p| p.token.text == "57").expect("57");
        assert_eq!(circle.kind, Kind::Cell);
        assert!(circle.words.contains("circle"));
        assert!(circle.words.contains("contact"));
        assert!(circle.words.contains("loose"), "row label");
        assert!(
            !circle.words.contains("tolerance"),
            "only the column header of its own column"
        );
        assert!(!circle.words.contains("wall"));
        assert!(circle.excerpt.contains("57"), "{}", circle.excerpt);
        let wall = found.iter().find(|p| p.token.text == "21").expect("21");
        assert!(wall.words.contains("wall"));
    }

    #[test]
    fn sentences_split_at_full_stops_and_keep_decimals_and_abbreviations() {
        let tex = "\\begin{document}\nWe reach 2.6298 (e.g. on 26 circles). A second sentence prints 57 contacts. Fig. 3 shows 0.5.\n\\end{document}\n";
        let found = printed(tex);
        assert_eq!(texts(&found), ["2.6298", "26", "57", "0.5"]);
        assert_eq!(found[0].unit, found[1].unit);
        assert_ne!(found[1].unit, found[2].unit);
        assert!(found[2].words.contains("contact"));
        assert!(!found[2].words.contains("circle"));
    }

    #[test]
    fn captions_and_comments_are_read_as_the_compiler_reads_them() {
        let tex = "\\begin{document}\n% 11 in a comment\nBody.\\footnote{Note 3 here.}\n\\begin{figure}\n\\caption{Score 2.5 of the best run.}\n\\end{figure}\n\\end{document}\n";
        let found = printed(tex);
        assert_eq!(texts(&found), ["3", "2.5"]);
        assert_eq!(found[1].kind, Kind::Caption);
    }

    #[test]
    fn an_import_outside_the_folder_is_a_problem_and_is_not_read() {
        let dir = tempfile::tempdir().expect("dir");
        let manuscript = Manuscript::from_main(dir.path(), "\\input{../secret.tex}\nbody 5");
        assert_eq!(manuscript.problems.len(), 1, "{:?}", manuscript.problems);
        assert_eq!(manuscript.files, ["paper.tex"]);
    }

    #[test]
    fn a_command_that_loads_a_file_the_check_does_not_follow_is_a_problem() {
        let dir = tempfile::tempdir().expect("dir");
        let tex = "\\begin{document}\n\\subfile{results}\n\\lstinputlisting{run.log}\n\\begin{verbatim}\n\\verbatiminput{x}\n\\end{verbatim}\n\\end{document}\n";
        let manuscript = Manuscript::from_main(dir.path(), tex);
        assert!(manuscript.problems.is_empty());
        assert_eq!(manuscript.unread.len(), 2, "{:?}", manuscript.unread);
        assert!(
            manuscript.unread[0].contains("\\subfile"),
            "{:?}",
            manuscript.unread
        );
        assert!(
            manuscript.unread[0].contains("line 2"),
            "{:?}",
            manuscript.unread
        );
        assert!(manuscript.unread[1].contains("\\lstinputlisting"));
    }

    #[test]
    fn structure_integers_in_math_and_row_labels_are_notation_but_results_are_not() {
        let tex = "\\begin{document}\nThe packing holds 2 circles. Constraints $x_i \\ge 0$ and $y \\le 1$ hold for $i=1,\\ldots,26$ with $d=104$.\n\\begin{tabular}{rr}\n$i$ & Radius \\\\\n7 & 0.0663 \\\\\n\\end{tabular}\n\\end{document}\n";
        let found = printed(tex);
        assert_eq!(texts(&found), ["2", "26", "104", "0.0663"], "{found:?}");
    }

    #[test]
    fn the_column_specification_is_not_read_as_a_header() {
        let tex = "\\begin{document}\n\\begin{tabular}{lr@{\\hspace{14pt}}r}\nName & Score & Count \\\\\nrun & 2.5 & 7 \\\\\n\\end{tabular}\n\\end{document}\n";
        let found = printed(tex);
        let score = found.iter().find(|p| p.token.text == "2.5").expect("2.5");
        assert!(score.words.contains("score"));
        assert!(!score.words.contains("count"));
        assert!(
            score.excerpt.contains("column \"Score\""),
            "{}",
            score.excerpt
        );
    }

    #[test]
    fn an_equation_takes_its_words_from_the_sentences_around_it() {
        let tex = "\\begin{document}\nThe packing must satisfy the wall constraints for every circle.\n\\begin{equation}\nd = 4 \\cdot 26\n\\end{equation}\nThese wall constraints are counted below.\n\\end{document}\n";
        let found = printed(tex);
        assert!(found[0].words.contains("wall"), "{:?}", found[0].words);
        assert!(found[0].words.contains("constraint"));
    }

    #[test]
    fn verbatim_text_is_printed_so_its_numbers_count() {
        let tex = "\\begin{document}\nSee the listing.\n\\begin{verbatim}\nbudget = 40.5 % seconds\n\\end{verbatim}\n\\end{document}\n";
        assert_eq!(texts(&printed(tex)), ["40.5"]);
    }

    #[test]
    fn drawing_coordinates_are_layout_but_node_text_and_plot_data_are_not() {
        let tex = "\\begin{document}\n\\begin{tikzpicture}[scale=0.5]\n\\draw[thick] (0,0) -- (3.5,2.25) node[above] {Best 2.6298};\n\\begin{axis}[width=5cm]\n\\addplot coordinates {(1,2.5) (2,2.6)};\n\\end{axis}\n\\end{tikzpicture}\n\\end{document}\n";
        // The coordinates of the drawing are layout; the node text and the
        // points of the plot, which are measurements, are read.
        assert_eq!(texts(&printed(tex)), ["2.6298", "1", "2.5", "2", "2.6"]);
    }

    #[test]
    fn text_with_multi_byte_characters_is_read_without_shifting_places() {
        let tex = "\\begin{document}\nCaf\u{e9} \u{2212} the r\u{e9}sum\u{e9}: $x \u{2264} 1$ and 7 items.\n\\begin{equation}\n\\alpha = \u{3b1} + 3.5\n\\end{equation}\n\\end{document}\n";
        let found = printed(tex);
        assert_eq!(texts(&found), ["7", "3.5"], "{found:?}");
        assert!(found[0].excerpt.contains("7 items"), "{}", found[0].excerpt);
        assert_eq!(found[1].line, 4);
    }

    #[test]
    fn odd_tex_never_stops_the_check() {
        // Pieces that open groups, environments, math and imports without
        // closing them, and characters of several widths.
        let pieces = [
            "\\begin{document}",
            "\\end{document}",
            "\\begin{tabular}{ll}",
            "\\end{tabular}",
            "\\begin{table}",
            "\\caption{",
            "\\caption[",
            "\\input{",
            "\\iffalse",
            "\\fi",
            "{",
            "}",
            "[",
            "]",
            "(",
            ")",
            "$",
            "$$",
            "\\[",
            "\\]",
            "\\\\",
            "&",
            "%",
            "^",
            "_",
            ".",
            ",",
            "-",
            "\\%",
            "\\times",
            "10^{-3}",
            "e-5",
            "12",
            "3.5",
            "0",
            "-7",
            ".5",
            "\u{e9}",
            "\u{2212}",
            "\u{1f600}",
            "\u{ff13}",
            " ",
            "\n",
            "\n\n",
            "word",
            "Table",
            "\\item",
            "\\begin{verbatim}",
            "\\end{verbatim}",
            "\\newcommand{\\a}{",
            "\\title{",
            "\\draw (",
            "\\begin{tikzpicture}",
        ];
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let dir = tempfile::tempdir().expect("dir");
        for _ in 0..3000 {
            let count = 1 + next() % 60;
            let tex: String = (0..count)
                .map(|_| pieces[(next() % pieces.len() as u64) as usize])
                .collect();
            let found = Manuscript::from_main(dir.path(), &tex).printed_numbers();
            for number in &found {
                assert!(number.line >= 1);
            }
            let _ = numbers::extract(&tex);
            let _ = clean_tex(&tex);
        }
    }

    #[test]
    fn content_words_drop_short_common_words_and_plurals() {
        let words = content_words("The contacts of Circles 7 are in the table");
        assert_eq!(words.into_iter().collect::<Vec<_>>(), ["circle", "contact"]);
    }
}
