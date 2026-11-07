//! Every role of the paper phase renders with its prompt and skills, and every
//! vendored file carries its licence header and edit record.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use polaris_cosmos::flow::{FlowSettings, RunHandle, RunMode, StepKey, stage};
use polaris_scientist::config::ScientistConfig;
use polaris_scientist::phases::paper::calls::{self, ALL_ROLES, CallEnv, Request};
use polaris_scientist::phases::paper::state::{PaperPlan, PaperState};
use polaris_scientist::roles;

use crate::harness::crate_path;

fn files_under(root: &Path, extension: &str, into: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .expect("read folder")
        .map(|e| e.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files_under(&path, extension, into);
        } else if path.extension().is_some_and(|e| e == extension) {
            into.push(path);
        }
    }
}

#[test]
fn every_role_of_the_book_renders_with_no_placeholder_left() {
    let cfg = ScientistConfig::default();
    let assets = crate_path("assets");
    let book = roles::load(&assets.join("roles"), &cfg).expect("role book");
    let plan = PaperPlan::from_config(&cfg, StepKey::new("paper").expect("key")).expect("plan");
    let state = PaperState {
        task_goal: "Pack circles {{not a placeholder}} in a square.".into(),
        ..PaperState::default()
    };
    let dir = tempfile::tempdir().expect("temp dir");
    let paper = dir.path().join("paper");
    std::fs::create_dir_all(&paper).expect("paper folder");
    std::fs::write(paper.join("paper_data.json"), "{}").expect("data");
    let env = CallEnv {
        plan: &plan,
        state: &state,
        paper_dir: &paper,
        assets: &assets,
        roles: &book,
    };
    let handle = RunHandle {
        run_id: "fixture".into(),
        root: dir.path().to_path_buf(),
        mode: RunMode::Run,
    };
    let settings = FlowSettings {
        assets: assets.clone(),
        task_dir: dir.path().join("task"),
        model_id: "model".into(),
        ..FlowSettings::default()
    };
    assert_eq!(ALL_ROLES.len(), 11, "every paper role is checked");
    for (index, spec) in ALL_ROLES.iter().enumerate() {
        let role = book
            .roles
            .get(spec.key)
            .unwrap_or_else(|| panic!("the role book has no {}", spec.key));
        let call = calls::assemble(
            &env,
            Request {
                role: spec,
                step: StepKey::new("paper")
                    .and_then(|key| key.child(&format!("probe{index}")))
                    .expect("key"),
                feedback: Some("Feedback with {{braces}} in it.".into()),
            },
        )
        .unwrap_or_else(|e| panic!("{}: {e}", spec.key));
        let workspace = dir.path().join(format!("work{index}"));
        let request = stage(&call, role, &workspace, 1_000_000, &handle, &settings)
            .unwrap_or_else(|e| panic!("{} does not render: {e}", spec.key));
        assert!(
            !request.system_prompt.contains("{{PAPER_")
                && !request.system_prompt.contains("{{TASK_"),
            "{} left a placeholder",
            spec.key
        );
        assert!(
            request
                .turn_prompt
                .contains("Feedback with {{braces}} in it."),
            "{}: feedback data is passed through unchanged",
            spec.key
        );
        for skill in &role.skills {
            let name = Path::new(skill).file_name().expect("skill name");
            assert!(
                workspace
                    .join("skills")
                    .join(name)
                    .join("SKILL.md")
                    .is_file(),
                "{}: skill {skill} is not staged",
                spec.key
            );
        }
    }
}

#[test]
fn a_role_gets_only_the_variables_its_prompt_uses() {
    let cfg = ScientistConfig::default();
    let assets = crate_path("assets");
    let book = roles::load(&assets.join("roles"), &cfg).expect("role book");
    let plan = PaperPlan::from_config(&cfg, StepKey::new("paper").expect("key")).expect("plan");
    let state = PaperState::default();
    let env = CallEnv {
        plan: &plan,
        state: &state,
        paper_dir: Path::new("/paper"),
        assets: &assets,
        roles: &book,
    };
    let call = calls::assemble(
        &env,
        Request {
            role: &calls::POSTHOC_RANKER,
            step: StepKey::new("paper")
                .and_then(|key| key.child("rank"))
                .expect("key"),
            feedback: None,
        },
    )
    .expect("call");
    let names: Vec<&String> = call.stage.vars.keys().collect();
    assert!(names.contains(&&"POSTHOC_RANKING_PATH".to_string()));
    assert!(!names.contains(&&"PAPER_MIN_REFS".to_string()), "{names:?}");
    assert_eq!(
        call.stage.vars["POSTHOC_RANKING_PATH"],
        "outputs/posthoc_ranking.json"
    );
}

#[test]
fn every_vendored_file_has_its_licence_header_and_edit_record() {
    let root = crate_path("assets/prompts/paper");
    let mut markdown = Vec::new();
    files_under(&root, "md", &mut markdown);
    files_under(&crate_path("assets/venues"), "md", &mut markdown);
    assert!(markdown.len() >= 15, "found {} files", markdown.len());
    let mut headers: BTreeMap<PathBuf, (String, String)> = BTreeMap::new();
    for path in &markdown {
        if path.file_name().is_some_and(|n| n == "NOTICE") {
            continue;
        }
        let text = std::fs::read_to_string(path).expect("read prompt");
        let mut lines = text.lines();
        headers.insert(
            path.clone(),
            (
                lines.next().unwrap_or_default().to_string(),
                lines.next().unwrap_or_default().to_string(),
            ),
        );
    }
    for (path, (source, adapted)) in &headers {
        let name = path.strip_prefix(crate_path("assets")).unwrap().display();
        assert!(
            source.starts_with("<!-- Source: ")
                && source.contains("Apache-2.0")
                && source.ends_with("-->"),
            "{name}: licence header is {source:?}"
        );
        assert!(
            adapted.starts_with("<!-- adapted: ") && adapted.ends_with("-->"),
            "{name}: edit record is {adapted:?}"
        );
    }
    for folder in ["assets/prompts/paper", "assets/venues/iclr"] {
        for file in ["LICENSE", "NOTICE"] {
            assert!(crate_path(folder).join(file).is_file(), "{folder}/{file}");
        }
    }
}

#[test]
fn no_vendored_prompt_names_a_feature_that_is_not_built() {
    let mut markdown = Vec::new();
    files_under(&crate_path("assets/prompts/paper"), "md", &mut markdown);
    for path in markdown {
        let text = std::fs::read_to_string(&path).expect("read prompt");
        let body: String = text.lines().skip(2).collect::<Vec<_>>().join("\n");
        for term in [
            "memory-log",
            "GLOBAL_MEMORY",
            "generate-concept-figure",
            "OPENAI",
            "GEMINI",
            "Claude Code",
            "downstream_study",
        ] {
            assert!(
                !body.contains(term),
                "{} still mentions {term}",
                path.display()
            );
        }
    }
}
