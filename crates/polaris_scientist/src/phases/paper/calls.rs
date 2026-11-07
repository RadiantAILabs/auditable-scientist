//! How the paper phase asks a role to work.
//!
//! Every role call stages the same things: a read-only copy of the paper
//! folder at `inputs/paper/`, for manuscript writers the venue template at
//! `inputs/venue/`, and an empty `outputs/` folder. A role's prompt names the
//! files it needs with `{{NAME}}` placeholders; [`vocabulary`] gives every
//! name a value, and [`assemble`] passes on only the names the role's prompt
//! and skills actually use, so a prompt may use any part of the vocabulary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use polaris_cosmos::flow::{AgentCall, RoleBook, StageSpec, StepKey};

use super::state::{PHASE, PaperPlan, PaperState};

/// Files only the manuscript-writing roles produce.
const MANUSCRIPT_FILES: [&str; 4] = [
    "paper.tex",
    "claims_grounding.json",
    "figures",
    "figure_manifest.json",
];

/// Variables that name a file of the paper folder: the placeholder and the
/// file. A role that produces the file sees `outputs/<file>`; every other role
/// sees the read-only copy at `inputs/paper/<file>`.
const ARTIFACTS: [(&str, &str); 14] = [
    ("REFERENCE_QUERY_PLAN_PATH", "reference_query_plan.md"),
    (
        "REFERENCE_QUERY_PLAN_JSON_PATH",
        "reference_query_plan.json",
    ),
    ("PAPER_STRATEGY_PATH", "paper_strategy.md"),
    ("PAPER_STRATEGY_JSON_PATH", "paper_strategy.json"),
    ("POSTHOC_RANKING_PATH", "posthoc_ranking.json"),
    ("DRAFT_REFERENCE_GAPS_PATH", "draft_reference_gaps.md"),
    (
        "DRAFT_REFERENCE_GAPS_JSON_PATH",
        "draft_reference_gaps.json",
    ),
    (
        "POSTDRAFT_REFERENCE_QUERY_PLAN_PATH",
        "postdraft_reference_query_plan.md",
    ),
    (
        "POSTDRAFT_REFERENCE_QUERY_PLAN_JSON_PATH",
        "postdraft_reference_query_plan.json",
    ),
    ("PAPER_TEX_PATH", "paper.tex"),
    ("PAPER_CLAIMS_PATH", "claims_grounding.json"),
    ("PAPER_FIGURES_DIR", "figures"),
    ("PAPER_FIGURE_MANIFEST_PATH", "figure_manifest.json"),
    ("PAPER_DIR", ""),
];

/// What a role does in the phase.
#[derive(Clone, Copy, Debug)]
pub struct RoleSpec {
    /// Key in the role book.
    pub key: &'static str,
    /// Files the role writes into `outputs/`.
    pub produces: &'static [&'static str],
    /// True for the roles that write the manuscript.
    pub manuscript: bool,
    /// True when the role answers in its final message and writes no file.
    pub replies: bool,
    /// The task text of the call.
    pub task: &'static str,
}

const WRITE_TASK: &str =
    "Do the task in your instructions. Write your results to outputs/ and then stop.";
const REPLY_TASK: &str = "Do the task in your instructions. Reply with the result in your final message. Write no files.";

/// Plans the queries for the bibliography.
pub const REFERENCE_PLANNER: RoleSpec = RoleSpec {
    key: "reference_planner",
    produces: &["reference_query_plan.md", "reference_query_plan.json"],
    manuscript: false,
    replies: false,
    task: WRITE_TASK,
};
/// Chooses the thesis and figure plan.
pub const PAPER_STRATEGIST: RoleSpec = RoleSpec {
    key: "paper_strategist",
    produces: &["paper_strategy.md", "paper_strategy.json"],
    manuscript: false,
    replies: false,
    task: WRITE_TASK,
};
/// Orders the post-hoc analyses.
pub const POSTHOC_RANKER: RoleSpec = RoleSpec {
    key: "posthoc_ranker",
    produces: &["posthoc_ranking.json"],
    manuscript: false,
    replies: false,
    task: WRITE_TASK,
};
/// Writes the first draft.
pub const PAPER_WRITER: RoleSpec = RoleSpec {
    key: "paper_writer",
    produces: &[],
    manuscript: true,
    replies: false,
    task: WRITE_TASK,
};
/// Finds missing citations in the draft.
pub const GAP_ANALYST: RoleSpec = RoleSpec {
    key: "citation_gap_analyst",
    produces: &["draft_reference_gaps.md", "draft_reference_gaps.json"],
    manuscript: false,
    replies: false,
    task: WRITE_TASK,
};
/// Plans the extra queries after the draft.
pub const POSTDRAFT_PLANNER: RoleSpec = RoleSpec {
    key: "reference_planner_postdraft",
    produces: &[
        "postdraft_reference_query_plan.md",
        "postdraft_reference_query_plan.json",
    ],
    manuscript: false,
    replies: false,
    task: WRITE_TASK,
};
/// Weaves the new references into the draft.
pub const PAPER_ENRICHER: RoleSpec = RoleSpec {
    key: "paper_enricher",
    produces: &[],
    manuscript: true,
    replies: false,
    task: "Do the task in your instructions. This is the citation-enrichment pass: the gap files in inputs/paper/ name the paragraphs to rewrite. Write the manuscript files to outputs/ and then stop.",
};
/// Audits the claims against the evidence.
pub const EVIDENCE_AUDITOR: RoleSpec = RoleSpec {
    key: "evidence_auditor",
    produces: &[],
    manuscript: false,
    replies: true,
    task: REPLY_TASK,
};
/// Reviews the presentation.
pub const PRESENTATION_REVIEWER: RoleSpec = RoleSpec {
    key: "presentation_reviewer",
    produces: &[],
    manuscript: false,
    replies: true,
    task: REPLY_TASK,
};
/// Repairs the manuscript.
pub const PAPER_REPAIRER: RoleSpec = RoleSpec {
    key: "paper_repairer",
    produces: &[],
    manuscript: true,
    replies: false,
    task: "Do the task in your instructions. The feedback to address is in inputs/paper/repair_feedback.md. Write the manuscript files to outputs/ and then stop.",
};
/// Edits the paper after an outside review (a later feature).
pub const PAPER_EDITOR: RoleSpec = RoleSpec {
    key: "paper_editor",
    produces: &[],
    manuscript: true,
    replies: false,
    task: WRITE_TASK,
};

/// Every role of the phase, in the order of the topology.
pub const ALL_ROLES: [&RoleSpec; 11] = [
    &REFERENCE_PLANNER,
    &PAPER_STRATEGIST,
    &POSTHOC_RANKER,
    &PAPER_WRITER,
    &GAP_ANALYST,
    &POSTDRAFT_PLANNER,
    &PAPER_ENRICHER,
    &EVIDENCE_AUDITOR,
    &PRESENTATION_REVIEWER,
    &PAPER_REPAIRER,
    &PAPER_EDITOR,
];

impl RoleSpec {
    fn writes(&self, file: &str) -> bool {
        self.produces.contains(&file) || (self.manuscript && MANUSCRIPT_FILES.contains(&file))
    }
}

/// What a call builder may read.
pub struct CallEnv<'a> {
    /// Settings fixed at build time.
    pub plan: &'a PaperPlan,
    /// The phase's state.
    pub state: &'a PaperState,
    /// The paper folder.
    pub paper_dir: &'a Path,
    /// Root of the assets (`prompts/`, `venues/`).
    pub assets: &'a Path,
    /// The role book.
    pub roles: &'a RoleBook,
}

/// The values every placeholder of the vocabulary takes for `role`.
#[must_use]
pub fn vocabulary(role: &RoleSpec, env: &CallEnv<'_>) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    let mut put = |name: &str, value: String| {
        vars.insert(name.to_string(), value);
    };
    put("TASK_GOAL", env.state.task_goal.clone());
    put("PAPER_FORMAT", env.plan.venue.clone());
    put("PAPER_MIN_REFS", env.plan.min_refs.to_string());
    put("PAPER_MAX_REFS", env.plan.max_refs.to_string());
    for (name, file) in [
        ("PAPER_DATA_PATH", "paper_data.json"),
        ("PAPER_BIB_PATH", "references.bib"),
        (
            "PAPER_REFERENCES_PROVENANCE_PATH",
            "references_provenance.json",
        ),
        ("PAPER_SCOPE_AUDIT_JSON_PATH", "paper_evidence_audit.json"),
        ("PAPER_SCOPE_AUDIT_MD_PATH", "paper_evidence_audit.md"),
        ("PAPER_VALIDATION_FEEDBACK_PATH", "repair_feedback.md"),
        ("PAPER_REVIEW_FEEDBACK_PATH", "review_feedback.md"),
        ("PAPER_AGGREGATED_DIR", "dossier/ablations/aggregated"),
        ("RESULTS_README", "dossier/ablations/README.md"),
    ] {
        put(name, format!("inputs/paper/{file}"));
    }
    put("CURRENT_PAPER_DIR", "inputs/paper".to_string());
    put("VENUE_DIR", "inputs/venue".to_string());
    put("VENUE_FILE_PATH", "inputs/venue/venue_brief.md".to_string());
    put(
        "PAPER_AUTHOR_BLOCK_PATH",
        "inputs/venue/author_block.tex".to_string(),
    );
    put(
        "PAPER_WRITER_SKILL",
        "skills/write-paper-standalone/SKILL.md".to_string(),
    );
    for (name, file) in ARTIFACTS {
        let value = if file.is_empty() {
            if role.manuscript {
                "outputs"
            } else {
                "inputs/paper"
            }
            .to_string()
        } else if role.writes(file) {
            format!("outputs/{file}")
        } else {
            format!("inputs/paper/{file}")
        };
        put(name, value);
    }
    vars
}

/// Names between `{{` and `}}` in a text.
fn placeholders(text: &str, into: &mut BTreeSet<String>) {
    let mut rest = text;
    while let Some((_, after)) = rest.split_once("{{") {
        let Some((name, tail)) = after.split_once("}}") else {
            return;
        };
        into.insert(name.trim().to_string());
        rest = tail;
    }
}

fn scan_markdown(path: &Path, into: &mut BTreeSet<String>) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            scan_markdown(&entry.path(), into);
        }
    } else if meta.is_file()
        && path.extension().is_some_and(|e| e == "md")
        && let Ok(text) = std::fs::read_to_string(path)
    {
        placeholders(&text, into);
    }
}

/// The placeholders the prompt and the staged skills of a role use.
#[must_use]
pub fn used_names(assets: &Path, roles: &RoleBook, key: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(role) = roles.roles.get(key) else {
        return names;
    };
    let prompts = assets.join("prompts");
    scan_markdown(&prompts.join(&role.prompt).with_extension("md"), &mut names);
    for skill in &role.skills {
        scan_markdown(&prompts.join(skill), &mut names);
    }
    names
}

/// One call, before its inputs and variables are attached.
pub struct Request<'a> {
    /// The role.
    pub role: &'a RoleSpec,
    /// Step key; unique for every call of a run.
    pub step: StepKey,
    /// Feedback for a retry, passed to the role in the task text.
    pub feedback: Option<String>,
}

/// Builds the call: staged inputs, the variables the role uses, the task
/// text and the files to look for afterwards.
///
/// # Errors
///
/// Returns a sentence when the role is not in the role book.
pub fn assemble(env: &CallEnv<'_>, request: Request<'_>) -> Result<AgentCall, String> {
    let role = request.role;
    if !env.roles.roles.contains_key(role.key) {
        return Err(format!("the role book has no role `{}`", role.key));
    }
    let used = used_names(env.assets, env.roles, role.key);
    let mut copy_in: Vec<(PathBuf, String)> = vec![(env.paper_dir.to_path_buf(), "paper".into())];
    // A role gets the venue template when it writes the manuscript or its
    // prompt points at a venue file.
    let wants_venue = role.manuscript
        || ["VENUE_DIR", "VENUE_FILE_PATH", "PAPER_AUTHOR_BLOCK_PATH"]
            .iter()
            .any(|name| used.contains(*name));
    if wants_venue {
        copy_in.push((
            env.assets.join("venues").join(&env.plan.venue),
            "venue".into(),
        ));
    }
    let mut vars = vocabulary(role, env);
    vars.retain(|name, _| used.contains(name));
    let mut turn = role.task.to_string();
    if let Some(feedback) = request.feedback {
        turn.push_str("\n\n{{FEEDBACK}}");
        vars.insert("FEEDBACK".to_string(), feedback);
    }
    Ok(AgentCall {
        step: request.step,
        role: role.key.to_string(),
        phase: PHASE.to_string(),
        stage: StageSpec {
            copy_in,
            vars,
            turn_prompt: Some(turn),
        },
        expect: role
            .produces
            .iter()
            .map(|f| format!("outputs/{f}"))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_found_in_text() {
        let mut names = BTreeSet::new();
        placeholders("a {{ONE}} b {{ TWO }} c {{broken", &mut names);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["ONE", "TWO"]);
    }

    #[test]
    fn producers_see_outputs_and_everyone_else_the_snapshot() {
        let plan = PaperPlan {
            prefix: StepKey::new("paper").expect("`paper` is a valid step key"),
            venue: "iclr".into(),
            two_stage: true,
            min_refs: 25,
            max_refs: 30,
            max_repair_rounds: 10,
            review_rounds: 0,
            tools: crate::latex::LatexTools::default(),
            refs: crate::phases::paper::refs::Settings::new(25, 30),
        };
        let state = PaperState::default();
        let roles = RoleBook::default();
        let env = CallEnv {
            plan: &plan,
            state: &state,
            paper_dir: Path::new("/p"),
            assets: Path::new("/a"),
            roles: &roles,
        };
        let planner = vocabulary(&REFERENCE_PLANNER, &env);
        assert_eq!(
            planner["REFERENCE_QUERY_PLAN_JSON_PATH"],
            "outputs/reference_query_plan.json"
        );
        assert_eq!(planner["PAPER_TEX_PATH"], "inputs/paper/paper.tex");
        assert_eq!(planner["PAPER_DIR"], "inputs/paper");
        let writer = vocabulary(&PAPER_WRITER, &env);
        assert_eq!(writer["PAPER_TEX_PATH"], "outputs/paper.tex");
        assert_eq!(writer["PAPER_FIGURES_DIR"], "outputs/figures");
        assert_eq!(writer["PAPER_DIR"], "outputs");
        assert_eq!(
            writer["DRAFT_REFERENCE_GAPS_JSON_PATH"],
            "inputs/paper/draft_reference_gaps.json"
        );
        assert_eq!(writer["PAPER_MIN_REFS"], "25");
        let auditor = vocabulary(&EVIDENCE_AUDITOR, &env);
        assert_eq!(
            auditor["PAPER_CLAIMS_PATH"],
            "inputs/paper/claims_grounding.json"
        );
    }
}
