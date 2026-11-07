//! Reading numbers out of prose, TeX and claim sentences.
//!
//! [`extract`] returns every number a text prints, with its sign, the digits it
//! prints and where it sits. The check that ties those numbers to recorded
//! values is in [`super::check`]; this module only reads.
//!
//! A number counts when a reader sees it as a measurement, a count, a setting
//! or a derived quantity. Numbers that only name a place or a layout are left
//! out: a section, table, figure or equation number, a year, a citation number
//! in square brackets, an index or exponent in a formula, a length (`14pt`,
//! `0.8\textwidth`), a number glued to letters (`H1`, `3D`, `eval_2`) and the
//! arguments of layout and colour commands. A power of ten (`10^{-11}`,
//! `1.5\times10^{-3}`, `1e-9`) is one number.
//!
//! The sign counts. A minus written directly before the digits (`-0.0025`,
//! `$-0.0025$`, `{-}0.0025`, U+2212) makes the number negative, except when it
//! joins two numbers (`0.04--0.42`) or follows a word (`step-3`).

/// One number written in text.
#[derive(Clone, Debug, PartialEq)]
pub struct NumberToken {
    /// The number as printed: its sign when negative, its digits and its
    /// exponent, without thousands separators.
    pub text: String,
    /// The signed value.
    pub value: f64,
    /// Half of the last printed digit's place value; the rounding slack.
    pub half_unit: f64,
    /// True when the number is followed by a percent sign.
    pub percent: bool,
    /// Byte offset of the first character, the sign when there is one.
    pub start: usize,
    /// Byte offset after the last character, the percent sign included.
    pub end: usize,
}

impl NumberToken {
    /// Whether `value` prints as this number: the same sign, and equal after
    /// rounding to the printed digits. A number followed by `%` may also be the
    /// recorded value times one hundred.
    #[must_use]
    pub fn matches(&self, value: f64) -> bool {
        self.distance(value) <= self.slack()
    }

    /// Distance between this number and the nearest reading of `value`.
    #[must_use]
    pub fn distance(&self, value: f64) -> f64 {
        let plain = (value - self.value).abs();
        if self.percent {
            plain.min((value * 100.0 - self.value).abs())
        } else {
            plain
        }
    }

    fn slack(&self) -> f64 {
        self.half_unit * (1.0 + 1e-9) + 1e-12
    }

    /// Whether `other` prints the same signed quantity as this number.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        (self.value - other.value).abs() <= 1e-9 * self.value.abs().max(1.0)
    }
}

/// Words that put a counter after them: `Table 2`, `Section 4`. The number
/// after such a word names a place in the document, not a measurement.
const LABEL_WORDS: [&str; 18] = [
    "table",
    "tables",
    "figure",
    "figures",
    "fig",
    "section",
    "sections",
    "sec",
    "appendix",
    "eq",
    "equation",
    "algorithm",
    "line",
    "step",
    "hypothesis",
    "theorem",
    "lemma",
    "page",
];

/// Letters that may follow digits without making the number an identifier or a
/// measurement: `3D`, `2nd`, `14pt`.
const SUFFIXES_THAT_HIDE: [&str; 15] = [
    "D", "d", "st", "nd", "rd", "th", "pt", "cm", "mm", "em", "ex", "in", "bp", "pc", "sp",
];

/// Length macros: `0.8\textwidth`.
const LENGTH_MACROS: [&str; 9] = [
    "textwidth",
    "linewidth",
    "columnwidth",
    "textheight",
    "paperwidth",
    "paperheight",
    "hsize",
    "baselineskip",
    "unitlength",
];

/// Commands whose arguments are layout, colour or cross-reference settings,
/// not text, with the number of braced arguments each takes. Their arguments
/// are blanked before numbers are read. The text of `\multicolumn`,
/// `\multirow`, `\textcolor` and `\scalebox` follows the counted arguments and
/// stays readable.
const LAYOUT_COMMANDS: [(&str, usize); 36] = [
    // `\item[1.]`: the optional label of a list item.
    ("item", 0),
    ("ref", 1),
    ("eqref", 1),
    ("label", 1),
    ("pageref", 1),
    ("autoref", 1),
    ("cref", 1),
    ("Cref", 1),
    ("nameref", 1),
    ("includegraphics", 1),
    ("input", 1),
    ("include", 1),
    ("bibliography", 1),
    ("bibliographystyle", 1),
    ("usepackage", 1),
    ("documentclass", 1),
    ("url", 1),
    ("href", 1),
    ("vspace", 1),
    ("hspace", 1),
    ("setlength", 2),
    ("addtolength", 2),
    ("setcounter", 2),
    ("addtocounter", 2),
    ("scalebox", 1),
    ("resizebox", 2),
    ("rule", 2),
    ("fontsize", 2),
    ("definecolor", 3),
    ("color", 1),
    ("textcolor", 1),
    ("colorbox", 1),
    ("fcolorbox", 2),
    ("cmidrule", 1),
    ("multicolumn", 2),
    ("multirow", 2),
];

/// Environments whose opening line carries a layout specification, with the
/// number of braced arguments after the name. Floats take only an optional
/// placement.
const LAYOUT_ENVIRONMENTS: [(&str, usize); 12] = [
    ("tabular", 1),
    ("tabular*", 2),
    ("tabularx", 2),
    ("longtable", 1),
    ("array", 1),
    ("minipage", 1),
    ("wrapfigure", 2),
    ("figure", 0),
    ("figure*", 0),
    ("table", 0),
    ("table*", 0),
    ("adjustbox", 0),
];

/// Numbers written in `text`, in order of appearance.
///
/// See the module documentation for what counts as a number.
#[must_use]
pub fn extract(text: &str) -> Vec<NumberToken> {
    let blanked = blank_layout_arguments(text);
    Scanner::new(&blanked).run()
}

/// Replaces the arguments of layout commands by spaces, keeping every byte
/// offset of the rest.
pub(crate) fn blank_layout_arguments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        let name_start = i + 1;
        let mut j = name_start;
        while bytes.get(j).is_some_and(u8::is_ascii_alphabetic) {
            j += 1;
        }
        if j == name_start {
            // `\\[2pt]` is a line break that carries a length.
            if bytes.get(name_start) == Some(&b'\\') {
                let k = skip_blanks(bytes, name_start + 1);
                if bytes.get(k) == Some(&b'[')
                    && let Some(close) = text[k..].find(']')
                {
                    ranges.push((k, k + close + 1));
                    i = k + close + 1;
                    continue;
                }
            }
            i = name_start + 1;
            continue;
        }
        let name = &text[name_start..j];
        let (count, mut k) = if name == "begin" {
            let open = skip_blanks(bytes, j);
            let Some(close) = text[open..]
                .find('}')
                .filter(|_| bytes.get(open) == Some(&b'{'))
            else {
                i = j;
                continue;
            };
            let env = &text[open + 1..open + close];
            let Some((_, count)) = LAYOUT_ENVIRONMENTS.iter().find(|(e, _)| *e == env) else {
                i = open + close + 1;
                continue;
            };
            (*count, open + close + 1)
        } else if let Some((_, count)) = LAYOUT_COMMANDS
            .iter()
            .find(|(command, _)| *command == name)
            .or_else(|| name.starts_with("cite").then_some(&("cite", 1)))
        {
            (*count, j)
        } else {
            i = j;
            continue;
        };
        // `\command*`, then `(..)`, then `[..]`, then the counted groups.
        if bytes.get(k) == Some(&b'*') {
            k += 1;
        }
        k = skip_blanks(bytes, k);
        if bytes.get(k) == Some(&b'(')
            && let Some(close) = text[k..].find(')')
        {
            k = skip_blanks(bytes, k + close + 1);
        }
        if bytes.get(k) == Some(&b'[')
            && let Some(close) = text[k..].find(']')
        {
            k = skip_blanks(bytes, k + close + 1);
        }
        for _ in 0..count {
            k = skip_blanks(bytes, k);
            if bytes.get(k) != Some(&b'{') {
                break;
            }
            k = group_end(bytes, k);
        }
        ranges.push((i, k));
        i = k.max(i + 1);
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (from, to) in ranges {
        if from < cursor || to > text.len() {
            continue;
        }
        out.push_str(&text[cursor..from]);
        out.push_str(&spaces(&text[from..to]));
        cursor = to;
    }
    out.push_str(&text[cursor..]);
    out
}

fn skip_blanks(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// The index after the balanced group that opens at `open`.
fn group_end(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while let Some(&b) = bytes.get(i) {
        i += 1;
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    i
}

fn spaces(text: &str) -> String {
    text.chars()
        .flat_map(|c| std::iter::repeat_n(if c == '\n' { '\n' } else { ' ' }, c.len_utf8()))
        .collect()
}

struct Scanner {
    chars: Vec<char>,
    /// Byte offset of each character, and the length of the text at the end.
    offsets: Vec<usize>,
}

impl Scanner {
    fn new(text: &str) -> Self {
        let mut chars = Vec::with_capacity(text.len());
        let mut offsets = Vec::with_capacity(text.len() + 1);
        for (offset, c) in text.char_indices() {
            chars.push(ascii_digit(c).unwrap_or(c));
            offsets.push(offset);
        }
        offsets.push(text.len());
        Self { chars, offsets }
    }

    fn at(&self, i: usize) -> Option<char> {
        self.chars.get(i).copied()
    }

    fn before(&self, i: usize, back: usize) -> Option<char> {
        i.checked_sub(back).and_then(|p| self.at(p))
    }

    fn offset(&self, i: usize) -> usize {
        self.offsets[i.min(self.chars.len())]
    }

    fn run(&self) -> Vec<NumberToken> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.chars.len() {
            let c = self.chars[i];
            if (c == '_' || c == '^') && self.before(i, 1) != Some('\\') {
                i = self.skip_script_argument(i + 1);
                continue;
            }
            if !c.is_ascii_digit() {
                i += 1;
                continue;
            }
            let (next, token) = self.number_at(i);
            out.extend(token);
            i = next.max(i + 1);
        }
        out
    }

    /// Skips the argument of `_` or `^`: a braced group or one character.
    fn skip_script_argument(&self, mut i: usize) -> usize {
        while self.at(i).is_some_and(char::is_whitespace) {
            i += 1;
        }
        match self.at(i) {
            Some('{') => {
                let mut depth = 0usize;
                while let Some(c) = self.at(i) {
                    i += 1;
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                i
            }
            Some('\\') => {
                i += 1;
                while self.at(i).is_some_and(|c| c.is_ascii_alphabetic()) {
                    i += 1;
                }
                i
            }
            Some(_) => i + 1,
            None => i,
        }
    }

    /// How many characters a thousands separator written at `i` takes, when
    /// exactly three digits follow it and the number so far has at most three.
    fn group_follows(&self, i: usize, int_len: usize) -> Option<usize> {
        if int_len == 0 || int_len > 3 {
            return None;
        }
        let skip = if self.at(i) == Some(',') {
            1
        } else if self.at(i) == Some('{')
            && self.at(i + 1) == Some(',')
            && self.at(i + 2) == Some('}')
        {
            3
        } else if self.at(i) == Some('\\') && self.at(i + 1) == Some(',') {
            2
        } else if self.at(i) == Some('\u{2009}') {
            1
        } else {
            return None;
        };
        let digits_ok = (0..3).all(|k| self.at(i + skip + k).is_some_and(|c| c.is_ascii_digit()));
        (digits_ok && !self.at(i + skip + 3).is_some_and(|c| c.is_ascii_digit())).then_some(skip)
    }

    /// Reads the number that starts at `start`. Returns the index after what
    /// was read and the token, or `None` when the digits are not a number the
    /// check counts.
    fn number_at(&self, start: usize) -> (usize, Option<NumberToken>) {
        let mut i = start;
        let mut int_digits = String::new();
        loop {
            match self.at(i) {
                Some(c) if c.is_ascii_digit() => {
                    int_digits.push(c);
                    i += 1;
                }
                _ => match self.group_follows(i, int_digits.len()) {
                    Some(skip) => i += skip,
                    None => break,
                },
            }
        }
        let mut frac_digits = String::new();
        if self.at(i) == Some('.') && self.at(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
            while let Some(c) = self.at(i).filter(char::is_ascii_digit) {
                frac_digits.push(c);
                i += 1;
            }
        }
        // `.5` and `p < .05`: a number written without its leading zero.
        let mut start = start;
        if frac_digits.is_empty() && self.starts_at_point(start) {
            frac_digits = std::mem::take(&mut int_digits);
            int_digits.push('0');
            start -= 1;
        }
        let mut mantissa_text = if frac_digits.is_empty() {
            int_digits.clone()
        } else {
            format!("{int_digits}.{frac_digits}")
        };
        let mut decimals = i32::try_from(frac_digits.len()).unwrap_or(0);
        let mut exponent: Option<i32> = None;
        if matches!(self.at(i), Some('e' | 'E'))
            && let Some((value, after)) = self.signed_integer(i + 1)
        {
            exponent = Some(value);
            i = after;
        } else if let Some((power, after)) = self.times_ten_power(i) {
            exponent = Some(power);
            i = after;
        } else if frac_digits.is_empty()
            && int_digits == "10"
            && self.at(i) == Some('^')
            && let Some((power, after)) = self.braced_signed_integer(i + 1)
        {
            // A bare power of ten: `10^{-11}` is the number 1e-11.
            exponent = Some(power);
            i = after;
            mantissa_text = "1".to_string();
            decimals = 0;
        }

        let prev = self.before(start, 1);
        let negative = self.is_negative(start);
        let (percent, end) = self.percent_sign(i);
        let next = self.at(i);

        if self.skipped(
            start,
            i,
            prev,
            next,
            &int_digits,
            &frac_digits,
            exponent.is_some(),
        ) {
            return (end, None);
        }
        let Ok(mantissa) = mantissa_text.parse::<f64>() else {
            return (end, None);
        };
        let exp = exponent.unwrap_or(0);
        let magnitude = mantissa * 10f64.powi(exp);
        let negative = negative && magnitude != 0.0;
        let mut text = String::new();
        if negative {
            text.push('-');
        }
        text.push_str(&mantissa_text);
        if exponent.is_some() {
            text.push_str(&format!("e{exp}"));
        }
        let first = if negative {
            self.sign_start(start)
        } else {
            start
        };
        let token = NumberToken {
            text,
            value: if negative { -magnitude } else { magnitude },
            half_unit: 0.5 * 10f64.powi(exp - decimals),
            percent,
            start: self.offset(first),
            end: self.offset(end),
        };
        (end, Some(token))
    }

    /// Whether a percent sign follows the number that ends at `i`, and the
    /// index after it.
    fn percent_sign(&self, i: usize) -> (bool, usize) {
        let mut j = i;
        loop {
            match (self.at(j), self.at(j + 1)) {
                (Some(' ' | '~' | '\u{a0}'), _) => j += 1,
                (Some('\\'), Some(',')) => j += 2,
                _ => break,
            }
        }
        match (self.at(j), self.at(j + 1)) {
            (Some('%'), _) => (true, j + 1),
            (Some('\\'), Some('%')) => (true, j + 2),
            _ => (false, i),
        }
    }

    /// Reads `-3`, `+3`, `3` and the minus sign U+2212 at `i`.
    fn signed_integer(&self, mut i: usize) -> Option<(i32, usize)> {
        let negative = matches!(self.at(i), Some('-' | '\u{2212}'));
        if matches!(self.at(i), Some('-' | '+' | '\u{2212}')) {
            i += 1;
        }
        let mut digits = String::new();
        while let Some(c) = self.at(i).filter(char::is_ascii_digit) {
            digits.push(c);
            i += 1;
        }
        let value = digits.parse::<i32>().ok()?;
        Some((if negative { -value } else { value }, i))
    }

    /// Reads `{-11}`, `-11` or `11` after a `^`.
    fn braced_signed_integer(&self, mut i: usize) -> Option<(i32, usize)> {
        while self.at(i).is_some_and(char::is_whitespace) {
            i += 1;
        }
        if self.at(i) == Some('{') {
            let (value, mut j) = self.signed_integer(i + 1)?;
            while self.at(j).is_some_and(char::is_whitespace) {
                j += 1;
            }
            return (self.at(j) == Some('}')).then_some((value, j + 1));
        }
        self.signed_integer(i)
    }

    /// Reads `\times 10^{-3}` (or `\cdot`, or a multiplication sign) after a
    /// mantissa.
    fn times_ten_power(&self, mut i: usize) -> Option<(i32, usize)> {
        while self.at(i).is_some_and(char::is_whitespace) {
            i += 1;
        }
        let rest: String = self.chars[i..self.chars.len().min(i + 6)].iter().collect();
        let skip = ["\\times", "\\cdot"]
            .iter()
            .find(|op| rest.starts_with(**op))
            .map(|op| op.chars().count())
            .or_else(|| matches!(self.at(i), Some('\u{d7}' | '\u{b7}')).then_some(1))?;
        i += skip;
        while self.at(i).is_some_and(char::is_whitespace) {
            i += 1;
        }
        if self.at(i) != Some('1') || self.at(i + 1) != Some('0') || self.at(i + 2) != Some('^') {
            return None;
        }
        self.braced_signed_integer(i + 3)
    }

    /// True when the digits at `start` follow a decimal point that no word,
    /// digit or closing bracket precedes: `.5`, not `v1.5` or `a).5`.
    fn starts_at_point(&self, start: usize) -> bool {
        self.before(start, 1) == Some('.')
            && !self
                .before(start, 2)
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ')' | ']' | '}'))
    }

    /// True when the digits at `start` are written with a minus sign.
    fn is_negative(&self, start: usize) -> bool {
        let before_sign = match self.before(start, 1) {
            Some('-' | '\u{2212}') => self.before(start, 2),
            Some('}')
                if self.before(start, 2) == Some('-') && self.before(start, 3) == Some('{') =>
            {
                self.before(start, 4)
            }
            _ => return false,
        };
        !before_sign.is_some_and(|c| {
            c.is_alphanumeric() || matches!(c, ')' | ']' | '}' | '.' | '-' | '\u{2212}' | '_')
        })
    }

    /// Index of the first character of the sign written before `start`.
    fn sign_start(&self, start: usize) -> usize {
        if self.before(start, 1) == Some('}') {
            start - 3
        } else {
            start - 1
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn skipped(
        &self,
        start: usize,
        end: usize,
        prev: Option<char>,
        next: Option<char>,
        int_digits: &str,
        frac_digits: &str,
        has_exponent: bool,
    ) -> bool {
        let glued_before = prev.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
        let hyphen_compound =
            prev == Some('-') && self.before(start, 2).is_some_and(char::is_alphabetic);
        if glued_before || hyphen_compound || next == Some('_') {
            return true;
        }
        if next.is_some_and(char::is_alphabetic) {
            let letters: String = self.chars[end..]
                .iter()
                .take_while(|c| c.is_alphabetic())
                .collect();
            // Other letters directly after digits (`40s`, `5x`, `10ms`) are a
            // unit after a measurement, and the number counts.
            if SUFFIXES_THAT_HIDE.contains(&letters.as_str()) {
                return true;
            }
        }
        // A base before `^` is a power (`2^k`); a power of ten was read above.
        next == Some('^')
            || self.follows_length_macro(end)
            || self.inside_numeric_bracket(start, end)
            // `(1)`, `(2)`: the marker of a list written in running text.
            || (prev == Some('(')
                && next == Some(')')
                && int_digits.len() == 1
                && frac_digits.is_empty())
            || self.follows_label_word(start)
            || (!has_exponent
                && frac_digits.is_empty()
                && is_year(int_digits)
                && !self.counted_noun_follows(end))
    }

    /// A length macro right after the number: `0.8\textwidth`.
    fn follows_length_macro(&self, end: usize) -> bool {
        if self.at(end) != Some('\\') {
            return false;
        }
        let name: String = self.chars[end + 1..]
            .iter()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        LENGTH_MACROS.contains(&name.as_str())
    }

    /// `[12]`, `[1,2]` and `[3--5]` after a word are numeric citations: the
    /// number sits in a bracket that holds only numbers, commas and dashes,
    /// written without spaces. A bracket after `=`, `\in` or `$`, or with a
    /// space after a comma, is an interval, and its numbers count.
    fn inside_numeric_bracket(&self, start: usize, end: usize) -> bool {
        let list_char = |c: char| c.is_ascii_digit() || matches!(c, ',' | '-' | '\u{2013}');
        let mut back = start;
        while back > 0 && list_char(self.chars[back - 1]) {
            back -= 1;
        }
        let mut forward = end;
        while self.at(forward).is_some_and(list_char) {
            forward += 1;
        }
        if !(back > 0 && self.chars[back - 1] == '[' && self.at(forward) == Some(']')) {
            return false;
        }
        let mut open = back - 1;
        while open > 0 && matches!(self.chars[open - 1], ' ' | '~') {
            open -= 1;
        }
        let Some(&before) = open.checked_sub(1).and_then(|p| self.chars.get(p)) else {
            return false;
        };
        if before.is_alphabetic() {
            // A word, not the end of a command such as `\in`.
            let mut word = open - 1;
            while word > 0 && self.chars[word - 1].is_alphabetic() {
                word -= 1;
            }
            return word == 0 || self.chars[word - 1] != '\\';
        }
        matches!(before, '.' | ',' | ']')
    }

    fn follows_label_word(&self, start: usize) -> bool {
        let mut end = start;
        while end > 0
            && (self.chars[end - 1].is_whitespace()
                || matches!(self.chars[end - 1], '~' | '.' | '('))
        {
            end -= 1;
        }
        let mut begin = end;
        while begin > 0 && self.chars[begin - 1].is_alphabetic() {
            begin -= 1;
        }
        if begin == end {
            return false;
        }
        let word: String = self.chars[begin..end]
            .iter()
            .collect::<String>()
            .to_lowercase();
        LABEL_WORDS.contains(&word.as_str())
    }

    /// A year-like integer is a count when a word follows it: `2000 samples`.
    fn counted_noun_follows(&self, end: usize) -> bool {
        let mut j = end;
        while self.at(j).is_some_and(|c| c == ' ' || c == '~') {
            j += 1;
        }
        j > end && self.at(j).is_some_and(char::is_alphabetic)
    }
}

/// The ASCII digit that a digit of another script stands for: full-width,
/// Arabic-Indic, Devanagari and mathematical digits are read as numbers too.
fn ascii_digit(c: char) -> Option<char> {
    if c.is_ascii() {
        return None;
    }
    let code = u32::from(c);
    let zero = [
        0x0660, 0x06f0, 0x0966, 0xff10, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6,
    ]
    .into_iter()
    .find(|zero| (*zero..*zero + 10).contains(&code))?;
    char::from_digit(code - zero, 10)
}

fn is_year(int_digits: &str) -> bool {
    int_digits.len() == 4
        && int_digits
            .parse::<u32>()
            .is_ok_and(|y| (1900..=2100).contains(&y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Vec<String> {
        extract(text).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn extract_reads_decimals_signs_and_groups() {
        assert_eq!(
            texts("scores 2.6311, delta -0.0025 and 1,000 runs"),
            ["2.6311", "-0.0025", "1000"]
        );
        assert_eq!(texts("$-1.6713$ and 1{,}250"), ["-1.6713", "1250"]);
        assert_eq!(texts("shift \u{2212}0.5 and {-}0.25"), ["-0.5", "-0.25"]);
    }

    #[test]
    fn a_number_without_its_leading_zero_is_a_number() {
        assert_eq!(
            texts("a gain of .5 and p < .05, also -.25"),
            ["0.5", "0.05", "-0.25"]
        );
        let token = &extract("a gain of .5")[0];
        assert!((token.value - 0.5).abs() < 1e-12);
        assert!(token.matches(0.5));
        // A point that ends a word, a version or a bracket is not a decimal point.
        assert_eq!(texts("see v1.5, (a).5 and end.5"), Vec::<String>::new());
        assert_eq!(texts("wait...5 runs"), Vec::<String>::new());
    }

    #[test]
    fn digits_of_other_scripts_are_read_as_numbers() {
        assert_eq!(
            texts("\u{ff12}\u{ff16} circles, \u{663}.\u{665} and \u{1d7d9}\u{1d7da}"),
            ["26", "3.5", "12"]
        );
    }

    #[test]
    fn a_minus_between_numbers_or_after_a_word_is_not_a_sign() {
        assert_eq!(texts("0.0453--0.4221"), ["0.0453", "0.4221"]);
        assert_eq!(texts("from 2-3 runs"), ["2", "3"]);
        assert_eq!(texts("x - 3 and step-3"), ["3"]);
    }

    #[test]
    fn extract_skips_labels_years_and_glued_tokens() {
        assert!(
            texts("Table 2 and Figure~3 in Section 4 of H1 (eval_2), ICLR 2026, 3D, [12], [1,3--5], (2)")
                .is_empty()
        );
        assert_eq!(texts("2000 evaluations"), ["2000"]);
    }

    #[test]
    fn brackets_and_parentheses_hide_only_markers_not_counts_or_intervals() {
        assert_eq!(
            texts("the packing has 78 contacts (57) and [1, 57]"),
            ["78", "57", "1", "57"]
        );
        assert_eq!(
            texts("as shown in prior work [12] and [3,4], Eq.~(12)"),
            Vec::<String>::new()
        );
        assert_eq!(texts("three steps: (1) set, (2) run"), Vec::<String>::new());
        assert_eq!(texts("$x \\in [1, 57]$"), ["1", "57"]);
    }

    #[test]
    fn layout_arguments_and_lengths_are_not_numbers() {
        let tex = "\\includegraphics[width=0.8\\textwidth]{a.pdf}\\hspace{14pt}\\vspace{2mm}\\begin{tabular}{rr@{\\hspace{14pt}}rr}\\multicolumn{2}{c}{12} \\cmidrule(lr){2-4} 0.5\\textwidth 3cm \\\\[2pt] \\textcolor{red}{7}";
        assert_eq!(texts(tex), ["12", "7"]);
    }

    #[test]
    fn extract_reads_percent_and_exponent() {
        let tokens = extract("about 97\\% and 1.5e-3 and 2 \\times 10^{-4}");
        assert_eq!(tokens.len(), 3);
        assert!(tokens[0].percent);
        assert!((tokens[1].value - 0.0015).abs() < 1e-12);
        assert!((tokens[2].value - 0.0002).abs() < 1e-12);
    }

    #[test]
    fn a_power_of_ten_is_one_number_and_other_powers_are_not() {
        let tokens = extract("shrink $10^{-11}$ and $r^2$ and $x_{12}$ and $2^k$");
        assert_eq!(tokens.len(), 1, "{tokens:?}");
        assert!((tokens[0].value - 1e-11).abs() < 1e-20);
        assert_eq!(tokens[0].text, "1e-11");
    }

    #[test]
    fn units_after_numbers_do_not_hide_them() {
        assert_eq!(texts("a 40\\,s budget and 5x and 10ms"), ["40", "5", "10"]);
    }

    #[test]
    fn matching_uses_the_printed_digits_and_the_sign() {
        let token = &extract("0.9598")[0];
        assert!(token.matches(0.959_764));
        assert!(!token.matches(-0.959_764));
        assert!(!token.matches(0.9589));
        let negative = &extract("-0.0025")[0];
        assert!(negative.matches(-0.0025));
        assert!(!negative.matches(0.0025));
        let percent = &extract("97%")[0];
        assert!(percent.matches(0.9702));
        assert!(!percent.matches(0.9802));
    }

    #[test]
    fn offsets_point_at_the_printed_text() {
        let text = "gap \u{2212}0.5 then 12%.";
        let tokens = extract(text);
        assert_eq!(&text[tokens[0].start..tokens[0].end], "\u{2212}0.5");
        assert_eq!(&text[tokens[1].start..tokens[1].end], "12%");
    }
}
