//! Reference retrieval from open scholarly services.
//!
//! Semantic Scholar is asked first. When it cannot supply the wanted number of
//! references, OpenAlex is asked next. The result is `references.bib` plus
//! `references_provenance.json`.
//!
//! Retrieval never fails the phase. A service that cannot be reached, answers
//! with an error, or returns too little is recorded under `attempts` in the
//! provenance file, and the files hold whatever was found. Output is
//! deterministic: no timestamps, stable ordering, stable citation keys.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Bibliography file name inside the paper folder.
pub const BIB_FILE: &str = "references.bib";
/// Provenance file name inside the paper folder.
pub const PROVENANCE_FILE: &str = "references_provenance.json";

/// Public address of the Semantic Scholar graph API.
pub const SEMANTIC_SCHOLAR_URL: &str = "https://api.semanticscholar.org/graph/v1";
/// Public address of the OpenAlex works search.
pub const OPENALEX_URL: &str = "https://api.openalex.org/works";

/// Environment variable that replaces the Semantic Scholar address.
pub const SEMANTIC_SCHOLAR_URL_VAR: &str = "POLARIS_SCIENTIST_S2_URL";
/// Environment variable that replaces the OpenAlex address.
pub const OPENALEX_URL_VAR: &str = "POLARIS_SCIENTIST_OPENALEX_URL";
/// Environment variable that marks a run as offline.
pub const OFFLINE_VAR: &str = "POLARIS_SCIENTIST_OFFLINE";

const SEMANTIC_SCHOLAR: &str = "semantic_scholar";
const OPENALEX: &str = "openalex";
/// Longest query text sent to a service.
const QUERY_CHARS: usize = 200;
/// Requests in a row that may fail before a service is given up.
const FAILURES_IN_A_ROW: usize = 3;
/// Failures kept in the provenance file for one service.
const ERRORS_KEPT: usize = 10;
/// Attempts for one request that is answered with 403 or 429.
const REQUEST_ATTEMPTS: u32 = 5;

/// Where the services are reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// Base address of the Semantic Scholar graph API.
    pub semantic_scholar: String,
    /// Address of the OpenAlex works search.
    pub openalex: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            semantic_scholar: SEMANTIC_SCHOLAR_URL.to_string(),
            openalex: OPENALEX_URL.to_string(),
        }
    }
}

/// How long to wait before asking again after a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// Wait per attempt when the service names no wait of its own.
    pub unit: Duration,
    /// Shortest wait.
    pub least: Duration,
    /// Longest wait.
    pub most: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            unit: Duration::from_millis(1500),
            least: Duration::from_millis(500),
            most: Duration::from_secs(20),
        }
    }
}

impl Backoff {
    fn wait(&self, attempt: u32, named: Option<f64>) -> Duration {
        let wanted = named
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map_or_else(|| self.unit * attempt, Duration::from_secs_f64);
        wanted.clamp(self.least, self.most.max(self.least))
    }
}

/// Everything retrieval needs to know.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Where the services are reached.
    pub endpoints: Endpoints,
    /// References wanted; a service with fewer is called short.
    pub min_refs: usize,
    /// Most references kept from one OpenAlex search.
    pub max_refs: usize,
    /// Hits asked for per Semantic Scholar query.
    pub semantic_scholar_hits: usize,
    /// Hits asked for per OpenAlex query.
    pub openalex_hits: usize,
    /// Time allowed for one request.
    pub timeout: Duration,
    /// Wait between repeated requests.
    pub backoff: Backoff,
    /// Semantic Scholar key, sent when present.
    pub semantic_scholar_key: Option<String>,
    /// Contact address sent to OpenAlex when present.
    pub mailto: Option<String>,
    /// When set, no service is contacted.
    pub offline: bool,
}

impl Settings {
    /// Settings for the public services with the given reference counts.
    pub fn new(min_refs: usize, max_refs: usize) -> Self {
        Self {
            endpoints: Endpoints::default(),
            min_refs: min_refs.max(1),
            max_refs: max_refs.max(min_refs).max(1),
            semantic_scholar_hits: 25,
            openalex_hits: 10,
            timeout: Duration::from_secs(20),
            backoff: Backoff::default(),
            semantic_scholar_key: None,
            mailto: None,
            offline: false,
        }
    }

    /// Settings that read the process environment: the optional service keys,
    /// replacement addresses for the services, and the offline marker. An
    /// offline run contacts no public service; an address that was replaced is
    /// still used, which lets an offline run point at a local stand-in.
    pub fn from_environment(min_refs: usize, max_refs: usize) -> Self {
        let mut settings = Self::new(min_refs, max_refs);
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        settings.semantic_scholar_key = var("S2_API_KEY");
        settings.mailto = var("OPENALEX_EMAIL");
        let s2 = var(SEMANTIC_SCHOLAR_URL_VAR);
        let openalex = var(OPENALEX_URL_VAR);
        settings.offline =
            var(OFFLINE_VAR).as_deref() == Some("1") && s2.is_none() && openalex.is_none();
        if let Some(url) = s2 {
            settings.endpoints.semantic_scholar = url;
        }
        if let Some(url) = openalex {
            settings.endpoints.openalex = url;
        }
        settings
    }
}

/// One retrieved reference as listed in the provenance file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    /// Stable identifier, `ref.s2.<id>` or `ref.openalex.<id>`.
    pub id: String,
    /// Identifier at the service.
    pub paper_id: String,
    /// Key used by `\cite`.
    pub citation_key: String,
    /// Title, when the service gave one.
    pub title: Option<String>,
    /// Publication year, when known.
    pub year: Option<i64>,
    /// Venue name, when known.
    pub venue: Option<String>,
    /// Citation count at the service, when known.
    pub citation_count: Option<i64>,
    /// Search query that found the reference.
    pub query: String,
    /// Where the record was read.
    pub source_url: String,
}

/// A reference together with its BibTeX entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The provenance row.
    pub reference: Reference,
    /// The BibTeX text, without surrounding blank lines.
    pub bibtex: String,
}

/// What happened when one service was asked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    /// Retrieval round: `plan` for the first, `postdraft` for the second.
    pub stage: String,
    /// `semantic_scholar` or `openalex`.
    pub service: String,
    /// `complete` (enough references), `short`, `failed` (none) or `skipped`.
    pub outcome: String,
    /// References the service supplied.
    pub references: usize,
    /// Failures seen, counting those not listed.
    pub error_count: usize,
    /// The first failures, in the order they happened.
    pub errors: Vec<String>,
}

/// The references one retrieval round found and how the services behaved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Retrieval {
    /// Entries from the service that supplied the most.
    pub entries: Vec<Entry>,
    /// Name of that service, `none` when no service supplied any.
    pub source: String,
    /// Normalised queries that were used.
    pub queries: Vec<String>,
    /// One record per service that was asked.
    pub attempts: Vec<Attempt>,
}

impl Retrieval {
    /// Whether at least `min_refs` references were found.
    pub fn is_complete(&self, min_refs: usize) -> bool {
        self.entries.len() >= min_refs
    }
}

/// The provenance file as it is stored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// Service the references of the latest round came from.
    #[serde(default)]
    pub source: String,
    /// Number of references listed.
    #[serde(default)]
    pub count: usize,
    /// Queries behind the references.
    #[serde(default)]
    pub queries: Vec<String>,
    /// Description of the latest query plan.
    #[serde(default)]
    pub reference_query_plan: Value,
    /// Descriptions of every query plan that contributed.
    #[serde(default)]
    pub reference_query_plans: Vec<Value>,
    /// The references.
    #[serde(default)]
    pub references: Vec<Reference>,
    /// How each service behaved in each round.
    #[serde(default)]
    pub attempts: Vec<Attempt>,
}

/// Collapses blanks, cuts long queries, and drops empty and repeated ones
/// (ignoring case).
pub fn normalize_queries(queries: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for query in queries {
        let mut clean = query.split_whitespace().collect::<Vec<_>>().join(" ");
        if clean.chars().count() > QUERY_CHARS {
            clean = clean
                .chars()
                .take(QUERY_CHARS)
                .collect::<String>()
                .trim_end()
                .to_string();
        }
        if clean.is_empty() || !seen.insert(clean.to_lowercase()) {
            continue;
        }
        out.push(clean);
    }
    out
}

/// Asks the services for references and reports what each one did. `stage`
/// labels the round in the provenance (`plan` or `postdraft`).
pub async fn retrieve(settings: &Settings, queries: &[String], stage: &str) -> Retrieval {
    let queries = normalize_queries(queries);
    let mut attempts = Vec::new();
    if settings.offline || queries.is_empty() {
        let reason = if settings.offline {
            "offline run: no reference service was contacted"
        } else {
            "no queries to search"
        };
        for service in [SEMANTIC_SCHOLAR, OPENALEX] {
            attempts.push(Attempt {
                stage: stage.to_string(),
                service: service.to_string(),
                outcome: "skipped".to_string(),
                references: 0,
                error_count: 1,
                errors: vec![reason.to_string()],
            });
        }
        return Retrieval {
            entries: Vec::new(),
            source: "none".to_string(),
            queries,
            attempts,
        };
    }
    let client = match client_for(settings) {
        Ok(client) => client,
        Err(error) => {
            for service in [SEMANTIC_SCHOLAR, OPENALEX] {
                attempts.push(Attempt {
                    stage: stage.to_string(),
                    service: service.to_string(),
                    outcome: "failed".to_string(),
                    references: 0,
                    error_count: 1,
                    errors: vec![format!("http client could not be built: {error}")],
                });
            }
            return Retrieval {
                entries: Vec::new(),
                source: "none".to_string(),
                queries,
                attempts,
            };
        }
    };

    let first = semantic_scholar(settings, &client, &queries).await;
    attempts.push(first.attempt(stage, settings.min_refs));
    let mut best = ("semantic_scholar", first.entries);
    if best.1.len() < settings.min_refs {
        tracing::info!(
            found = best.1.len(),
            wanted = settings.min_refs,
            "Semantic Scholar was short; asking OpenAlex"
        );
        let second = openalex(settings, &client, &queries).await;
        attempts.push(second.attempt(stage, settings.min_refs));
        if second.entries.len() > best.1.len() {
            best = (OPENALEX, second.entries);
        }
    }
    let source = if best.1.is_empty() { "none" } else { best.0 };
    Retrieval {
        entries: best.1,
        source: source.to_string(),
        queries,
        attempts,
    }
}

/// Writes the files for the first retrieval round, replacing earlier ones.
pub fn store(dir: &Path, retrieval: &Retrieval, plan: &Value) -> std::io::Result<Provenance> {
    let provenance = Provenance {
        source: retrieval.source.clone(),
        count: retrieval.entries.len(),
        queries: retrieval.queries.clone(),
        reference_query_plan: plan.clone(),
        reference_query_plans: vec![plan.clone()],
        references: retrieval
            .entries
            .iter()
            .map(|entry| entry.reference.clone())
            .collect(),
        attempts: retrieval.attempts.clone(),
    };
    write_files(dir, &retrieval.entries, &provenance)?;
    Ok(provenance)
}

/// Adds the references of a later round to the files already in `dir`.
/// Existing references stay first; a new one with a known key is dropped.
pub fn store_merged(
    dir: &Path,
    retrieval: &Retrieval,
    plan: &Value,
) -> std::io::Result<Provenance> {
    let (mut entries, mut provenance) = load(dir);
    let mut keys: HashSet<String> = entries
        .iter()
        .map(|entry| entry.reference.citation_key.clone())
        .collect();
    for entry in &retrieval.entries {
        if keys.insert(entry.reference.citation_key.clone()) {
            entries.push(entry.clone());
        }
    }
    let mut queries = provenance.queries.clone();
    queries.extend(retrieval.queries.iter().cloned());
    provenance.queries = normalize_queries(&queries);
    provenance.source = if retrieval.entries.is_empty() {
        provenance.source
    } else {
        retrieval.source.clone()
    };
    provenance.count = entries.len();
    provenance.reference_query_plan = plan.clone();
    provenance.reference_query_plans.push(plan.clone());
    provenance.references = entries
        .iter()
        .map(|entry| entry.reference.clone())
        .collect();
    provenance
        .attempts
        .extend(retrieval.attempts.iter().cloned());
    write_files(dir, &entries, &provenance)?;
    Ok(provenance)
}

/// Reads the bibliography and provenance from `dir`. A missing or unreadable
/// file counts as empty. Only entries that have a provenance row are kept.
pub fn load(dir: &Path) -> (Vec<Entry>, Provenance) {
    let provenance: Provenance = std::fs::read_to_string(dir.join(PROVENANCE_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let bib = std::fs::read_to_string(dir.join(BIB_FILE)).unwrap_or_default();
    let texts = split_entries(&bib);
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for reference in &provenance.references {
        let Some(bibtex) = texts.iter().find(|(key, _)| *key == reference.citation_key) else {
            continue;
        };
        if seen.insert(reference.citation_key.clone()) {
            entries.push(Entry {
                reference: reference.clone(),
                bibtex: bibtex.1.clone(),
            });
        }
    }
    (entries, provenance)
}

fn write_files(dir: &Path, entries: &[Entry], provenance: &Provenance) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut bib = entries
        .iter()
        .map(|entry| entry.bibtex.trim().to_string())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !bib.is_empty() {
        bib.push('\n');
    }
    crate::durable::atomic_write(&dir.join(BIB_FILE), bib.as_bytes())
        .map_err(std::io::Error::other)?;
    let value = serde_json::to_value(provenance).map_err(std::io::Error::other)?;
    let mut text = serde_json::to_string_pretty(&value).map_err(std::io::Error::other)?;
    text.push('\n');
    crate::durable::atomic_write(&dir.join(PROVENANCE_FILE), text.as_bytes())
        .map_err(std::io::Error::other)
}

/// Splits a bibliography into `(key, text)` pairs, one per entry.
fn split_entries(bib: &str) -> Vec<(String, String)> {
    let mut texts: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in bib.lines() {
        if line.trim_start().starts_with('@') && !current.is_empty() {
            texts.push(current.join("\n").trim().to_string());
            current.clear();
        }
        current.push(line);
    }
    if !current.is_empty() {
        texts.push(current.join("\n").trim().to_string());
    }
    texts
        .into_iter()
        .filter_map(|text| bibtex_key(&text).map(|key| (key, text)))
        .collect()
}

/// The key of a BibTeX entry, such as `Smith2020` in `@article{Smith2020, …}`.
pub fn bibtex_key(bibtex: &str) -> Option<String> {
    static KEY: OnceLock<Regex> = OnceLock::new();
    let key = KEY.get_or_init(|| Regex::new(r"@\w+\s*\{\s*([^,\s]+)\s*,").expect("valid pattern"));
    key.captures(bibtex)
        .map(|found| found[1].trim().to_string())
        .filter(|key| !key.is_empty())
}

// --- shared request code -------------------------------------------------

/// What one service supplied and how it went.
struct Run {
    service: &'static str,
    entries: Vec<Entry>,
    errors: Vec<String>,
}

impl Run {
    fn new(service: &'static str) -> Self {
        Self {
            service,
            entries: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn attempt(&self, stage: &str, min_refs: usize) -> Attempt {
        let outcome = if self.entries.len() >= min_refs {
            "complete"
        } else if self.entries.is_empty() {
            "failed"
        } else {
            "short"
        };
        Attempt {
            stage: stage.to_string(),
            service: self.service.to_string(),
            outcome: outcome.to_string(),
            references: self.entries.len(),
            error_count: self.errors.len(),
            errors: self.errors.iter().take(ERRORS_KEPT).cloned().collect(),
        }
    }
}

fn is_loopback(url: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|prefix| url.starts_with(prefix))
}

fn client_for(settings: &Settings) -> Result<Client, reqwest::Error> {
    let mut builder = Client::builder().timeout(settings.timeout);
    if is_loopback(&settings.endpoints.semantic_scholar)
        && is_loopback(&settings.endpoints.openalex)
    {
        builder = builder.no_proxy();
    }
    builder.build()
}

/// Reads a JSON object. A refusal with status 403 or 429 is retried after a
/// wait; every other failure is returned as a short, stable sentence.
async fn get_json(
    client: &Client,
    settings: &Settings,
    url: &str,
    query: &[(&str, String)],
    headers: &[(&str, String)],
) -> Result<Map<String, Value>, String> {
    for attempt in 1..=REQUEST_ATTEMPTS {
        let mut request = client
            .get(url)
            .query(query)
            .header("Accept", "application/json");
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                "request timed out".to_string()
            } else if error.is_connect() {
                "connection failed".to_string()
            } else {
                "request failed".to_string()
            }
        })?;
        let status = response.status();
        if (status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS)
            && attempt < REQUEST_ATTEMPTS
        {
            let named = response
                .headers()
                .get("Retry-After")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<f64>().ok());
            tokio::time::sleep(settings.backoff.wait(attempt, named)).await;
            continue;
        }
        if !status.is_success() {
            return Err(format!("HTTP {}", status.as_u16()));
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| "response could not be read".to_string())?;
        return match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Object(object)) => Ok(object),
            Ok(_) => Err("response was not a JSON object".to_string()),
            Err(_) => Err("response was not valid JSON".to_string()),
        };
    }
    Err("request refused after repeated attempts".to_string())
}

/// Counts failures in a row so a service that is down is given up quickly.
#[derive(Default)]
struct Streak(usize);

impl Streak {
    fn failed(&mut self) -> bool {
        self.0 += 1;
        self.0 >= FAILURES_IN_A_ROW
    }

    fn succeeded(&mut self) {
        self.0 = 0;
    }
}

fn give_up(run: &mut Run, service: &str) {
    tracing::warn!(
        service,
        "giving up on reference service after repeated failures"
    );
    run.errors.push(format!(
        "stopped after {FAILURES_IN_A_ROW} failed requests in a row"
    ));
}

fn text_of(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| !text.is_empty())
}

fn int_of(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|float| float as i64)),
        _ => None,
    }
}

/// Percent-encodes a path segment.
fn encode_segment(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// --- Semantic Scholar ----------------------------------------------------

async fn semantic_scholar(settings: &Settings, client: &Client, queries: &[String]) -> Run {
    let mut run = Run::new(SEMANTIC_SCHOLAR);
    let base = settings.endpoints.semantic_scholar.trim_end_matches('/');
    let headers: Vec<(&str, String)> = settings
        .semantic_scholar_key
        .iter()
        .map(|key| ("x-api-key", key.clone()))
        .collect();
    let mut seen_papers: HashSet<String> = HashSet::new();
    let mut used_keys: HashSet<String> = HashSet::new();
    let mut streak = Streak::default();

    'queries: for query in queries {
        if run.entries.len() >= settings.min_refs {
            break;
        }
        let search = get_json(
            client,
            settings,
            &format!("{base}/paper/search"),
            &[
                ("query", query.clone()),
                ("limit", settings.semantic_scholar_hits.to_string()),
                (
                    "fields",
                    "paperId,title,year,venue,citationCount".to_string(),
                ),
            ],
            &headers,
        )
        .await;
        let hits = match search {
            Ok(body) => {
                streak.succeeded();
                body
            }
            Err(error) => {
                run.errors.push(format!("search for \"{query}\": {error}"));
                if streak.failed() {
                    give_up(&mut run, SEMANTIC_SCHOLAR);
                    break 'queries;
                }
                continue;
            }
        };
        let mut candidates: Vec<(String, i64)> = hits
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let id = text_of(row.get("paperId"))?;
                (!seen_papers.contains(&id))
                    .then(|| (id, int_of(row.get("citationCount")).unwrap_or(0)))
            })
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        candidates.dedup_by(|a, b| a.0 == b.0);

        for (paper_id, hit_count) in candidates {
            if run.entries.len() >= settings.min_refs {
                break;
            }
            seen_papers.insert(paper_id.clone());
            let url = format!("{base}/paper/{}", encode_segment(&paper_id));
            let details = get_json(
                client,
                settings,
                &url,
                &[(
                    "fields",
                    "paperId,title,year,venue,citationCount,citationStyles,url".to_string(),
                )],
                &headers,
            )
            .await;
            let details = match details {
                Ok(body) => {
                    streak.succeeded();
                    body
                }
                Err(error) => {
                    run.errors.push(format!("details of {paper_id}: {error}"));
                    if streak.failed() {
                        give_up(&mut run, SEMANTIC_SCHOLAR);
                        break 'queries;
                    }
                    continue;
                }
            };
            let Some(bibtex) = text_bibtex(details.get("citationStyles")) else {
                continue;
            };
            let Some(key) = bibtex_key(&bibtex) else {
                continue;
            };
            if !used_keys.insert(key.clone()) {
                continue;
            }
            run.entries.push(Entry {
                reference: Reference {
                    id: format!("ref.s2.{paper_id}"),
                    paper_id,
                    citation_key: key,
                    title: details.get("title").and_then(|v| text_of(Some(v))),
                    year: int_of(details.get("year")),
                    venue: text_of(details.get("venue")),
                    citation_count: int_of(details.get("citationCount")).or(Some(hit_count)),
                    query: query.clone(),
                    source_url: url,
                },
                bibtex,
            });
        }
    }
    run
}

/// The BibTeX text of Semantic Scholar's `citationStyles` object.
fn text_bibtex(styles: Option<&Value>) -> Option<String> {
    let bibtex = styles?.get("bibtex")?.as_str()?.trim();
    (!bibtex.is_empty()).then(|| bibtex.to_string())
}

// --- OpenAlex ------------------------------------------------------------

async fn openalex(settings: &Settings, client: &Client, queries: &[String]) -> Run {
    let mut run = Run::new(OPENALEX);
    let mut used_papers: HashSet<String> = HashSet::new();
    let mut used_keys: HashSet<String> = HashSet::new();
    let mut streak = Streak::default();

    for query in queries {
        if run.entries.len() >= settings.max_refs {
            break;
        }
        let mut params = vec![
            ("search", query.clone()),
            ("per-page", settings.openalex_hits.to_string()),
        ];
        if let Some(mailto) = &settings.mailto {
            params.push(("mailto", mailto.clone()));
        }
        let body =
            match get_json(client, settings, &settings.endpoints.openalex, &params, &[]).await {
                Ok(body) => {
                    streak.succeeded();
                    body
                }
                Err(error) => {
                    run.errors.push(format!("search for \"{query}\": {error}"));
                    if streak.failed() {
                        give_up(&mut run, OPENALEX);
                        break;
                    }
                    continue;
                }
            };
        for work in body
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if run.entries.len() >= settings.max_refs {
                break;
            }
            let Some(paper_id) = text_of(work.get("id")) else {
                continue;
            };
            if used_papers.contains(&paper_id) {
                continue;
            }
            let Some(parts) = WorkParts::read(work) else {
                continue;
            };
            let key = unused_key(&parts.key_stem(), &used_keys);
            let bibtex = parts.bibtex(&key);
            used_papers.insert(paper_id.clone());
            used_keys.insert(key.clone());
            let short_id = paper_id.rsplit('/').next().unwrap_or(&paper_id).to_string();
            run.entries.push(Entry {
                reference: Reference {
                    id: format!("ref.openalex.{short_id}"),
                    paper_id: paper_id.clone(),
                    citation_key: key,
                    title: Some(parts.title.clone()),
                    year: Some(parts.year),
                    venue: parts.venue.clone(),
                    citation_count: int_of(work.get("cited_by_count")).or(Some(0)),
                    query: query.clone(),
                    source_url: parts.doi_url.clone().unwrap_or(paper_id),
                },
                bibtex,
            });
        }
    }
    run
}

/// The parts of an OpenAlex work that a BibTeX entry is built from.
struct WorkParts {
    title: String,
    authors: Vec<String>,
    year: i64,
    venue: Option<String>,
    doi_url: Option<String>,
}

impl WorkParts {
    /// Reads a work; a work without a title, an author or a year has no entry.
    fn read(work: &Value) -> Option<Self> {
        let title = text_of(work.get("display_name"))?;
        let year = int_of(work.get("publication_year"))?;
        let authors = author_names(work);
        if authors.is_empty() {
            return None;
        }
        let venue = work
            .get("primary_location")
            .and_then(|location| location.get("source"))
            .and_then(|source| text_of(source.get("display_name")));
        let doi_url = text_of(work.get("doi"))
            .or_else(|| work.get("ids").and_then(|ids| text_of(ids.get("doi"))));
        Some(Self {
            title,
            authors,
            year,
            venue,
            doi_url,
        })
    }

    /// First author's family name, year and first long title word.
    fn key_stem(&self) -> String {
        let family = self
            .authors
            .first()
            .and_then(|name| name.split_whitespace().last())
            .map(ascii_letters)
            .filter(|family| !family.is_empty())
            .unwrap_or_else(|| "Anon".to_string());
        let word = self
            .title
            .split(|c: char| !c.is_alphanumeric())
            .map(ascii_letters)
            .find(|word| {
                word.len() >= 4
                    && !["with", "from", "that", "this", "using", "their"]
                        .contains(&word.to_lowercase().as_str())
            })
            .unwrap_or_default();
        format!("{family}{}{word}", self.year)
    }

    fn bibtex(&self, key: &str) -> String {
        let kind = if self.venue.is_some() {
            "article"
        } else {
            "misc"
        };
        let mut lines = vec![
            format!("  title={{{}}}", escape(&self.title)),
            format!(
                "  author={{{}}}",
                self.authors
                    .iter()
                    .map(|name| escape(name))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            format!("  year={{{}}}", self.year),
        ];
        if let Some(venue) = &self.venue {
            lines.push(format!("  journal={{{}}}", escape(venue)));
        }
        if let Some(url) = &self.doi_url {
            lines.push(format!("  url={{{}}}", escape_url(url)));
        }
        format!("@{kind}{{{key},\n{}\n}}", lines.join(",\n"))
    }
}

/// A key not in `used`: the stem, or the stem with `b`, `c`, … appended.
fn unused_key(stem: &str, used: &HashSet<String>) -> String {
    if !used.contains(stem) {
        return stem.to_string();
    }
    ('b'..='z')
        .map(|suffix| format!("{stem}{suffix}"))
        .find(|candidate| !used.contains(candidate))
        .unwrap_or_else(|| {
            (2..)
                .map(|n| format!("{stem}{n}"))
                .find(|candidate| !used.contains(candidate))
                .expect("some suffix is unused")
        })
}

fn ascii_letters(text: &str) -> String {
    let letters: String = text.chars().filter(char::is_ascii_alphanumeric).collect();
    let mut chars = letters.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

/// Ordered author names of an OpenAlex work. The display name is used unless
/// it contains non-Latin letters and the raw name does not.
fn author_names(work: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for authorship in work
        .get("authorships")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let display = authorship
            .get("author")
            .and_then(|author| text_of(author.get("display_name")))
            .unwrap_or_default();
        let raw = text_of(authorship.get("raw_author_name")).unwrap_or_default();
        let display = if !display.is_empty() && !raw.is_empty() && has_non_latin_letters(&display) {
            String::new()
        } else {
            display
        };
        let name = if display.is_empty() { raw } else { display };
        if !name.is_empty() && seen.insert(name.to_lowercase()) {
            names.push(name);
        }
    }
    names
}

fn has_non_latin_letters(name: &str) -> bool {
    name.chars().any(|c| {
        c.is_alphabetic()
            && !(c.is_ascii()
                || ('\u{00C0}'..='\u{024F}').contains(&c)
                || ('\u{1E00}'..='\u{1EFF}').contains(&c))
    })
}

/// Makes text safe inside a braced BibTeX field.
fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
    {
        match c {
            '{' | '}' | '\\' => {}
            '&' | '%' | '$' | '#' | '_' => {
                out.push('\\');
                out.push(c);
            }
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            _ => out.push(c),
        }
    }
    out
}

/// Makes a URL safe inside a braced BibTeX field; `\url` handles the rest.
fn escape_url(url: &str) -> String {
    url.chars()
        .filter(|c| !matches!(c, '{' | '}' | '\\'))
        .collect()
}

/// The part of a validated query plan that the provenance file keeps.
pub fn plan_metadata(plan: &Value) -> Value {
    let field = |name: &str, empty: Value| plan.get(name).cloned().unwrap_or(empty);
    let text = |name: &str| {
        Value::String(
            plan.get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string(),
        )
    };
    json!({
        "status": text("status"),
        "central_topic": text("central_topic"),
        "core_methods": field("core_methods", json!([])),
        "comparators": field("comparators", json!([])),
        "benchmarks": field("benchmarks", json!([])),
        "architectural_shifts": field("architectural_shifts", json!([])),
        "result_driven_themes": field("result_driven_themes", json!([])),
        "forbidden_query_patterns": field("forbidden_query_patterns", json!([])),
        "queries": field("queries", json!([])),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_cleaned_cut_and_deduplicated() {
        let long = "x".repeat(300);
        let cleaned = normalize_queries(&[
            "  circle   packing ".into(),
            "Circle packing".into(),
            String::new(),
            long,
        ]);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[0], "circle packing");
        assert_eq!(cleaned[1].len(), 200);
    }

    #[test]
    fn keys_are_read_from_entries() {
        assert_eq!(
            bibtex_key("@article{Smith2020Deep,\n title={x}}").as_deref(),
            Some("Smith2020Deep")
        );
        assert_eq!(bibtex_key("not an entry"), None);
        assert_eq!(bibtex_key("@misc{ , a=b}"), None);
    }

    #[test]
    fn a_bibliography_splits_at_entry_starts() {
        let parts = split_entries("@a{one,\n x={1}\n}\n\n@b{two,\n y={2}\n}\n");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "one");
        assert!(parts[1].1.contains("y={2}"));
    }

    #[test]
    fn waits_follow_the_named_delay_inside_the_bounds() {
        let backoff = Backoff::default();
        assert_eq!(backoff.wait(2, None), Duration::from_secs(3));
        assert_eq!(backoff.wait(1, Some(0.0)), Duration::from_millis(500));
        assert_eq!(backoff.wait(1, Some(90.0)), Duration::from_secs(20));
        assert_eq!(backoff.wait(1, Some(f64::NAN)), Duration::from_millis(1500));
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(encode_segment("ab-1/2 3"), "ab-1%2F2%203");
    }

    #[test]
    fn an_openalex_work_becomes_a_stable_entry() {
        let work = json!({
            "id": "https://openalex.org/W1",
            "display_name": "A circle packing algorithm_v2 & more",
            "publication_year": 2003,
            "doi": "https://doi.org/10.1/x",
            "authorships": [
                {"author": {"display_name": "Charles R. Collins"}},
                {"author": {"display_name": "Charles R. Collins"}},
                {"author": {"display_name": "Kenneth Stephenson"}}
            ],
            "primary_location": {"source": {"display_name": "Computational Geometry"}}
        });
        let parts = WorkParts::read(&work).unwrap();
        assert_eq!(parts.key_stem(), "Collins2003Circle");
        let text = parts.bibtex("Collins2003Circle");
        assert!(text.starts_with("@article{Collins2003Circle,"));
        assert!(text.contains("author={Charles R. Collins and Kenneth Stephenson}"));
        assert!(text.contains("algorithm\\_v2 \\& more"));
        assert_eq!(bibtex_key(&text).as_deref(), Some("Collins2003Circle"));
        assert_eq!(text, parts.bibtex("Collins2003Circle"));
    }

    #[test]
    fn a_work_without_a_title_author_or_year_has_no_entry() {
        let base = json!({"id": "i", "display_name": "T", "publication_year": 2000,
            "authorships": [{"author": {"display_name": "A B"}}]});
        assert!(WorkParts::read(&base).is_some());
        for field in ["display_name", "publication_year", "authorships"] {
            let mut work = base.clone();
            work.as_object_mut().unwrap().remove(field);
            assert!(WorkParts::read(&work).is_none(), "{field}");
        }
    }

    #[test]
    fn a_non_latin_display_name_gives_way_to_the_raw_name() {
        let work = json!({"authorships": [
            {"author": {"display_name": "Аlan Turing"}, "raw_author_name": "Alan Turing"},
            {"author": {"display_name": "Ada Lovelace"}, "raw_author_name": "A. Lovelace"}
        ]});
        assert_eq!(author_names(&work), vec!["Alan Turing", "Ada Lovelace"]);
    }

    #[test]
    fn repeated_keys_get_a_letter() {
        let used: HashSet<String> = ["K".to_string(), "Kb".to_string()].into();
        assert_eq!(unused_key("K", &used), "Kc");
        assert_eq!(unused_key("Z", &used), "Z");
    }

    #[test]
    fn files_round_trip_and_merge_keeping_existing_entries_first() {
        let dir = tempfile::tempdir().unwrap();
        let entry = |key: &str, query: &str| Entry {
            reference: Reference {
                id: format!("ref.s2.{key}"),
                paper_id: key.to_string(),
                citation_key: key.to_string(),
                title: None,
                year: Some(2020),
                venue: None,
                citation_count: Some(1),
                query: query.to_string(),
                source_url: String::new(),
            },
            bibtex: format!("@misc{{{key},\n  title={{{key}}}\n}}"),
        };
        let first = Retrieval {
            entries: vec![entry("A", "q1"), entry("B", "q1")],
            source: "semantic_scholar".into(),
            queries: vec!["q1".into()],
            attempts: Vec::new(),
        };
        store(dir.path(), &first, &json!({"status": "ready"})).unwrap();
        let second = Retrieval {
            entries: vec![entry("B", "q2"), entry("C", "q2")],
            source: "openalex".into(),
            queries: vec!["Q1".into(), "q2".into()],
            attempts: Vec::new(),
        };
        let merged = store_merged(dir.path(), &second, &json!({"status": "ready"})).unwrap();
        let keys: Vec<_> = merged
            .references
            .iter()
            .map(|r| r.citation_key.as_str())
            .collect();
        assert_eq!(keys, ["A", "B", "C"]);
        assert_eq!(merged.count, 3);
        assert_eq!(merged.queries, ["q1", "q2"]);
        assert_eq!(merged.reference_query_plans.len(), 2);
        assert_eq!(merged.references[1].query, "q1");
        let (entries, _) = load(dir.path());
        assert_eq!(entries.len(), 3);
        let bib = std::fs::read_to_string(dir.path().join(BIB_FILE)).unwrap();
        assert!(bib.ends_with("}\n"));
        assert_eq!(bib.matches("@misc").count(), 3);
    }
}
