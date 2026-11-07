//! Running the paper phase inside a small campaign, entirely offline.
//!
//! The campaign runs only the `paper` phase (and the review phase after it).
//! The earlier phases' results come from the fixture folder `run/`, copied into
//! the run folder before the campaign starts. Role calls are answered by a
//! scripted runner from `tests/scenarios/paper`; TeX is a stand-in program;
//! the reference services are a stand-in server on the loopback address.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use polaris::graph::Graph;
use polaris_cosmos::flow::{Scenario, ScriptEntry, ScriptedRunner, StepKey};
use polaris_scientist::campaign;
use polaris_scientist::config::{ConfigFlags, ScientistConfig};
use polaris_scientist::latex::LatexTools;
use polaris_scientist::phases::paper::{self, PaperPlan, refs};
use polaris_scientist::records::Outcome;
use serde_json::{Value, json};

use crate::stand_in::{Reply, Request, StandIn};

/// A path below the crate.
pub fn crate_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// A stand-in program or fixture file.
pub fn fixture(name: &str) -> PathBuf {
    crate_path("tests/fixtures/paper").join(name)
}

/// The TeX programs of the stand-ins.
pub fn stand_in_tools() -> LatexTools {
    LatexTools {
        pdflatex: fixture("fake_pdflatex.sh").display().to_string(),
        bibtex: fixture("fake_bibtex.sh").display().to_string(),
        timeout: Duration::from_secs(60),
    }
}

/// Copies a folder, replacing `@RUN@` in JSON files with `run_root`.
pub fn copy_tree(from: &Path, to: &Path, run_root: &Path) {
    std::fs::create_dir_all(to).expect("create folder");
    for entry in std::fs::read_dir(from).expect("read folder") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &target, run_root);
        } else if entry.path().extension().is_some_and(|e| e == "json") {
            let text = std::fs::read_to_string(entry.path()).expect("read file");
            std::fs::write(
                target,
                text.replace("@RUN@", &run_root.display().to_string()),
            )
            .expect("write file");
        } else {
            std::fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

/// The catalogue the reference stand-in answers from.
fn catalogue() -> Vec<Value> {
    let text =
        std::fs::read_to_string(fixture("references/catalogue.json")).expect("catalogue file");
    serde_json::from_str(&text).expect("catalogue is JSON")
}

/// A stand-in for both reference services, answering from the catalogue.
pub fn reference_services() -> StandIn {
    let rows = catalogue();
    StandIn::start(move |request: &Request| {
        if request.path == "/graph/v1/paper/search" {
            let data: Vec<Value> = rows
                .iter()
                .map(|r| json!({"paperId": r["paperId"], "citationCount": r["citationCount"]}))
                .collect();
            return Reply::json(json!({"data": data}));
        }
        if let Some(id) = request.path.strip_prefix("/graph/v1/paper/")
            && let Some(row) = rows.iter().find(|r| r["paperId"] == id)
        {
            return Reply::json(row.clone());
        }
        if request.path == "/works" {
            return Reply::json(json!({"results": []}));
        }
        Reply::status(404)
    })
}

/// Settings that send retrieval to `services`.
pub fn refs_for(services: &StandIn, min_refs: usize, max_refs: usize) -> refs::Settings {
    let mut settings = refs::Settings::new(min_refs, max_refs);
    settings.endpoints = refs::Endpoints {
        semantic_scholar: format!("{}/graph/v1", services.url),
        openalex: format!("{}/works", services.url),
    };
    settings.backoff = refs::Backoff {
        unit: Duration::from_millis(1),
        least: Duration::ZERO,
        most: Duration::from_millis(5),
    };
    settings.timeout = Duration::from_secs(10);
    settings
}

/// One campaign that runs the paper phase.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub task: PathBuf,
    pub cfg: ScientistConfig,
    pub tools: LatexTools,
    pub refs: refs::Settings,
    /// Keeps the reference stand-in running for the life of the harness.
    pub _services: StandIn,
    pub review: bool,
}

/// What a run left behind.
pub struct Run {
    pub outcome: Outcome,
    pub root: PathBuf,
    pub calls: Vec<Value>,
}

impl Run {
    /// The role of every call that was answered, in order.
    pub fn roles(&self) -> Vec<String> {
        self.calls
            .iter()
            .filter(|c| c["played"] == true)
            .map(|c| c["role"].as_str().expect("role").to_string())
            .collect()
    }

    /// The paper folder.
    pub fn paper(&self) -> PathBuf {
        self.root.join("paper")
    }

    /// A JSON file of the paper folder.
    pub fn json(&self, name: &str) -> Value {
        let text = std::fs::read_to_string(self.paper().join(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    /// The status of the paper phase.
    pub fn paper_status(&self) -> String {
        self.outcome.phases["paper"].clone()
    }
}

impl Harness {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let task = dir.path().join("example");
        std::fs::create_dir_all(&task).expect("task dir");
        std::fs::write(task.join("evaluate.py"), "# frozen evaluator\n").expect("evaluator");
        let cfg = ScientistConfig::resolve(
            &[],
            &[r#"campaign.phases=["paper"]"#.to_string()],
            ConfigFlags {
                results_root: Some(dir.path().join("results")),
                max_usd: Some(50_000_000),
                ..ConfigFlags::default()
            },
        )
        .expect("config");
        let services = reference_services();
        let refs = refs_for(&services, 25, 30);
        Self {
            dir,
            task,
            cfg,
            tools: stand_in_tools(),
            refs,
            _services: services,
            review: true,
        }
    }

    /// Sets a configuration key to a TOML value.
    pub fn set(&mut self, key: &str, value: &str) {
        let parsed: toml::Table = toml::from_str(&format!("value = {value}")).expect("value");
        self.cfg.set(key, parsed["value"].clone()).expect("set key");
    }

    /// The run folder.
    pub fn root(&self) -> PathBuf {
        campaign::run_root(&self.cfg.path("paths.results_root"), &self.task, "fixture")
    }

    /// Copies the fixture results into the run folder.
    pub fn seed(&self) {
        let root = self.root();
        std::fs::create_dir_all(&root).expect("run folder");
        let canonical = root.canonicalize().expect("canonical run folder");
        copy_tree(&fixture("run"), &canonical, &canonical);
    }

    /// The scenario of the paper phase.
    pub fn scenario(&self) -> Scenario {
        Scenario::load(&crate_path("tests/scenarios/paper")).expect("paper scenario")
    }

    /// Runs the campaign and gathers what it left.
    pub async fn run(&self, scenario: Scenario) -> Run {
        self.run_with(scenario, |_| {}).await
    }

    /// Like [`Harness::run`], after `prepare` has changed the seeded run folder.
    pub async fn run_with(&self, scenario: Scenario, prepare: impl FnOnce(&Path)) -> Run {
        self.seed();
        prepare(&self.root());
        let calls = self.dir.path().join("calls");
        let runner = Arc::new(ScriptedRunner::new(scenario, &calls).expect("runner"));
        let mut plan = PaperPlan::from_config(&self.cfg, StepKey::new("paper").expect("key"))
            .expect("paper plan");
        plan.tools = self.tools.clone();
        plan.refs = self.refs.clone();
        let mut bodies: BTreeMap<String, Graph> = BTreeMap::new();
        bodies.insert(
            "paper".into(),
            paper::build_with_plan(Graph::new(), plan).expect("paper graph"),
        );
        if self.review {
            bodies.insert("review".into(), review_body(&self.cfg));
        }
        let outcome = campaign::run_with_phases(
            &self.task,
            self.cfg.clone(),
            runner,
            Some("fixture".into()),
            Some(bodies),
            crate_path("assets"),
        )
        .await
        .expect("campaign");
        let root = self.root();
        let calls_text = std::fs::read_to_string(calls.join("calls.jsonl")).unwrap_or_default();
        Run {
            outcome,
            root,
            calls: calls_text
                .lines()
                .map(|l| serde_json::from_str(l).expect("call line"))
                .collect(),
        }
    }
}

/// The review phase as the campaign builds it.
fn review_body(cfg: &ScientistConfig) -> Graph {
    let roles = polaris_cosmos::flow::RoleBook::default();
    let ctx = polaris_scientist::PhaseCtx {
        config: Arc::new(cfg.clone()),
        roles: Arc::new(roles),
        prefix: StepKey::new("review").expect("key"),
    };
    polaris_scientist::phases::review::build(Graph::new(), &ctx).expect("review graph")
}

/// The manuscript the paper scenario's writer leaves: `(paper.tex, claims,
/// figure manifest)`.
pub fn good_paper() -> (String, String, String) {
    let dir = crate_path("tests/scenarios/paper/roles/paper_writer/01/outputs");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect(name);
    (
        read("paper.tex"),
        read("claims_grounding.json"),
        read("figure_manifest.json"),
    )
}

impl Harness {
    /// A folder whose `outputs/` holds `files`, for a scripted answer.
    pub fn variant(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.dir.path().join("variants").join(name);
        for (relative, text) in files {
            let path = dir.join("outputs").join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("variant folder");
            std::fs::write(path, text).expect("variant file");
        }
        dir
    }

    /// A variant that holds the good paper with `edit` applied to its TeX.
    pub fn paper_variant(&self, name: &str, edit: impl Fn(&str) -> String) -> PathBuf {
        let (tex, claims, figures) = good_paper();
        self.variant(
            name,
            &[
                ("paper.tex", &edit(&tex)),
                ("claims_grounding.json", &claims),
                ("figure_manifest.json", &figures),
            ],
        )
    }
}

/// A scripted answer that writes `files` (a folder) into the call's workspace.
pub fn answer(files: &Path) -> ScriptEntry {
    ScriptEntry {
        files: Some(files.to_path_buf()),
        ..ScriptEntry::default()
    }
}

/// A scripted answer with a final text.
pub fn reply(text: &str) -> ScriptEntry {
    ScriptEntry {
        final_text: text.to_string(),
        ..ScriptEntry::default()
    }
}
