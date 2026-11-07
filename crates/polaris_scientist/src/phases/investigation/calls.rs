//! Staged role-call contracts shared by research phases.
use crate::{
    eval::EvalError,
    phases::task::{self, files::safe_relative},
};
use polaris_cosmos::flow::{
    AgentCall, AgentOutcome, RoleBook, RunHandle, StageSpec, StepKey, StopCause,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// Read a JSON artifact with contextual file errors.
pub fn read_json(path: &Path) -> Result<Value, EvalError> {
    serde_json::from_slice(
        &fs::read(path).map_err(|e| EvalError(format!("{}: {e}", path.display())))?,
    )
    .map_err(Into::into)
}
/// Read optional context without inventing an artifact.
pub fn context(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}
/// Private workspace corresponding to an agent's durable step key.
pub fn workspace(run: &RunHandle, key: &StepKey) -> PathBuf {
    polaris_cosmos::flow::latest_workspace(&run.root, key)
        .ok()
        .flatten()
        .unwrap_or_else(|| run.root.join("work").join(key.as_str()))
}
/// Whether a role returned normally; artifact validation remains separate.
pub fn finished(outcome: &AgentOutcome) -> bool {
    matches!(outcome,AgentOutcome::Ran {report,..} if report.stop==StopCause::Finished)
}
/// Whether a role timed out, ending retry loops immediately.
pub fn timed_out(outcome: &AgentOutcome) -> bool {
    matches!(outcome,AgentOutcome::Ran {report,..} if report.stop==StopCause::Timeout)
}
/// Fail a required step when its reservation or session did not run.
pub fn required(outcome: &AgentOutcome) -> Result<(), EvalError> {
    match outcome {
        AgentOutcome::Skipped(reason) => {
            Err(EvalError(format!("required role skipped: {reason:?}")))
        }
        AgentOutcome::Failed(reason) => Err(EvalError(format!("required role failed: {reason:?}"))),
        _ => Ok(()),
    }
}
fn placeholders(text: &str, keys: &mut BTreeSet<String>) {
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("}}") else { break };
        keys.insert(rest[..end].trim().to_owned());
        rest = &rest[end + 2..];
    }
}
/// How the turn prompt starts a role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Briefing {
    /// Send the agent to its staged skills and inputs before it writes anything.
    ///
    /// For roles whose prompt names files to work from.
    #[default]
    ReadFirst,
    /// The role prompt carries the whole task context, so the agent writes at
    /// once. Staged skills stay on disk for the agent to open if it chooses.
    Inline,
}

/// Prepare only the variables used by the role text and its staged skills.
///
/// Every semantic input is supplied by the owning phase. Unused upstream optional
/// context sections render empty; path variables always name the private folder.
/// The agent starts by reading its staged skills and inputs; see
/// [`call_briefed`] for roles that need no such start.
#[expect(
    clippy::too_many_arguments,
    reason = "The call boundary keeps role identity, staged inputs and expected outputs explicit."
)]
pub fn call(
    run: &RunHandle,
    roles: &RoleBook,
    phase: &str,
    key: StepKey,
    role: &str,
    copy_in: Vec<(PathBuf, String)>,
    vars: BTreeMap<String, String>,
    expect: Vec<String>,
    instructions: &str,
) -> Result<AgentCall, EvalError> {
    call_briefed(
        run,
        roles,
        phase,
        key,
        role,
        copy_in,
        vars,
        expect,
        instructions,
        Briefing::ReadFirst,
    )
}

/// Same as [`call`], with the way the turn prompt starts the role chosen.
#[expect(
    clippy::too_many_arguments,
    reason = "The call boundary keeps role identity, staged inputs and expected outputs explicit."
)]
pub fn call_briefed(
    run: &RunHandle,
    roles: &RoleBook,
    phase: &str,
    key: StepKey,
    role: &str,
    copy_in: Vec<(PathBuf, String)>,
    mut vars: BTreeMap<String, String>,
    expect: Vec<String>,
    instructions: &str,
    briefing: Briefing,
) -> Result<AgentCall, EvalError> {
    let role_spec = roles
        .roles
        .get(role)
        .ok_or_else(|| EvalError(format!("unknown role: {role}")))?;
    let assets = crate::campaign::assets_root().join("prompts");
    let prompt = assets.join(format!("{}.md", role_spec.prompt));
    let mut keys = BTreeSet::new();
    placeholders(&fs::read_to_string(&prompt)?, &mut keys);
    for skill in &role_spec.skills {
        let path = assets.join(skill);
        let path = if path.is_dir() {
            path.join("SKILL.md")
        } else {
            path
        };
        placeholders(&fs::read_to_string(path)?, &mut keys);
    }
    placeholders(instructions, &mut keys);

    let private = workspace(run, &key);
    let defaults = [
        ("TASK_DIR", "inputs/task"),
        ("TASK_MD_PATH", "inputs/task/task.md"),
        ("RESULTS_DIR", "outputs"),
        ("SEED_PROGRAM_DIR", "inputs/program_0"),
        ("GLOBAL_RESULTS_DIR", "outputs"),
        ("IDEA_OUTPUT_DIR", "outputs"),
        ("IDEA_BUNDLE_FILE", "outputs/idea_bundle.json"),
        ("TARGET_RUBRICS_FILE", "outputs/target_rubrics.json"),
        ("GLOBAL_MEMORY_MD_FILE", "outputs/memory.md"),
        ("GLOBAL_MEMORY_JSONL_FILE", "outputs/memory.jsonl"),
        ("CONCLUSIONS_MD_FILE", "outputs/conclusions.md"),
        (
            "CONCLUSIONS_HISTORY_FILE",
            "outputs/conclusions_history.jsonl",
        ),
        ("ACTIVE_IDEA_FILE", "inputs/idea_bundle.json"),
        ("ACTIVE_RUBRICS_FILE", "inputs/target_rubrics.json"),
        ("IDEAS_HISTORY_FILE", "inputs/ideas_history.jsonl"),
        ("ATTEMPTS_HISTORY_FILE", "inputs/attempts_history.jsonl"),
        ("BEST_MANIFEST_FILE", "inputs/best_program_manifest.json"),
        ("TASK_RUBRICS", "No task-level rubrics supplied."),
        ("MAX_PROGRAM_EVALS_PER_ATTEMPT", "1"),
    ];
    for (name, value) in defaults {
        vars.entry(name.into()).or_insert_with(|| value.into());
    }
    vars.entry("WORKSPACE".into())
        .or_insert_with(|| private.to_string_lossy().into_owned());
    let standard = ["TASK_DIR", "WORKSPACE", "STEP_KEY", "MAX_TURNS"];
    let vars = keys
        .into_iter()
        .filter(|key| !standard.contains(&key.as_str()))
        .map(|key| {
            let value = vars.remove(&key).unwrap_or_default();
            (key, value)
        })
        .collect();
    if copy_in.iter().any(|(_, name)| !safe_relative(name)) {
        return Err(EvalError("unsafe staged input name".into()));
    }
    let testing = matches!(
        role,
        "program_author" | "variant_author" | "variant_retrofit"
    );
    let evaluation = if testing {
        "You may choose test_candidate to run your candidate through the sealed task evaluator and read its advisory result in this session. At most one advisory test is available in this call. After testing, make any evidence-backed correction, record rubric evidence and memory, and finish. Tests never replace the host evaluation after you exit."
    } else {
        "Do not run the official evaluator."
    };
    let turn_prompt = match briefing {
        Briefing::ReadFirst => {
            let skill_instructions = role_spec
                .skills
                .iter()
                .filter_map(|skill| Path::new(skill).file_name())
                .map(|name| {
                    format!(
                        "Read skills/{}/SKILL.md before writing outputs.",
                        name.to_string_lossy()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "{instructions}\n{skill_instructions}\nRead only inputs/ and write every deliverable under outputs/. When cloning a frozen input for editing, use cp -r --no-preserve=mode so the output files and directories are writable; never use plain cp -r on frozen inputs. {evaluation} Then stop."
            )
        }
        Briefing::Inline => format!(
            "{instructions}\nWrite every deliverable under outputs/. {evaluation} Then stop."
        ),
    };
    Ok(AgentCall {
        step: key,
        role: role.into(),
        phase: phase.into(),
        stage: StageSpec {
            copy_in,
            vars,
            turn_prompt: Some(turn_prompt),
        },
        expect,
    })
}
/// Basic text context for phase-specific role prompts.
pub fn task_vars(root: &Path) -> BTreeMap<String, String> {
    let task = context(&root.join("task/task.md"));
    BTreeMap::from([
        ("TASK_MD".into(), task.clone()),
        ("TASK_GOAL".into(), task.clone()),
        ("TASK_DESCRIPTION".into(), task),
    ])
}
/// Explicitly stage the minimum public task text, excluding scorer and secrets.
pub fn public_task(root: &Path) -> Vec<(PathBuf, String)> {
    vec![(root.join("task/task.md"), "task/task.md".into())]
}
/// Shared integer cap lookup for role iteration prompts.
pub fn cap(cfg: &crate::config::ScientistConfig, key: &str, default: u64) -> String {
    task::number(cfg, key, default).to_string()
}
