//! Checks of the planning files the paper roles write: the reference query
//! plan, the draft's reference gaps and the paper strategy.
//!
//! Each check reads a JSON file and its memo from the paper folder and
//! returns the first problem as one sentence. The sentence goes back to the
//! role as feedback, so it names the file and the field.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use super::checks::read_regular;
use super::figures::{FIGURE_TYPES, strategy_type};

/// Query categories a plan may use.
pub const QUERY_CATEGORIES: [&str; 5] = [
    "core_method",
    "baseline_comparator",
    "benchmark_context",
    "architectural_shift",
    "result_driven_theme",
];

/// Most queries a plan may hold unless the caller sets another limit.
pub const MAX_QUERIES: usize = 10;

/// Longest query text.
const MAX_QUERY_CHARS: usize = 200;

/// Gap priorities.
const PRIORITIES: [&str; 3] = ["high", "medium", "low"];

/// What a citation is for.
const INTENTS: [&str; 4] = [
    "background",
    "positioning",
    "comparator",
    "benchmark_context",
];

/// Tools a figure spec may prefer.
const GENERATORS: [&str; 4] = ["matplotlib", "tikz", "native_latex", "manual"];

/// The only prompt family this build writes with.
pub const PROMPT_FAMILY: &str = "paper_standalone";

/// File names of one planning record: the JSON and its readable memo.
#[derive(Clone, Copy, Debug)]
pub struct PlanFiles {
    /// The JSON record.
    pub json: &'static str,
    /// The memo that explains it.
    pub memo: &'static str,
}

/// The plan written before the draft.
pub const REFERENCE_PLAN: PlanFiles = PlanFiles {
    json: "reference_query_plan.json",
    memo: "reference_query_plan.md",
};

/// The plan written after the draft.
pub const POSTDRAFT_PLAN: PlanFiles = PlanFiles {
    json: "postdraft_reference_query_plan.json",
    memo: "postdraft_reference_query_plan.md",
};

/// The draft's reference gaps.
pub const GAPS: PlanFiles = PlanFiles {
    json: "draft_reference_gaps.json",
    memo: "draft_reference_gaps.md",
};

/// The paper strategy.
pub const STRATEGY: PlanFiles = PlanFiles {
    json: "paper_strategy.json",
    memo: "paper_strategy.md",
};

/// A checked reference query plan.
#[derive(Clone, Debug, PartialEq)]
pub struct ReferencePlan {
    /// The whole record, for the provenance file.
    pub record: Value,
    /// The query texts in order.
    pub queries: Vec<String>,
}

/// A checked gap analysis.
#[derive(Clone, Debug, PartialEq)]
pub struct Gaps {
    /// True when the analyst found nothing to add.
    pub none: bool,
    /// The search topics of every rewrite target, in order, without repeats.
    pub topics: Vec<String>,
}

/// Collapses runs of white space.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn text_field(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .map(squash)
        .unwrap_or_default()
}

/// Reads the memo and the JSON object of `files`.
fn read_pair(dir: &Path, files: PlanFiles) -> Result<Value, String> {
    let memo = read_regular(&dir.join(files.memo))
        .map_err(|e| format!("required memo {} is not usable: {e}", files.memo))?;
    if memo.trim().is_empty() {
        return Err(format!("the memo {} is empty", files.memo));
    }
    let body = read_regular(&dir.join(files.json))
        .map_err(|e| format!("required record {} is not usable: {e}", files.json))?;
    let value: Value = serde_json::from_str(&body)
        .map_err(|e| format!("{} is not valid JSON: {e}", files.json))?;
    if !value.is_object() {
        return Err(format!("{} must hold one JSON object", files.json));
    }
    Ok(value)
}

/// Reads one JSON object that has no memo.
fn read_object(dir: &Path, name: &str) -> Result<Value, String> {
    let body = read_regular(&dir.join(name))
        .map_err(|e| format!("required record {name} is not usable: {e}"))?;
    let value: Value =
        serde_json::from_str(&body).map_err(|e| format!("{name} is not valid JSON: {e}"))?;
    if !value.is_object() {
        return Err(format!("{name} must hold one JSON object"));
    }
    Ok(value)
}

fn status_of(value: &Value) -> String {
    text_field(value, "status")
}

fn failure_reason(value: &Value) -> String {
    text_field(value, "failure_reason")
}

/// Checks the reference query plan in `dir`.
///
/// # Errors
///
/// Returns the first problem: a missing or empty file, a bad `status`, a plan
/// with `status` `fail`, no topic, no queries, too many queries, a query that
/// is empty, too long or repeated, a missing rationale or an unknown category.
pub fn check_reference_plan(
    dir: &Path,
    files: PlanFiles,
    max_queries: usize,
) -> Result<ReferencePlan, String> {
    let record = read_pair(dir, files)?;
    let name = files.json;
    match status_of(&record).as_str() {
        "ready" => {}
        "fail" => {
            let reason = failure_reason(&record);
            if reason.is_empty() {
                return Err(format!(
                    "{name} status=fail needs a non-empty failure_reason"
                ));
            }
            return Err(format!("reference query planning failed: {reason}"));
        }
        _ => return Err(format!("{name} must set status to \"ready\" or \"fail\"")),
    }
    if text_field(&record, "central_topic").is_empty() {
        return Err(format!("{name} needs a non-empty central_topic"));
    }
    let rows = record
        .get("queries")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| format!("{name} needs a non-empty queries list"))?;
    if rows.len() > max_queries {
        return Err(format!(
            "{name} holds {} queries; the limit is {max_queries}",
            rows.len()
        ));
    }
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if !row.is_object() {
            return Err(format!("{name} queries[{index}] must be an object"));
        }
        let query = text_field(row, "query");
        if query.is_empty() {
            return Err(format!("{name} queries[{index}] needs a non-empty query"));
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(format!(
                "{name} queries[{index}].query is longer than {MAX_QUERY_CHARS} characters"
            ));
        }
        if text_field(row, "rationale").is_empty() {
            return Err(format!(
                "{name} queries[{index}] needs a non-empty rationale"
            ));
        }
        if !QUERY_CATEGORIES.contains(&text_field(row, "category").as_str()) {
            return Err(format!(
                "{name} queries[{index}].category must be one of: {}",
                QUERY_CATEGORIES.join(", ")
            ));
        }
        if !seen.insert(query.to_lowercase()) {
            return Err(format!(
                "{name} queries[{index}] repeats the query: {query}"
            ));
        }
        queries.push(query);
    }
    for field in [
        "core_methods",
        "comparators",
        "benchmarks",
        "architectural_shifts",
        "result_driven_themes",
        "forbidden_query_patterns",
    ] {
        if record.get(field).is_some_and(|v| !v.is_array()) {
            return Err(format!("{name} field {field} must be a list"));
        }
    }
    Ok(ReferencePlan { record, queries })
}

/// Checks the draft's reference gaps in `dir`.
///
/// # Errors
///
/// Returns the first problem, as for [`check_reference_plan`].
pub fn check_gaps(dir: &Path) -> Result<Gaps, String> {
    let record = read_pair(dir, GAPS)?;
    let name = GAPS.json;
    let status = status_of(&record);
    if !["ready", "no_gaps", "fail"].contains(&status.as_str()) {
        return Err(format!(
            "{name} must set status to \"ready\", \"no_gaps\" or \"fail\""
        ));
    }
    if text_field(&record, "summary").is_empty() {
        return Err(format!("{name} needs a non-empty summary"));
    }
    for field in ["rewrite_targets", "missing_literatures"] {
        if record.get(field).is_some_and(|v| !v.is_array()) {
            return Err(format!("{name} field {field} must be a list"));
        }
    }
    let targets = record
        .get("rewrite_targets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match status.as_str() {
        "fail" => {
            let reason = failure_reason(&record);
            if reason.is_empty() {
                return Err(format!(
                    "{name} status=fail needs a non-empty failure_reason"
                ));
            }
            return Err(format!("draft reference gap analysis failed: {reason}"));
        }
        "no_gaps" => {
            if !targets.is_empty() {
                return Err(format!(
                    "{name} status=no_gaps needs an empty rewrite_targets"
                ));
            }
            return Ok(Gaps {
                none: true,
                topics: Vec::new(),
            });
        }
        _ => {}
    }
    if targets.is_empty() {
        return Err(format!(
            "{name} status=ready needs a non-empty rewrite_targets"
        ));
    }
    let mut ids = BTreeSet::new();
    let mut topics = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, row) in targets.iter().enumerate() {
        if !row.is_object() {
            return Err(format!("{name} rewrite_targets[{index}] must be an object"));
        }
        let id = text_field(row, "target_id");
        if id.is_empty() {
            return Err(format!(
                "{name} rewrite_targets[{index}] needs a non-empty target_id"
            ));
        }
        if !ids.insert(id.clone()) {
            return Err(format!(
                "{name} rewrite_targets[{index}] repeats target_id {id}"
            ));
        }
        for key in ["section", "reason", "anchor_text"] {
            if text_field(row, key).is_empty() {
                return Err(format!(
                    "{name} rewrite_targets[{index}] needs a non-empty {key}"
                ));
            }
        }
        if !PRIORITIES.contains(&text_field(row, "priority").as_str()) {
            return Err(format!(
                "{name} rewrite_targets[{index}].priority must be one of: {}",
                PRIORITIES.join(", ")
            ));
        }
        if !INTENTS.contains(&text_field(row, "citation_intent").as_str()) {
            return Err(format!(
                "{name} rewrite_targets[{index}].citation_intent must be one of: {}",
                INTENTS.join(", ")
            ));
        }
        let list = row
            .get("search_topics")
            .and_then(Value::as_array)
            .filter(|l| !l.is_empty())
            .ok_or_else(|| {
                format!("{name} rewrite_targets[{index}] needs a non-empty search_topics list")
            })?;
        for topic in list {
            let topic = topic.as_str().map(squash).unwrap_or_default();
            if topic.is_empty() {
                return Err(format!(
                    "{name} rewrite_targets[{index}] holds an empty search topic"
                ));
            }
            if seen.insert(topic.to_lowercase()) {
                topics.push(topic);
            }
        }
    }
    Ok(Gaps {
        none: false,
        topics,
    })
}

fn require_text(value: &Value, name: &str, key: &str) -> Result<(), String> {
    if text_field(value, key).is_empty() {
        return Err(format!("{name} needs a non-empty field: {key}"));
    }
    Ok(())
}

/// Checks the paper strategy in `dir` against the source ids of
/// `paper_data.json`.
///
/// # Errors
///
/// Returns the first problem: a missing field, an unknown source id, a figure
/// spec that is not well formed or a prompt family other than the one this
/// build writes with.
pub fn check_strategy(dir: &Path, source_ids: &BTreeSet<String>) -> Result<Value, String> {
    let record = read_pair(dir, STRATEGY)?;
    let name = STRATEGY.json;
    let status = status_of(&record);
    if !["ready", "fail"].contains(&status.as_str()) {
        return Err(format!("{name} must set status to \"ready\" or \"fail\""));
    }
    let family = text_field(&record, "selected_prompt_family");
    if family.is_empty() {
        return Err(format!("{name} needs selected_prompt_family"));
    }
    if status == "fail" {
        let reason = failure_reason(&record);
        if reason.is_empty() {
            return Err(format!(
                "{name} status=fail needs a non-empty failure_reason"
            ));
        }
        return Err(format!("paper strategy failed: {reason}"));
    }
    if family != PROMPT_FAMILY {
        return Err(format!(
            "{name} selected_prompt_family must be \"{PROMPT_FAMILY}\"; it is \"{family}\""
        ));
    }
    for key in [
        "recommended_title",
        "central_question",
        "central_thesis",
        "narrative_rationale",
    ] {
        require_text(&record, name, key)?;
    }
    check_supplement(&record, source_ids)?;
    let groups = record
        .get("method_groups")
        .and_then(Value::as_array)
        .filter(|g| !g.is_empty())
        .ok_or_else(|| format!("{name} needs a non-empty method_groups list"))?;
    for (index, row) in groups.iter().enumerate() {
        for key in ["name", "role", "include_reason"] {
            if text_field(row, key).is_empty() {
                return Err(format!(
                    "{name} method_groups[{index}] needs a non-empty field: {key}"
                ));
            }
        }
    }
    for field in [
        "results_focus",
        "figure_plan",
        "figure_specs",
        "forbidden_headings",
        "forbidden_phrases",
        "notes_for_writer",
    ] {
        if !record.get(field).is_some_and(Value::is_array) {
            return Err(format!("{name} field {field} must be a list"));
        }
    }
    check_figure_specs(&record)?;
    Ok(record)
}

fn check_supplement(record: &Value, source_ids: &BTreeSet<String>) -> Result<(), String> {
    let name = STRATEGY.json;
    let plan = record
        .get("supplementary_plan")
        .filter(|p| p.is_object())
        .ok_or_else(|| format!("{name} needs a supplementary_plan object"))?;
    if text_field(plan, "purpose").is_empty() {
        return Err(format!(
            "{name} supplementary_plan.purpose must be non-empty"
        ));
    }
    let sections = plan
        .get("planned_subsections")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!("{name} supplementary_plan.planned_subsections must be a non-empty list")
        })?;
    for (index, row) in sections.iter().enumerate() {
        let at = format!("{name} supplementary_plan.planned_subsections[{index}]");
        for key in ["heading", "grounded_content"] {
            if text_field(row, key).is_empty() {
                return Err(format!("{at}.{key} must be non-empty"));
            }
        }
        let ids = row
            .get("source_ids")
            .and_then(Value::as_array)
            .filter(|l| !l.is_empty())
            .ok_or_else(|| format!("{at}.source_ids must be a non-empty list"))?;
        let mut unknown = Vec::new();
        for id in ids {
            let Some(id) = id.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                return Err(format!("{at}.source_ids entries must be non-empty strings"));
            };
            if !source_ids.contains(id) {
                unknown.push(id.to_string());
            }
        }
        if !unknown.is_empty() {
            unknown.sort();
            return Err(format!(
                "{at}.source_ids names unknown grounding source ids: {}",
                unknown.join(", ")
            ));
        }
    }
    Ok(())
}

fn check_figure_specs(record: &Value) -> Result<(), String> {
    let name = STRATEGY.json;
    let specs = record
        .get("figure_specs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (index, row) in specs.iter().enumerate() {
        if !row.is_object() {
            return Err(format!("{name} figure_specs[{index}] must be an object"));
        }
        for key in ["figure_id", "figure_type"] {
            if text_field(row, key).is_empty() {
                return Err(format!(
                    "{name} figure_specs[{index}] needs a non-empty field: {key}"
                ));
            }
        }
        // The manifest must name a supported type and the same type as the strategy, so a
        // strategy type that no manifest can carry would make every manifest fail.
        let figure_type = text_field(row, "figure_type");
        if !FIGURE_TYPES.contains(&strategy_type(&figure_type)) {
            return Err(format!(
                "{name} figure_specs[{index}].figure_type {figure_type:?} is not a supported \
                 figure type; use one of: {} (flow_diagram means diagram, horizontal_bar means bar)",
                FIGURE_TYPES.join(", ")
            ));
        }
        let generator = text_field(row, "preferred_generator");
        if !generator.is_empty() && !GENERATORS.contains(&generator.as_str()) {
            return Err(format!(
                "{name} figure_specs[{index}].preferred_generator must be one of: {}",
                GENERATORS.join(", ")
            ));
        }
        for key in [
            "max_panels",
            "max_categories_per_panel",
            "max_series_per_panel",
        ] {
            if !row.get(key).and_then(Value::as_u64).is_some_and(|n| n > 0) {
                return Err(format!(
                    "{name} figure_specs[{index}].{key} must be a positive integer"
                ));
            }
        }
        if !row.get("allow_progression").is_some_and(Value::is_boolean) {
            return Err(format!(
                "{name} figure_specs[{index}].allow_progression must be true or false"
            ));
        }
        if row.get("required").is_some_and(|v| !v.is_boolean()) {
            return Err(format!(
                "{name} figure_specs[{index}].required must be true or false"
            ));
        }
    }
    Ok(())
}

/// File name of the ranking of the post-hoc analyses.
pub const RANKING_FILE: &str = "posthoc_ranking.json";

/// How much an analysis matters for the thesis.
const RELEVANCE: [&str; 3] = ["high", "medium", "low"];

/// Where an analysis belongs in the paper.
const USAGE: [&str; 4] = ["main_results", "supporting", "appendix", "skip"];

/// Checks `posthoc_ranking.json` in `dir`: every analysis in `ids` is ranked
/// exactly once, with ranks `1..=n`, a relevance, a usage and the two texts.
///
/// Returns the entries by analysis id in rank order.
///
/// # Errors
///
/// Returns one sentence naming the field that breaks the contract.
pub fn check_ranking(dir: &Path, ids: &[String]) -> Result<Vec<(String, Value)>, String> {
    let record = read_object(dir, RANKING_FILE)?;
    let rationale = text_field(&record, "ranking_rationale");
    if rationale.is_empty() {
        return Err(format!(
            "{RANKING_FILE} ranking_rationale must be non-empty"
        ));
    }
    let entries = record
        .get("ranked_analyses")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{RANKING_FILE} ranked_analyses must be a list"))?;
    let mut ranked: Vec<(String, u64, Value)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let at = format!("{RANKING_FILE} ranked_analyses[{index}]");
        if !entry.is_object() {
            return Err(format!("{at} must be an object"));
        }
        let id = text_field(entry, "analysis_id");
        if id.is_empty() {
            return Err(format!("{at}.analysis_id must be non-empty"));
        }
        let rank = entry
            .get("rank")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{at}.rank must be an integer"))?;
        if !RELEVANCE.contains(&text_field(entry, "relevance_to_thesis").as_str()) {
            return Err(format!(
                "{at}.relevance_to_thesis must be one of: {}",
                RELEVANCE.join(", ")
            ));
        }
        if !USAGE.contains(&text_field(entry, "recommended_usage").as_str()) {
            return Err(format!(
                "{at}.recommended_usage must be one of: {}",
                USAGE.join(", ")
            ));
        }
        for key in ["relevance_justification", "key_finding_for_paper"] {
            if text_field(entry, key).is_empty() {
                return Err(format!("{at}.{key} must be non-empty"));
            }
        }
        ranked.push((id, rank, entry.clone()));
    }
    let mut seen = BTreeSet::new();
    for (id, _, _) in &ranked {
        if !seen.insert(id.as_str()) {
            return Err(format!("{RANKING_FILE} ranks analysis {id} more than once"));
        }
    }
    let expected: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let unknown: Vec<&str> = seen.difference(&expected).copied().collect();
    if !unknown.is_empty() {
        return Err(format!(
            "{RANKING_FILE} ranks unknown analyses: {}",
            unknown.join(", ")
        ));
    }
    let missing: Vec<&str> = expected.difference(&seen).copied().collect();
    if !missing.is_empty() {
        return Err(format!(
            "{RANKING_FILE} leaves analyses unranked: {}",
            missing.join(", ")
        ));
    }
    let mut ranks: Vec<u64> = ranked.iter().map(|(_, rank, _)| *rank).collect();
    ranks.sort_unstable();
    let wanted: Vec<u64> = (1..=ids.len() as u64).collect();
    if ranks != wanted {
        return Err(format!(
            "{RANKING_FILE} ranks must be the integers 1 to {} used once each",
            ids.len()
        ));
    }
    ranked.sort_by_key(|(_, rank, _)| *rank);
    Ok(ranked.into_iter().map(|(id, _, row)| (id, row)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn put(dir: &Path, files: PlanFiles, record: &Value) {
        std::fs::write(dir.join(files.memo), "memo\n").unwrap();
        std::fs::write(dir.join(files.json), record.to_string()).unwrap();
    }

    fn plan(queries: Value) -> Value {
        json!({"status": "ready", "central_topic": "topic", "queries": queries})
    }

    fn query(text: &str) -> Value {
        json!({"query": text, "category": "core_method", "rationale": "why"})
    }

    #[test]
    fn a_good_plan_lists_its_queries_in_order() {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            REFERENCE_PLAN,
            &plan(json!([query("alpha  beta"), query("gamma")])),
        );
        let checked = check_reference_plan(dir.path(), REFERENCE_PLAN, MAX_QUERIES).unwrap();
        assert_eq!(checked.queries, ["alpha beta", "gamma"]);
    }

    #[test]
    fn a_plan_is_refused_for_each_contract_break() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (json!({"status": "maybe"}), "status"),
            (
                json!({"status": "fail", "failure_reason": "no data"}),
                "planning failed: no data",
            ),
            (json!({"status": "fail"}), "failure_reason"),
            (json!({"status": "ready", "queries": []}), "central_topic"),
            (plan(json!([])), "queries list"),
            (plan(json!([query("a"), query("A")])), "repeats the query"),
            (
                plan(json!([{"query": "a", "category": "other", "rationale": "r"}])),
                "category",
            ),
            (
                plan(json!([{"query": "a", "category": "core_method"}])),
                "rationale",
            ),
            (plan(json!([query(&"x".repeat(201))])), "longer than 200"),
        ];
        for (record, needle) in cases {
            put(dir.path(), REFERENCE_PLAN, &record);
            let error =
                check_reference_plan(dir.path(), REFERENCE_PLAN, MAX_QUERIES).expect_err(needle);
            assert!(error.contains(needle), "{error} lacks {needle}");
        }
    }

    #[test]
    fn a_plan_with_too_many_queries_names_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            REFERENCE_PLAN,
            &plan(json!([query("a"), query("b"), query("c")])),
        );
        let error = check_reference_plan(dir.path(), REFERENCE_PLAN, 2).unwrap_err();
        assert!(error.contains("limit is 2"), "{error}");
    }

    #[test]
    fn a_missing_or_empty_memo_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let error = check_reference_plan(dir.path(), REFERENCE_PLAN, MAX_QUERIES).unwrap_err();
        assert!(error.contains("reference_query_plan.md"), "{error}");
        put(dir.path(), REFERENCE_PLAN, &plan(json!([query("a")])));
        std::fs::write(dir.path().join(REFERENCE_PLAN.memo), "  \n").unwrap();
        let error = check_reference_plan(dir.path(), REFERENCE_PLAN, MAX_QUERIES).unwrap_err();
        assert!(error.contains("empty"), "{error}");
    }

    fn target(id: &str, topics: Value) -> Value {
        json!({"target_id": id, "section": "Related Work", "priority": "high",
            "citation_intent": "positioning", "reason": "r", "anchor_text": "a",
            "search_topics": topics})
    }

    #[test]
    fn gaps_collect_topics_without_repeats() {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            GAPS,
            &json!({"status": "ready", "summary": "s", "rewrite_targets": [
                target("t1", json!(["one", "two"])), target("t2", json!(["Two", "three"]))]}),
        );
        let gaps = check_gaps(dir.path()).unwrap();
        assert!(!gaps.none);
        assert_eq!(gaps.topics, ["one", "two", "three"]);
    }

    #[test]
    fn gaps_say_when_there_is_nothing_to_add_and_refuse_a_contradiction() {
        let dir = tempfile::tempdir().unwrap();
        put(
            dir.path(),
            GAPS,
            &json!({"status": "no_gaps", "summary": "s", "rewrite_targets": []}),
        );
        assert!(check_gaps(dir.path()).unwrap().none);
        put(
            dir.path(),
            GAPS,
            &json!({"status": "no_gaps", "summary": "s",
                "rewrite_targets": [target("t1", json!(["x"]))]}),
        );
        assert!(check_gaps(dir.path()).unwrap_err().contains("no_gaps"));
        put(
            dir.path(),
            GAPS,
            &json!({"status": "ready", "summary": "s",
                "rewrite_targets": [target("t1", json!([]))]}),
        );
        assert!(
            check_gaps(dir.path())
                .unwrap_err()
                .contains("search_topics")
        );
    }

    fn strategy() -> Value {
        json!({
            "status": "ready", "selected_prompt_family": "paper_standalone",
            "recommended_title": "t", "central_question": "q", "central_thesis": "th",
            "narrative_rationale": "n",
            "supplementary_plan": {"purpose": "p", "planned_subsections": [
                {"heading": "h", "grounded_content": "g", "source_ids": ["a.b"]}]},
            "method_groups": [{"name": "n", "role": "baseline", "include_reason": "i"}],
            "results_focus": [], "figure_plan": [], "forbidden_headings": [],
            "forbidden_phrases": [], "notes_for_writer": [],
            "figure_specs": [{"figure_id": "f1", "figure_type": "bar",
                "max_panels": 1, "max_categories_per_panel": 6,
                "max_series_per_panel": 2, "allow_progression": false}]
        })
    }

    fn ids() -> BTreeSet<String> {
        ["a.b".to_string()].into()
    }

    fn ranking(entries: &[(&str, u64)]) -> Value {
        json!({
            "ranking_rationale": "why",
            "ranked_analyses": entries.iter().map(|(id, rank)| json!({
                "analysis_id": id, "rank": rank, "relevance_to_thesis": "high",
                "relevance_justification": "j", "recommended_usage": "supporting",
                "key_finding_for_paper": "k"
            })).collect::<Vec<_>>()
        })
    }

    fn ranking_ids() -> Vec<String> {
        vec!["a".to_string(), "b".to_string()]
    }

    #[test]
    fn a_ranking_lists_every_analysis_once_in_rank_order() {
        let dir = tempfile::tempdir().unwrap();
        let write = |value: &Value| {
            std::fs::write(dir.path().join(RANKING_FILE), value.to_string()).unwrap();
        };
        write(&ranking(&[("a", 2), ("b", 1)]));
        let ranked = check_ranking(dir.path(), &ranking_ids()).unwrap();
        assert_eq!(ranked[0].0, "b");
        assert_eq!(ranked[1].0, "a");
        for (value, needle) in [
            (ranking(&[("a", 1)]), "unranked: b"),
            (
                ranking(&[("a", 1), ("b", 2), ("c", 3)]),
                "unknown analyses: c",
            ),
            (ranking(&[("a", 1), ("a", 2)]), "more than once"),
            (ranking(&[("a", 1), ("b", 3)]), "integers 1 to 2"),
        ] {
            write(&value);
            let error = check_ranking(dir.path(), &ranking_ids()).unwrap_err();
            assert!(error.contains(needle), "{error} lacks {needle}");
        }
        let mut bad = ranking(&[("a", 1), ("b", 2)]);
        bad["ranked_analyses"][0]["recommended_usage"] = json!("main");
        write(&bad);
        assert!(
            check_ranking(dir.path(), &ranking_ids())
                .unwrap_err()
                .contains("recommended_usage")
        );
        std::fs::remove_file(dir.path().join(RANKING_FILE)).unwrap();
        assert!(
            check_ranking(dir.path(), &ranking_ids())
                .unwrap_err()
                .contains("posthoc_ranking.json")
        );
    }

    #[test]
    fn a_good_strategy_passes_and_each_break_is_named() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), STRATEGY, &strategy());
        check_strategy(dir.path(), &ids()).unwrap();
        type Break = (&'static str, fn(&mut Value));
        let breaks: [Break; 5] = [
            ("unknown grounding source ids: zz", |v| {
                v["supplementary_plan"]["planned_subsections"][0]["source_ids"] = json!(["zz"]);
            }),
            ("selected_prompt_family must be", |v| {
                v["selected_prompt_family"] = json!("paper");
            }),
            ("method_groups", |v| {
                v["method_groups"] = json!([]);
            }),
            ("max_panels", |v| {
                v["figure_specs"][0]["max_panels"] = json!(0);
            }),
            ("preferred_generator", |v| {
                v["figure_specs"][0]["preferred_generator"] = json!("generate-concept-figure");
            }),
        ];
        for (needle, change) in breaks {
            let mut record = strategy();
            change(&mut record);
            put(dir.path(), STRATEGY, &record);
            let error = check_strategy(dir.path(), &ids()).expect_err(needle);
            assert!(error.contains(needle), "{error} lacks {needle}");
        }
    }

    #[test]
    fn a_figure_type_the_manifest_cannot_name_is_rejected_with_the_allowed_types() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = strategy();
        record["figure_specs"][0]["figure_type"] =
            json!("composite (geometry drawing + line plot)");
        put(dir.path(), STRATEGY, &record);
        let error = check_strategy(dir.path(), &ids()).unwrap_err();
        assert!(
            error.contains("figure_specs[0].figure_type")
                && error.contains("composite (geometry drawing + line plot)"),
            "{error}"
        );
        for allowed in FIGURE_TYPES {
            assert!(error.contains(allowed), "{error} lacks {allowed}");
        }
    }

    #[test]
    fn a_strategy_name_that_means_a_supported_type_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        for kind in ["flow_diagram", "horizontal_bar", "composite", " diagram "] {
            let mut record = strategy();
            record["figure_specs"][0]["figure_type"] = json!(kind);
            put(dir.path(), STRATEGY, &record);
            check_strategy(dir.path(), &ids()).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        }
    }
}
