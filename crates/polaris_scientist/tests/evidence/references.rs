//! Reference retrieval against a local stand-in for the two services.

use std::path::Path;
use std::time::Duration;

use polaris_scientist::phases::paper::refs::{
    self, BIB_FILE, Backoff, Endpoints, PROVENANCE_FILE, Settings, retrieve, store,
};
use serde_json::{Value, json};

use crate::stand_in::{Reply, Request, StandIn};

/// Semantic Scholar answers for the papers `s2-1` … `s2-4`; the citation
/// counts are 10, 40, 40 and 5, so the ranking is `s2-2`, `s2-3`, `s2-1`, `s2-4`.
fn semantic_scholar_answers(request: &Request) -> Reply {
    if request.path == "/graph/v1/paper/search" {
        return Reply::json(json!({"data": [
            {"paperId": "s2-1", "citationCount": 10},
            {"paperId": "s2-2", "citationCount": 40},
            {"paperId": "s2-3", "citationCount": 40},
            {"paperId": "s2-4", "citationCount": 5},
            {"title": "no id"}
        ]}));
    }
    let id = request.path.trim_start_matches("/graph/v1/paper/");
    let n: i64 = id.trim_start_matches("s2-").parse().unwrap_or(0);
    Reply::json(json!({
        "paperId": id, "title": format!("Paper {n}"), "year": 2000 + n,
        "venue": "Venue", "citationCount": n * 3,
        "citationStyles": {"bibtex": format!("@Article{{Key{n},\n title={{Paper {n}}},\n}}")}
    }))
}

fn openalex_answers(request: &Request) -> Reply {
    let work = |n: u32, title: &str| {
        json!({
            "id": format!("https://openalex.org/W{n}"),
            "display_name": title,
            "publication_year": 2010 + n,
            "cited_by_count": n,
            "doi": format!("https://doi.org/10.1/{n}"),
            "authorships": [{"author": {"display_name": format!("Ann Author{n}")}}],
            "primary_location": {"source": {"display_name": "Journal"}}
        })
    };
    let query = request.param("search").unwrap_or_default().to_string();
    Reply::json(json!({"results": [
        work(1, &format!("Alpha method for {query}")),
        work(2, "Beta results"),
        work(3, "Gamma bounds"),
        {"id": "https://openalex.org/W9", "display_name": "No authors", "publication_year": 2000}
    ]}))
}

/// One server for both services: Semantic Scholar under `/graph/v1`, OpenAlex
/// under `/works`.
fn services(
    semantic_scholar: fn(&Request) -> Reply,
    openalex: fn(&Request) -> Reply,
) -> (StandIn, Settings) {
    let server = StandIn::start(move |request| {
        if request.path.starts_with("/graph/v1") {
            semantic_scholar(request)
        } else {
            openalex(request)
        }
    });
    let mut settings = Settings::new(3, 5);
    settings.endpoints = Endpoints {
        semantic_scholar: format!("{}/graph/v1", server.url),
        openalex: format!("{}/works", server.url),
    };
    settings.backoff = Backoff {
        unit: Duration::from_millis(1),
        least: Duration::ZERO,
        most: Duration::from_millis(5),
    };
    settings.timeout = Duration::from_secs(10);
    (server, settings)
}

fn queries() -> Vec<String> {
    vec![
        "circle packing".into(),
        "packing  heuristics".into(),
        "bin packing".into(),
    ]
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("file")
}

fn provenance(dir: &Path) -> Value {
    serde_json::from_str(&read(dir, PROVENANCE_FILE)).expect("provenance")
}

fn not_found(_: &Request) -> Reply {
    Reply::status(404)
}

fn failing(_: &Request) -> Reply {
    Reply::status(500)
}

#[tokio::test]
async fn the_first_service_answers_and_the_second_is_not_asked() {
    let (server, settings) = services(semantic_scholar_answers, failing);
    let result = retrieve(&settings, &queries(), "plan").await;

    let keys: Vec<_> = result
        .entries
        .iter()
        .map(|e| e.reference.citation_key.as_str())
        .collect();
    assert_eq!(
        keys,
        ["Key2", "Key3", "Key1"],
        "ranked by citations, then id"
    );
    assert_eq!(result.source, "semantic_scholar");
    assert_eq!(result.attempts.len(), 1);
    assert_eq!(result.attempts[0].outcome, "complete");
    assert_eq!(result.attempts[0].error_count, 0);
    assert!(
        server.requests().iter().all(|r| r.starts_with("/graph/v1")),
        "{:?}",
        server.requests()
    );
    let first = &result.entries[0].reference;
    assert_eq!(first.id, "ref.s2.s2-2");
    assert_eq!(first.query, "circle packing");
    assert_eq!(first.year, Some(2002));
    assert_eq!(first.citation_count, Some(6));
    assert!(first.source_url.ends_with("/graph/v1/paper/s2-2"));
}

#[tokio::test]
async fn the_second_service_is_used_when_the_first_fails() {
    let (server, settings) = services(failing, openalex_answers);
    let result = retrieve(&settings, &queries(), "plan").await;

    assert_eq!(result.source, "openalex");
    assert_eq!(result.attempts.len(), 2);
    let first = &result.attempts[0];
    assert_eq!(
        (first.service.as_str(), first.outcome.as_str()),
        ("semantic_scholar", "failed")
    );
    assert!(first.errors[0].contains("HTTP 500"), "{:?}", first.errors);
    assert!(
        first
            .errors
            .last()
            .unwrap()
            .contains("3 failed requests in a row"),
        "a service that keeps failing is given up: {:?}",
        first.errors
    );
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.starts_with("/graph/v1"))
            .count(),
        3
    );
    assert_eq!(result.attempts[1].outcome, "complete");
    let ids: Vec<_> = result
        .entries
        .iter()
        .map(|e| e.reference.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["ref.openalex.W1", "ref.openalex.W2", "ref.openalex.W3"],
        "works without authors are left out and repeats across queries are not added again"
    );
    let entry = &result.entries[0];
    assert_eq!(entry.reference.citation_key, "Author12011Alpha");
    assert!(entry.bibtex.contains("author={Ann Author1}"));
    assert_eq!(entry.reference.source_url, "https://doi.org/10.1/1");
}

#[tokio::test]
async fn both_services_failing_is_recorded_and_the_counts_stay_short() {
    let (_server, settings) = services(failing, not_found);
    let dir = tempfile::tempdir().unwrap();
    let result = retrieve(&settings, &queries(), "plan").await;
    store(dir.path(), &result, &json!({"status": "ready"})).unwrap();

    assert!(result.entries.is_empty());
    assert_eq!(result.source, "none");
    let record = provenance(dir.path());
    assert_eq!(record["count"], 0);
    assert_eq!(record["references"], json!([]));
    assert_eq!(record["source"], "none");
    let attempts = record["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    for attempt in attempts {
        assert_eq!(attempt["outcome"], "failed");
        assert_eq!(attempt["references"], 0);
        assert!(attempt["error_count"].as_u64().unwrap() >= 1);
        assert_eq!(attempt["stage"], "plan");
    }
    assert_eq!(
        attempts[1]["errors"][0],
        "search for \"circle packing\": HTTP 404"
    );
    assert_eq!(read(dir.path(), BIB_FILE), "");
}

#[tokio::test]
async fn a_short_answer_from_the_first_service_is_kept_when_the_second_has_less() {
    fn few(request: &Request) -> Reply {
        if request.path == "/graph/v1/paper/search" {
            return Reply::json(json!({"data": [{"paperId": "s2-1", "citationCount": 1}]}));
        }
        semantic_scholar_answers(request)
    }
    fn none(_: &Request) -> Reply {
        Reply::json(json!({"results": []}))
    }
    let (_server, settings) = services(few, none);
    let result = retrieve(&settings, &queries(), "plan").await;

    assert_eq!(result.source, "semantic_scholar");
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.attempts[0].outcome, "short");
    assert_eq!(result.attempts[1].outcome, "failed");
    assert!(!result.is_complete(3));
}

#[tokio::test]
async fn the_second_service_wins_when_it_supplies_more_than_a_short_first() {
    fn few(request: &Request) -> Reply {
        if request.path == "/graph/v1/paper/search" {
            return Reply::json(json!({"data": [{"paperId": "s2-1", "citationCount": 1}]}));
        }
        semantic_scholar_answers(request)
    }
    let (_server, settings) = services(few, openalex_answers);
    let result = retrieve(&settings, &queries(), "plan").await;

    assert_eq!(result.source, "openalex");
    assert_eq!(result.entries.len(), 3);
    assert_eq!(result.attempts[0].outcome, "short");
    assert_eq!(result.attempts[1].outcome, "complete");
}

#[tokio::test]
async fn a_rate_limit_is_retried_after_the_named_wait() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEARCHES: AtomicUsize = AtomicUsize::new(0);
    fn limited(request: &Request) -> Reply {
        if request.path == "/graph/v1/paper/search" && SEARCHES.fetch_add(1, Ordering::SeqCst) == 0
        {
            return Reply::status(429).with_header("Retry-After", "0");
        }
        semantic_scholar_answers(request)
    }
    let (_server, settings) = services(limited, failing);
    let result = retrieve(&settings, &queries(), "plan").await;

    assert_eq!(result.attempts[0].outcome, "complete");
    assert_eq!(
        result.attempts[0].error_count, 0,
        "the retry hid the refusal"
    );
    assert_eq!(result.entries.len(), 3);
}

#[tokio::test]
async fn an_answer_that_is_not_json_is_a_named_failure() {
    fn page(_: &Request) -> Reply {
        Reply::text("<html>maintenance</html>")
    }
    let (_server, settings) = services(page, failing);
    let result = retrieve(&settings, &queries(), "plan").await;

    assert!(
        result.attempts[0].errors[0].contains("not valid JSON"),
        "{:?}",
        result.attempts[0]
    );
    assert!(result.entries.is_empty());
}

#[tokio::test]
async fn an_unreachable_service_is_a_failure_not_a_crash() {
    let mut settings = Settings::new(3, 5);
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap();
    drop(free);
    settings.endpoints = Endpoints {
        semantic_scholar: format!("http://{address}/graph/v1"),
        openalex: format!("http://{address}/works"),
    };
    settings.timeout = Duration::from_secs(2);
    let result = retrieve(&settings, &queries(), "plan").await;

    assert!(result.entries.is_empty());
    assert_eq!(
        result.attempts[0].errors[0],
        "search for \"circle packing\": connection failed"
    );
}

#[tokio::test]
async fn an_offline_run_contacts_nobody_and_says_so() {
    let (server, mut settings) = services(semantic_scholar_answers, openalex_answers);
    settings.offline = true;
    let result = retrieve(&settings, &queries(), "plan").await;

    assert!(server.requests().is_empty());
    assert!(result.entries.is_empty());
    assert!(result.attempts.iter().all(|a| a.outcome == "skipped"));
    assert!(result.attempts[0].errors[0].contains("offline"));
}

#[tokio::test]
async fn the_same_answers_give_the_same_files() {
    let (_server, settings) = services(semantic_scholar_answers, failing);
    let plan = json!({"status": "ready", "queries": ["circle packing"]});
    let mut written = Vec::new();
    for _ in 0..2 {
        let dir = tempfile::tempdir().unwrap();
        let result = retrieve(&settings, &queries(), "plan").await;
        store(dir.path(), &result, &refs::plan_metadata(&plan)).unwrap();
        written.push((
            read(dir.path(), BIB_FILE),
            read(dir.path(), PROVENANCE_FILE),
        ));
    }
    assert_eq!(written[0], written[1]);
    assert!(written[0].0.starts_with("@Article{Key2,"));
    assert!(!written[0].1.contains("retrieved_at"));
    assert!(!written[0].1.contains("generated_at"));
}

#[tokio::test]
async fn a_second_round_adds_only_new_keys_and_logs_both_rounds() {
    let (_server, settings) = services(semantic_scholar_answers, failing);
    let dir = tempfile::tempdir().unwrap();
    let first = retrieve(&settings, &["circle packing".to_string()], "plan").await;
    store(dir.path(), &first, &json!({"status": "ready"})).unwrap();

    let (_other, other_settings) = services(failing, openalex_answers);
    let second = retrieve(&other_settings, &["Alpha".to_string()], "postdraft").await;
    let merged = refs::store_merged(dir.path(), &second, &json!({"status": "ready"})).unwrap();

    let keys: Vec<_> = merged
        .references
        .iter()
        .map(|r| r.citation_key.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "Key2",
            "Key3",
            "Key1",
            "Author12011Alpha",
            "Author22012Beta",
            "Author32013Gamma"
        ]
    );
    assert_eq!(merged.count, 6);
    let stages: Vec<_> = merged.attempts.iter().map(|a| a.stage.as_str()).collect();
    assert_eq!(stages, ["plan", "postdraft", "postdraft"]);
    let on_disk = provenance(dir.path());
    assert_eq!(on_disk["count"], 6);
    assert_eq!(on_disk["queries"], json!(["circle packing", "Alpha"]));
    assert_eq!(read(dir.path(), BIB_FILE).matches("\n@").count(), 5);
}
