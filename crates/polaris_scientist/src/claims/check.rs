//! The number check: every number the manuscript prints must be backed.
//!
//! A printed number is **backed** when a claim covers it and a recorded value
//! gives it:
//!
//! 1. *The claim states it.* A number in prose, a caption, the title or a macro
//!    must be written, with the same sign and digits, in the text of a claim.
//!    A number in a table cell needs no such statement: a table is a listing of
//!    recorded values, so a claim that is about the table and cites the source
//!    that holds the values covers every cell.
//! 2. *The claim is about the sentence or cell.* The claim and the sentence (or
//!    the cell with its column header, row label and table caption) must share
//!    content words. A claim cannot cover a number printed in unrelated text.
//! 3. *A recorded value gives it.* The number must equal, after rounding to the
//!    printed digits and with the same sign, a value of a source the claim
//!    cites, or the result of a derivation the claim records. A number followed
//!    by `%` may also be the recorded value times one hundred.
//!
//! Only the sources a claim cites count. Values of neighbouring sources and
//! constants of the task are not used unless the claim cites them as sources.
//!
//! A *derivation* records how a number was computed: an arithmetic
//! `expression` over named `inputs`, each input a recorded source, optionally
//! with a `#path` into the values of that source. The check evaluates the
//! expression itself.
//!
//! ```json
//! {"claim": "The best score is 2.74 times the baseline.",
//!  "source_ids": ["result.best_correct_score", "baseline.combined_score"],
//!  "derivations": [{"expression": "a / b",
//!                   "inputs": {"a": "result.best_correct_score",
//!                              "b": "baseline.combined_score"}}]}
//! ```
//!
//! A source holds a number in its `value`, and may list more in `values`: an
//! object from a path to a number, or a list of numbers.
//!
//! The check is mechanical. It shows that each printed number is a recorded
//! value or the result of recorded arithmetic, and where it was printed. It
//! does not show that the sentence says something true about that value; the
//! evidence audit judges meaning.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::derive;
use super::manuscript::{Kind, PrintedNumber, content_words};
use super::numbers::{self, NumberToken};

/// One claim, reduced to what the number check reads.
#[derive(Clone, Debug)]
pub struct ClaimInput {
    /// The claim's identifier: its `id` when it has one, otherwise `claims[<index>]`.
    pub id: String,
    /// The claim sentence.
    pub text: String,
    /// The source ids the claim names, in the order written.
    pub source_ids: Vec<String>,
    /// Recorded derivations of numbers the claim states.
    pub derivations: Vec<Derivation>,
    /// Problems found while reading the claim's derivations.
    pub problems: Vec<String>,
}

/// How a number was computed from recorded values.
#[derive(Clone, Debug, PartialEq)]
pub struct Derivation {
    /// Arithmetic over the names in `inputs`; see [`derive`].
    pub expression: String,
    /// Each name of the expression with the recorded source it stands for:
    /// `source.id` or `source.id#path`.
    pub inputs: BTreeMap<String, String>,
}

/// A value a source holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Recorded {
    /// The source holding it.
    pub source_id: String,
    /// Where in the source: empty for its `value`, else the path in `values`.
    pub path: String,
    /// The number.
    pub value: f64,
}

/// The numbers of every source, by source id.
#[derive(Clone, Debug, Default)]
pub struct SourceValues {
    by_source: BTreeMap<String, Vec<Recorded>>,
}

impl SourceValues {
    /// Reads the `grounding_sources` object of `paper_data.json`.
    #[must_use]
    pub fn from_sources(sources: &Map<String, Value>) -> Self {
        let mut by_source = BTreeMap::new();
        for (id, record) in sources {
            let mut values = Vec::new();
            if let Some(value) = record.get("value").and_then(Value::as_f64) {
                values.push(Recorded {
                    source_id: id.clone(),
                    path: String::new(),
                    value,
                });
            }
            match record.get("values") {
                Some(Value::Object(map)) => {
                    for (path, value) in map {
                        if let Some(value) = value.as_f64() {
                            values.push(Recorded {
                                source_id: id.clone(),
                                path: path.clone(),
                                value,
                            });
                        }
                    }
                }
                Some(Value::Array(list)) => {
                    for (index, value) in list.iter().enumerate() {
                        if let Some(value) = value.as_f64() {
                            values.push(Recorded {
                                source_id: id.clone(),
                                path: index.to_string(),
                                value,
                            });
                        }
                    }
                }
                _ => {}
            }
            by_source.insert(id.clone(), values);
        }
        Self { by_source }
    }

    /// The values of source `id`, empty when it holds none.
    #[must_use]
    pub fn of(&self, id: &str) -> &[Recorded] {
        self.by_source.get(id).map_or(&[], Vec::as_slice)
    }

    /// True when `id` is a recorded source.
    #[must_use]
    pub fn knows(&self, id: &str) -> bool {
        self.by_source.contains_key(id)
    }

    /// The value an input reference `source.id` or `source.id#path` names.
    fn input(&self, reference: &str) -> Result<f64, String> {
        let (id, path) = reference.split_once('#').unwrap_or((reference, ""));
        let values = self
            .by_source
            .get(id)
            .ok_or_else(|| format!("input `{reference}` is not a recorded source"))?;
        values
            .iter()
            .find(|v| v.path == path)
            .map(|v| v.value)
            .ok_or_else(|| format!("input `{reference}` names no recorded number"))
    }
}

/// Why a printed number is not backed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// No claim states this number.
    NotStated,
    /// Claims state this number, but none shares words with its sentence.
    ClaimIsAboutOtherText,
    /// A claim covers the sentence, but none of its sources or derivations
    /// gives the number.
    NoRecordedValue,
    /// No claim is about this table.
    TableNotCovered,
}

/// How a backed number is backed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Backing {
    /// The claim that covers the number.
    pub claim_id: String,
    /// The recorded source that gives the number, when it is not derived.
    pub source_id: Option<String>,
    /// Where in the source: empty for its `value`.
    pub path: String,
    /// The recorded value, or the result of the derivation.
    pub recorded_value: f64,
    /// The derivation's expression, when the number is derived.
    pub derivation: Option<String>,
}

/// The nearest recorded value to an unbacked number.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Nearest {
    /// The source.
    pub source_id: String,
    /// The value.
    pub value: f64,
}

/// One printed number and what the check found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NumberFinding {
    /// The number as printed, with its sign.
    pub printed: String,
    /// The file it is printed in.
    pub file: String,
    /// The line of that file.
    pub line: usize,
    /// Where it is printed.
    pub kind: Kind,
    /// The sentence or table cell, shortened.
    pub excerpt: String,
    /// How it is backed; `None` when it is not.
    pub backed_by: Option<Backing>,
    /// Why it is not backed; `None` when it is.
    pub reason: Option<Reason>,
    /// The claims that state this number, or cover this table.
    pub claims_considered: Vec<String>,
    /// The nearest value among the sources of those claims.
    pub nearest: Option<Nearest>,
}

impl NumberFinding {
    /// True when a recorded value or derivation gives the number.
    #[must_use]
    pub fn is_backed(&self) -> bool {
        self.backed_by.is_some()
    }
}

/// The result of the number check.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NumberReport {
    /// Every number the manuscript prints, in order.
    pub findings: Vec<NumberFinding>,
    /// Problems with the claims' derivations.
    pub problems: Vec<String>,
}

impl NumberReport {
    /// Numbers printed.
    #[must_use]
    pub fn printed(&self) -> usize {
        self.findings.len()
    }

    /// Numbers backed by a recorded value or derivation.
    #[must_use]
    pub fn backed(&self) -> usize {
        self.findings.iter().filter(|f| f.is_backed()).count()
    }

    /// The numbers that are not backed.
    pub fn unbacked(&self) -> impl Iterator<Item = &NumberFinding> {
        self.findings.iter().filter(|f| !f.is_backed())
    }

    /// True when every printed number is backed and the derivations are sound.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty() && self.unbacked().next().is_none()
    }
}

/// Words a claim must share with a sentence to be about it.
fn shared_words_needed(claim_words: usize, unit_words: usize) -> usize {
    if claim_words >= 6 && unit_words >= 6 {
        2
    } else {
        1
    }
}

/// A claim prepared for matching.
struct Prepared<'a> {
    claim: &'a ClaimInput,
    tokens: Vec<NumberToken>,
    words: std::collections::BTreeSet<String>,
}

/// Checks every printed number against the claims and the recorded sources.
///
/// `printed` is the list from [`super::manuscript::Manuscript::printed_numbers`].
#[must_use]
pub fn check(
    printed: &[PrintedNumber],
    claims: &[ClaimInput],
    sources: &SourceValues,
) -> NumberReport {
    let prepared: Vec<Prepared<'_>> = claims
        .iter()
        .map(|claim| Prepared {
            claim,
            tokens: numbers::extract(&claim.text),
            words: content_words(&claim.text),
        })
        .collect();
    let mut report = NumberReport::default();
    for claim in claims {
        report.problems.extend(claim.problems.iter().cloned());
        for derivation in &claim.derivations {
            if let Err(problem) = derived_value(derivation, sources) {
                report.problems.push(format!(
                    "`{}` records a derivation that cannot be computed: {problem}",
                    claim.id
                ));
            }
        }
    }
    for number in printed {
        report
            .findings
            .push(check_number(number, &prepared, sources));
    }
    report
}

fn check_number(
    number: &PrintedNumber,
    claims: &[Prepared<'_>],
    sources: &SourceValues,
) -> NumberFinding {
    let mut finding = NumberFinding {
        printed: number.token.text.clone(),
        file: number.file.clone(),
        line: number.line,
        kind: number.kind,
        excerpt: number.excerpt.clone(),
        backed_by: None,
        reason: None,
        claims_considered: Vec::new(),
        nearest: None,
    };
    let is_cell = number.kind == Kind::Cell;
    // The claims that may cover the number: those that state it, or for a
    // table cell those that are about the table.
    let about = |prepared: &Prepared<'_>| {
        let shared = prepared.words.intersection(&number.words).count();
        shared >= shared_words_needed(prepared.words.len(), number.words.len())
    };
    let states = |prepared: &Prepared<'_>| prepared.tokens.iter().any(|t| t.same_as(&number.token));
    let stating: Vec<&Prepared<'_>> = claims.iter().filter(|p| states(p)).collect();
    let candidates: Vec<&Prepared<'_>> = if is_cell {
        claims.iter().filter(|p| about(p)).collect()
    } else {
        stating.iter().copied().filter(|p| about(p)).collect()
    };
    finding.claims_considered = if candidates.is_empty() {
        stating.iter().map(|p| p.claim.id.clone()).collect()
    } else {
        candidates.iter().map(|p| p.claim.id.clone()).collect()
    };
    if candidates.is_empty() {
        finding.reason = Some(if is_cell {
            Reason::TableNotCovered
        } else if stating.is_empty() {
            Reason::NotStated
        } else {
            Reason::ClaimIsAboutOtherText
        });
        return finding;
    }
    for prepared in &candidates {
        if let Some(backing) = backing(&number.token, prepared.claim, sources) {
            finding.backed_by = Some(backing);
            finding.claims_considered = vec![prepared.claim.id.clone()];
            return finding;
        }
    }
    finding.reason = Some(Reason::NoRecordedValue);
    finding.nearest = candidates
        .iter()
        .flat_map(|p| p.claim.source_ids.iter())
        .flat_map(|id| sources.of(id))
        .min_by(|a, b| {
            number
                .token
                .distance(a.value)
                .total_cmp(&number.token.distance(b.value))
        })
        .map(|r| Nearest {
            source_id: r.source_id.clone(),
            value: r.value,
        });
    finding
}

/// The recorded value or derivation of `claim` that gives `token`.
fn backing(token: &NumberToken, claim: &ClaimInput, sources: &SourceValues) -> Option<Backing> {
    for id in &claim.source_ids {
        if let Some(found) = sources.of(id).iter().find(|r| token.matches(r.value)) {
            return Some(Backing {
                claim_id: claim.id.clone(),
                source_id: Some(found.source_id.clone()),
                path: found.path.clone(),
                recorded_value: found.value,
                derivation: None,
            });
        }
    }
    for derivation in &claim.derivations {
        if let Ok(value) = derived_value(derivation, sources)
            && token.matches(value)
        {
            return Some(Backing {
                claim_id: claim.id.clone(),
                source_id: None,
                path: String::new(),
                recorded_value: value,
                derivation: Some(derivation.expression.clone()),
            });
        }
    }
    None
}

/// Recomputes a derivation from its recorded inputs.
///
/// The result must depend on the inputs: an expression that gives the same
/// value when every input changes (a bare number, or `a * 0 + 999`) records no
/// arithmetic on recorded values and is refused.
fn derived_value(derivation: &Derivation, sources: &SourceValues) -> Result<f64, String> {
    let mut inputs = BTreeMap::new();
    for (name, reference) in &derivation.inputs {
        inputs.insert(name.clone(), sources.input(reference)?);
    }
    let value = derive::evaluate(&derivation.expression, &inputs).map_err(|e| e.to_string())?;
    let changed: BTreeMap<String, f64> = inputs
        .iter()
        .enumerate()
        .map(|(index, (name, v))| (name.clone(), v * 1.37 + 0.61 + index as f64))
        .collect();
    let moved = derive::evaluate(&derivation.expression, &changed).map_err(|e| e.to_string())?;
    if (moved - value).abs() <= 1e-9 * value.abs().max(1.0) {
        return Err("its result does not depend on its inputs".to_string());
    }
    Ok(value)
}

/// Reads the `derivations` of a claim record. A derivation that is not an
/// object with an `expression` and `inputs` is reported as a problem.
pub(crate) fn read_derivations(claim: &Value, id: &str) -> (Vec<Derivation>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let Some(list) = claim.get("derivations") else {
        return (out, problems);
    };
    let Some(list) = list.as_array() else {
        problems.push(format!("`{id}`: `derivations` must be a list"));
        return (out, problems);
    };
    for (index, item) in list.iter().enumerate() {
        let expression = item.get("expression").and_then(Value::as_str);
        let inputs = item.get("inputs").and_then(Value::as_object);
        match (expression, inputs) {
            (Some(expression), Some(inputs)) => {
                let mut map = BTreeMap::new();
                for (name, reference) in inputs {
                    match reference.as_str() {
                        Some(reference) => {
                            map.insert(name.clone(), reference.to_string());
                        }
                        None => problems.push(format!(
                            "`{id}`: derivations[{index}].inputs.{name} must name a source"
                        )),
                    }
                }
                out.push(Derivation {
                    expression: expression.to_string(),
                    inputs: map,
                });
            }
            _ => problems.push(format!(
                "`{id}`: derivations[{index}] needs an `expression` and an `inputs` object"
            )),
        }
    }
    (out, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::manuscript::Manuscript;
    use serde_json::json;

    fn sources() -> SourceValues {
        let map = json!({
            "result.best": {"value": 2.6298},
            "baseline.score": {"value": 0.9598},
            "ablation.delta": {"value": -0.1991},
            "task.constants.num_circles": {"value": 26},
            "posthoc.x.data.contact": {"values": {"per.1e-07.pairs": 57, "per.1e-07.walls": 21}}
        });
        SourceValues::from_sources(map.as_object().expect("object"))
    }

    fn claim(id: &str, text: &str, ids: &[&str]) -> ClaimInput {
        ClaimInput {
            id: id.into(),
            text: text.into(),
            source_ids: ids.iter().map(|s| (*s).to_string()).collect(),
            derivations: Vec::new(),
            problems: Vec::new(),
        }
    }

    fn run(tex: &str, claims: &[ClaimInput]) -> NumberReport {
        let dir = tempfile::tempdir().expect("dir");
        let body = format!("\\begin{{document}}\n{tex}\n\\end{{document}}\n");
        let printed = Manuscript::from_main(dir.path(), &body).printed_numbers();
        check(&printed, claims, &sources())
    }

    #[test]
    fn a_stated_number_that_a_cited_source_holds_is_backed() {
        let claims = [claim(
            "c1",
            "The pipeline reaches a sum of radii of 2.6298.",
            &["result.best"],
        )];
        let report = run(
            "The pipeline reaches a sum of radii of 2.6298 here.",
            &claims,
        );
        assert_eq!((report.printed(), report.backed()), (1, 1));
        let backing = report.findings[0].backed_by.as_ref().expect("backed");
        assert_eq!(backing.claim_id, "c1");
        assert_eq!(backing.source_id.as_deref(), Some("result.best"));
        assert!(report.is_clean());
    }

    #[test]
    fn a_number_no_claim_states_is_not_backed() {
        let claims = [claim(
            "c1",
            "The pipeline reaches a sum of radii of 2.6298.",
            &["result.best"],
        )];
        let report = run("The pipeline reaches a sum of radii of 999 here.", &claims);
        assert_eq!(report.findings[0].reason, Some(Reason::NotStated));
        assert!(!report.is_clean());
    }

    #[test]
    fn a_claim_about_other_text_does_not_cover_the_sentence() {
        let claims = [claim(
            "c1",
            "The control scores 2.6298 on the benchmark.",
            &["result.best"],
        )];
        let report = run("Wall clock time was 2.6298 seconds overall.", &claims);
        assert_eq!(
            report.findings[0].reason,
            Some(Reason::ClaimIsAboutOtherText)
        );
        assert_eq!(report.findings[0].claims_considered, ["c1"]);
    }

    #[test]
    fn a_stated_number_with_no_recorded_value_names_the_nearest_one() {
        let claims = [claim(
            "c1",
            "The pipeline reaches a sum of radii of 2.7001.",
            &["result.best"],
        )];
        let report = run(
            "The pipeline reaches a sum of radii of 2.7001 here.",
            &claims,
        );
        let finding = &report.findings[0];
        assert_eq!(finding.reason, Some(Reason::NoRecordedValue));
        assert_eq!(finding.nearest.as_ref().map(|n| n.value), Some(2.6298));
    }

    #[test]
    fn signs_count() {
        let claims = [claim(
            "c1",
            "The ablation score change is 0.1991 below the control.",
            &["ablation.delta"],
        )];
        let report = run(
            "The ablation score change is 0.1991 below the control.",
            &claims,
        );
        assert_eq!(report.findings[0].reason, Some(Reason::NoRecordedValue));
        let claims = [claim(
            "c1",
            "The ablation score change is -0.1991 against the control.",
            &["ablation.delta"],
        )];
        let report = run(
            "The ablation score change is $-0.1991$ against the control.",
            &claims,
        );
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn a_derivation_must_recompute_to_the_printed_number() {
        let mut c = claim(
            "c1",
            "The best score is 2.74 times the baseline score.",
            &["result.best", "baseline.score"],
        );
        c.derivations.push(Derivation {
            expression: "a / b".into(),
            inputs: BTreeMap::from([
                ("a".into(), "result.best".into()),
                ("b".into(), "baseline.score".into()),
            ]),
        });
        let ok = run(
            "The best score is 2.74 times the baseline score.",
            std::slice::from_ref(&c),
        );
        assert!(ok.is_clean(), "{ok:?}");
        assert_eq!(
            ok.findings[0]
                .backed_by
                .as_ref()
                .and_then(|b| b.derivation.as_deref()),
            Some("a / b")
        );
        let mut wrong = c.clone();
        wrong.text = "The best score is 2.80 times the baseline score.".into();
        let bad = run("The best score is 2.80 times the baseline score.", &[wrong]);
        assert_eq!(bad.findings[0].reason, Some(Reason::NoRecordedValue));
    }

    #[test]
    fn a_derivation_that_ignores_its_inputs_backs_nothing() {
        for expression in ["999", "a * 0 + 999", "max(a, 999)"] {
            let mut c = claim("c1", "The pipeline reaches 999 circles.", &["result.best"]);
            c.derivations.push(Derivation {
                expression: expression.into(),
                inputs: BTreeMap::from([("a".into(), "result.best".into())]),
            });
            let report = run("The pipeline reaches 999 circles.", &[c]);
            assert_eq!(
                report.findings[0].reason,
                Some(Reason::NoRecordedValue),
                "{expression}"
            );
            assert!(
                report
                    .problems
                    .iter()
                    .any(|p| p.contains("does not depend")),
                "{expression}: {:?}",
                report.problems
            );
        }
    }

    #[test]
    fn a_derivation_with_an_unknown_input_is_a_problem() {
        let mut c = claim("c1", "The ratio is 2.74.", &["result.best"]);
        c.derivations.push(Derivation {
            expression: "a / b".into(),
            inputs: BTreeMap::from([
                ("a".into(), "result.best".into()),
                ("b".into(), "nowhere".into()),
            ]),
        });
        let report = run("The ratio is 2.74.", &[c]);
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
        assert!(!report.is_clean());
    }

    #[test]
    fn values_of_neighbouring_sources_and_task_constants_are_not_used() {
        let claims = [claim(
            "c1",
            "The baseline score is 2.6298 here.",
            &["baseline.score"],
        )];
        let report = run("The baseline score is 2.6298 here.", &claims);
        assert_eq!(report.findings[0].reason, Some(Reason::NoRecordedValue));
        let claims = [claim(
            "c1",
            "The task packs 26 circles and reaches 2.6298.",
            &["result.best"],
        )];
        let report = run("The task packs 26 circles and reaches 2.6298.", &claims);
        assert_eq!(report.unbacked().count(), 1, "26 needs the constant cited");
        let claims = [claim(
            "c1",
            "The task packs 26 circles and reaches 2.6298.",
            &["result.best", "task.constants.num_circles"],
        )];
        assert!(run("The task packs 26 circles and reaches 2.6298.", &claims).is_clean());
    }

    #[test]
    fn a_table_cell_is_covered_by_a_claim_about_the_table_that_cites_its_values() {
        let tex = "\\begin{table}\n\\caption{Contact counts of the final packing.}\n\\begin{tabular}{lrr}\nTolerance & Circle & Wall \\\\\n\\midrule\nloose & 57 & 21 \\\\\nzero & 58 & 21 \\\\\n\\end{tabular}\n\\end{table}";
        let claims = [claim(
            "c1",
            "Table lists the circle and wall contact counts of the final packing.",
            &["posthoc.x.data.contact"],
        )];
        let report = run(tex, &claims);
        let verdicts: Vec<(&str, bool)> = report
            .findings
            .iter()
            .map(|f| (f.printed.as_str(), f.is_backed()))
            .collect();
        assert_eq!(
            verdicts,
            [("57", true), ("21", true), ("58", false), ("21", true)]
        );
        assert_eq!(
            report.findings[0]
                .backed_by
                .as_ref()
                .map(|b| b.path.as_str()),
            Some("per.1e-07.pairs")
        );
        let none = run(
            tex,
            &[claim(
                "c9",
                "Unrelated remark about timing.",
                &["result.best"],
            )],
        );
        assert!(
            none.findings
                .iter()
                .all(|f| f.reason == Some(Reason::TableNotCovered))
        );
    }
}
