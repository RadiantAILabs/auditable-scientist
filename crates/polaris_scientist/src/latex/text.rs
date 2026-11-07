//! Reading helpers for LaTeX and BibTeX text.
//!
//! These are deliberately small and conservative: they answer structural
//! questions about a manuscript (which sections exist, which keys are cited,
//! which files are imported) without trying to be a TeX parser. Anything they
//! cannot decide statically, such as an import whose target is built by a
//! macro, is reported so a caller can refuse it.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

/// Section names a manuscript must contain, with the pattern that finds each.
///
/// The order is the order of the original manuscript layout and is the order
/// in which missing sections are reported.
pub const REQUIRED_SECTIONS: [(&str, &str); 7] = [
    (
        "abstract",
        r"(?i)\\begin\{abstract\}|\\section\*?\{[^}]*abstract[^}]*\}",
    ),
    (
        "introduction",
        r"(?i)\\section\*?\{[^}]*introduction[^}]*\}",
    ),
    (
        "related work",
        r"(?i)\\section\*?\{[^}]*related work[^}]*\}",
    ),
    ("method", r"(?i)\\section\*?\{[^}]*method[^}]*\}"),
    ("results", r"(?i)\\section\*?\{[^}]*results[^}]*\}"),
    ("conclusion", r"(?i)\\section\*?\{[^}]*conclusion[^}]*\}"),
    (
        "references",
        r"(?i)\\bibliography\{|\\printbibliography|\\begin\{thebibliography\}|\\section\*?\{[^}]*references[^}]*\}",
    ),
];

static SECTION_REGEXES: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    REQUIRED_SECTIONS
        .iter()
        .map(|(name, pattern)| {
            (
                *name,
                Regex::new(pattern).expect("section pattern is valid"),
            )
        })
        .collect()
});

static BIB_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@\w+\{([^,]+),").expect("bib key pattern is valid"));

static CITE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\cite[a-zA-Z*]*\{([^}]+)\}").expect("cite pattern is valid"));

static EVIDENCE_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@evidence\[\[([^\]]+)\]\]").expect("evidence token pattern is valid")
});

/// Names of the required sections that `tex` does not contain.
#[must_use]
pub fn missing_sections(tex: &str) -> Vec<&'static str> {
    SECTION_REGEXES
        .iter()
        .filter(|(_, re)| !re.is_match(tex))
        .map(|(name, _)| *name)
        .collect()
}

/// Citation keys defined by a BibTeX file.
#[must_use]
pub fn bib_keys(bib: &str) -> BTreeSet<String> {
    BIB_KEY
        .captures_iter(bib)
        .map(|c| c[1].trim().to_string())
        .filter(|key| !key.is_empty())
        .collect()
}

/// Citation keys used by `\cite`-style commands in `tex`.
///
/// Pass text with comments already removed; a commented-out citation is not a
/// citation.
#[must_use]
pub fn cited_keys(tex: &str) -> BTreeSet<String> {
    CITE.captures_iter(tex)
        .flat_map(|c| {
            c[1].split(',')
                .map(|k| k.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|key| !key.is_empty())
        .collect()
}

/// One `@evidence[[target#locator]]` token found in a text file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceToken {
    /// The text between the double brackets.
    pub raw: String,
    /// The part before the first `#`: a path or identifier.
    pub target: String,
    /// The part after the first `#`, when present.
    pub locator: Option<String>,
    /// 1-based line number in the scanned text.
    pub line: usize,
}

/// Every `@evidence[[…]]` token in `text`, in order of appearance.
#[must_use]
pub fn evidence_tokens(text: &str) -> Vec<EvidenceToken> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        for caps in EVIDENCE_REF.captures_iter(line) {
            let raw = caps[1].trim().to_string();
            let (target, locator) = match raw.split_once('#') {
                Some((t, l)) => (t.trim().to_string(), Some(l.trim().to_string())),
                None => (raw.clone(), None),
            };
            out.push(EvidenceToken {
                raw,
                target,
                locator,
                line: idx + 1,
            });
        }
    }
    out
}

/// True when the character at byte `index` of `line` is escaped by an odd
/// number of backslashes.
fn is_escaped(line: &str, index: usize) -> bool {
    line.as_bytes()[..index]
        .iter()
        .rev()
        .take_while(|b| **b == b'\\')
        .count()
        % 2
        == 1
}

/// Removes LaTeX comments, keeping `\%` and the text inside `\verb` spans.
///
/// Line structure is preserved, so line numbers in the result match the input.
#[must_use]
pub fn strip_comments(tex: &str) -> String {
    let mut lines = Vec::new();
    for line in tex.lines() {
        let bytes = line.as_bytes();
        let mut end = line.len();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(b"\\verb") {
                let mut d = i + 5;
                if bytes.get(d) == Some(&b'*') {
                    d += 1;
                }
                if let Some(&delim) = bytes.get(d)
                    && delim.is_ascii()
                    && !delim.is_ascii_whitespace()
                    && !delim.is_ascii_alphabetic()
                    && let Some(close) = line[d + 1..].find(delim as char)
                {
                    i = d + 1 + close + 1;
                    continue;
                }
            }
            if bytes[i] == b'%' && !is_escaped(line, i) {
                end = i;
                break;
            }
            i += 1;
        }
        lines.push(&line[..end]);
    }
    lines.join("\n")
}

/// Replaces `verbatim`-style environments and `\verb` spans with spaces, so
/// commands written inside them are not mistaken for live TeX.
#[must_use]
pub fn mask_verbatim(tex: &str) -> String {
    static ENV: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)\\begin\{(verbatim\*?|lstlisting|minted)\}.*?\\end\{(verbatim\*?|lstlisting|minted)\}")
            .expect("verbatim pattern is valid")
    });
    // The `regex` crate has no back-references, so `\verb` spans are found by hand.
    let masked = ENV
        .replace_all(tex, |caps: &regex::Captures<'_>| blank(&caps[0]))
        .into_owned();
    let mut out = String::with_capacity(masked.len());
    let mut rest = masked.as_str();
    while let Some(pos) = rest.find("\\verb") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 5..];
        let star = usize::from(after.starts_with('*'));
        let tail = &after[star..];
        match tail.chars().next() {
            Some(delim) if !delim.is_whitespace() && !delim.is_alphabetic() => {
                let body = &tail[delim.len_utf8()..];
                if let Some(close) = body.find(delim) {
                    let total = 5 + star + delim.len_utf8() + close + delim.len_utf8();
                    out.push_str(&blank(&rest[pos..pos + total]));
                    rest = &rest[pos + total..];
                    continue;
                }
            }
            _ => {}
        }
        out.push_str("\\verb");
        rest = after;
    }
    out.push_str(rest);
    out
}

fn blank(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' { '\n' } else { ' ' })
        .collect()
}

/// Removes text between `\iffalse` and its matching `\fi`.
#[must_use]
pub fn strip_false_conditionals(tex: &str) -> String {
    static IF_OR_FI: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\(if[A-Za-z@]*|fi)(?:[^A-Za-z@]|$)").expect("conditional pattern is valid")
    });
    let mut out = String::with_capacity(tex.len());
    let mut cursor = 0;
    while let Some(start) = tex[cursor..].find("\\iffalse") {
        let start = cursor + start;
        let after = start + "\\iffalse".len();
        if tex[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '@')
        {
            out.push_str(&tex[cursor..after]);
            cursor = after;
            continue;
        }
        out.push_str(&tex[cursor..start]);
        let mut depth = 1usize;
        let mut end = tex.len();
        for m in IF_OR_FI.captures_iter(&tex[after..]) {
            let whole = m.get(0).expect("group 0 always exists");
            if &m[1] == "iff" {
                continue;
            }
            if &m[1] == "fi" {
                depth -= 1;
                if depth == 0 {
                    end = after + whole.start() + 3;
                    break;
                }
            } else {
                depth += 1;
            }
        }
        out.push_str(&blank(&tex[start..end]));
        cursor = end;
    }
    out.push_str(&tex[cursor..]);
    out
}

/// Targets of `\input` and `\include` and whether any import could not be
/// resolved statically.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imports {
    /// Literal targets, as written.
    pub inputs: Vec<String>,
    /// Literal `\usepackage`, `\RequirePackage`, `\documentclass` and
    /// `\LoadClass` targets with the extension the command implies.
    pub support: Vec<String>,
    /// Imports whose target is built by a macro or cannot be read.
    pub dynamic: Vec<String>,
}

/// Finds the imports of one TeX source, after comment and verbatim removal.
#[must_use]
pub fn imports(tex: &str) -> Imports {
    static INPUT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\(?:input|include)(?:\s*\{([^{}]+)\}|\s+([^\s%{}]+))")
            .expect("input pattern is valid")
    });
    static INPUT_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\(?:input|include)(?:[^A-Za-z@]|$)").expect("pattern is valid")
    });
    static SUPPORT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\(usepackage|RequirePackage|documentclass|LoadClass)\s*(?:\[[^\]]*\])?\s*\{([^{}]+)\}")
            .expect("support pattern is valid")
    });
    static SUPPORT_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\(?:usepackage|RequirePackage|documentclass|LoadClass)(?:[^A-Za-z@]|$)")
            .expect("pattern is valid")
    });
    static CSNAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\csname\s*(?:input|include)\s*\\endcsname").expect("pattern is valid")
    });

    let text = strip_false_conditionals(&mask_verbatim(&strip_comments(tex)));
    let mut found = Imports::default();
    if CSNAME.is_match(&text) {
        found.dynamic.push("\\csname input\\endcsname".to_string());
    }
    let parsed_inputs: BTreeSet<usize> = INPUT.find_iter(&text).map(|m| m.start()).collect();
    for m in INPUT_COMMAND.find_iter(&text) {
        if !parsed_inputs.contains(&m.start()) {
            found.dynamic.push(
                text[m.start()..]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            );
        }
    }
    for caps in INPUT.captures_iter(&text) {
        let target = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map_or("", |m| m.as_str())
            .trim()
            .to_string();
        if target.contains('\\') || target.contains('#') {
            found.dynamic.push(target);
        } else {
            found.inputs.push(target);
        }
    }
    let parsed_support: BTreeSet<usize> = SUPPORT.find_iter(&text).map(|m| m.start()).collect();
    for m in SUPPORT_COMMAND.find_iter(&text) {
        if !parsed_support.contains(&m.start()) {
            found.dynamic.push(
                text[m.start()..]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            );
        }
    }
    for caps in SUPPORT.captures_iter(&text) {
        let suffix = if caps[1].to_ascii_lowercase().contains("class") {
            "cls"
        } else {
            "sty"
        };
        for raw in caps[2].split(',') {
            let target = raw.trim();
            if target.contains('\\') || target.contains('#') {
                found.dynamic.push(target.to_string());
            } else if !target.is_empty() {
                found.support.push(
                    if target.rsplit('/').next().is_some_and(|n| n.contains('.')) {
                        target.to_string()
                    } else {
                        format!("{target}.{suffix}")
                    },
                );
            }
        }
    }
    found
}

/// Bibliography declarations of one TeX source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bibliography {
    /// Databases named by `\bibliography` and `\addbibresource`, as written.
    pub databases: Vec<String>,
    /// Styles named by `\bibliographystyle`, as written.
    pub styles: Vec<String>,
    /// Declarations whose target is built by a macro.
    pub dynamic: Vec<String>,
}

/// Finds bibliography databases and styles, after comment and verbatim removal.
#[must_use]
pub fn bibliography(tex: &str) -> Bibliography {
    static DATABASE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\\bibliography\s*\{([^{}]+)\}").expect("pattern is valid"));
    static RESOURCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\addbibresource\s*(?:\[[^\]]*\])?\s*\{([^{}]+)\}").expect("pattern is valid")
    });
    static STYLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\bibliographystyle\s*\{([^{}]+)\}").expect("pattern is valid")
    });
    static ANY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\(?:bibliography|addbibresource|bibliographystyle)(?:[^A-Za-z@]|$)")
            .expect("pattern is valid")
    });
    let text = strip_false_conditionals(&mask_verbatim(&strip_comments(tex)));
    let mut found = Bibliography::default();
    let mut parsed: BTreeSet<usize> = BTreeSet::new();
    for caps in DATABASE.captures_iter(&text) {
        parsed.insert(caps.get(0).expect("group 0 always exists").start());
        for target in caps[1].split(',') {
            push_target(&mut found.databases, &mut found.dynamic, target);
        }
    }
    for caps in RESOURCE.captures_iter(&text) {
        parsed.insert(caps.get(0).expect("group 0 always exists").start());
        push_target(&mut found.databases, &mut found.dynamic, &caps[1]);
    }
    for caps in STYLE.captures_iter(&text) {
        parsed.insert(caps.get(0).expect("group 0 always exists").start());
        push_target(&mut found.styles, &mut found.dynamic, &caps[1]);
    }
    for m in ANY.find_iter(&text) {
        if !parsed.contains(&m.start()) {
            found.dynamic.push(
                text[m.start()..]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            );
        }
    }
    found
}

fn push_target(into: &mut Vec<String>, dynamic: &mut Vec<String>, raw: &str) {
    let target = raw.trim();
    if target.contains('\\') || target.contains('#') {
        dynamic.push(target.to_string());
    } else if !target.is_empty() {
        into.push(target.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_removed_but_escaped_percent_stays() {
        let out = strip_comments("a 5\\% b % note\nc \\verb|%x| d % e\n");
        assert_eq!(out, "a 5\\% b \nc \\verb|%x| d ");
    }

    #[test]
    fn false_conditional_blocks_are_blanked_with_nesting() {
        let out = strip_false_conditionals("a \\iffalse x \\ifx y \\fi z \\fi b");
        assert!(out.starts_with("a "));
        assert!(out.ends_with(" b"));
        assert!(!out.contains('x') && !out.contains('z'));
    }

    #[test]
    fn imports_split_literal_and_dynamic() {
        let found = imports(
            "\\input{sec/intro}\n\\input{\\jobname-x}\n\\usepackage[a]{paper,times}\n\\documentclass{article}",
        );
        assert_eq!(found.inputs, ["sec/intro"]);
        assert_eq!(found.support, ["paper.sty", "times.sty", "article.cls"]);
        assert_eq!(found.dynamic, ["\\jobname-x"]);
    }

    #[test]
    fn bibliography_finds_database_and_style() {
        let found =
            bibliography("\\bibliographystyle{iclr2026_conference}\n\\bibliography{references}");
        assert_eq!(found.databases, ["references"]);
        assert_eq!(found.styles, ["iclr2026_conference"]);
        assert!(found.dynamic.is_empty());
    }

    #[test]
    fn evidence_tokens_carry_line_numbers_and_split_locator() {
        let tokens = evidence_tokens("x\ny % @evidence[[paper_data.json#grounding_sources.a.b]]\n");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].line, 2);
        assert_eq!(tokens[0].target, "paper_data.json");
        assert_eq!(tokens[0].locator.as_deref(), Some("grounding_sources.a.b"));
    }

    #[test]
    fn section_check_reports_missing_names() {
        let tex =
            "\\begin{abstract}\\end{abstract}\\section{Introduction}\\bibliography{references}";
        let missing = missing_sections(tex);
        assert_eq!(missing, ["related work", "method", "results", "conclusion"]);
    }
}
