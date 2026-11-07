use polaris_cosmos::flow::{self, FlowSettings, RoleBook, RunHandle, RunMode, StepKey};
use polaris_scientist::phases::investigation::calls;
use std::{collections::BTreeMap, fs, path::Path};

/// Whether a vendored file names its licence: the research prompts carry an
/// SPDX line, the paper prompts a source line that points at the licence and
/// notice files of their folder.
fn names_licence(text: &str) -> bool {
    text.contains("SPDX-License-Identifier: Apache-2.0")
        || text.contains("(Apache-2.0; see LICENSE and NOTICE")
}

#[test]
fn every_research_role_renders_main_prompt_and_skills_with_licence_and_edit_records() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let roles = RoleBook::load(&assets.join("roles")).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let run = RunHandle {
        run_id: "render".into(),
        root: temp.path().into(),
        mode: RunMode::Run,
    };
    let settings = FlowSettings {
        assets: assets.clone(),
        task_dir: temp.path().join("task"),
        model_id: "offline".into(),
        ..FlowSettings::default()
    };
    for (role_key, role) in &roles.roles {
        let prompt = assets.join("prompts").join(format!("{}.md", role.prompt));
        let source = fs::read_to_string(&prompt).unwrap();
        assert!(names_licence(&source), "{role_key}");
        assert!(source.contains("adapted:"), "{role_key}");
        let call = calls::call(
            &run,
            &roles,
            "test",
            StepKey::new(format!("render/{role_key}")).unwrap(),
            role_key,
            Vec::new(),
            BTreeMap::new(),
            Vec::new(),
            "Render the instructions and stop.",
        )
        .unwrap();
        let work = temp.path().join(role_key);
        let request = flow::stage(&call, role, &work, 1_000_000, &run, &settings).unwrap();
        assert!(!request.system_prompt.contains("{{"), "{role_key}");
        for skill in &role.skills {
            let name = Path::new(skill).file_name().unwrap();
            let text = fs::read_to_string(work.join("skills").join(name).join("SKILL.md")).unwrap();
            assert!(!text.contains("{{"), "{role_key}/{skill}");
            assert!(names_licence(&text), "{role_key}/{skill}");
            assert!(text.contains("adapted:"));
        }
    }
}

/// The turn prompt a role is started with, for each briefing.
fn turn_prompt(role_key: &str, briefing: calls::Briefing) -> String {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let roles = RoleBook::load(&assets.join("roles")).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let run = RunHandle {
        run_id: "briefing".into(),
        root: temp.path().into(),
        mode: RunMode::Run,
    };
    let settings = FlowSettings {
        assets,
        task_dir: temp.path().join("task"),
        model_id: "offline".into(),
        ..FlowSettings::default()
    };
    let call = calls::call_briefed(
        &run,
        &roles,
        "test",
        StepKey::new(format!("briefing/{role_key}")).unwrap(),
        role_key,
        Vec::new(),
        BTreeMap::new(),
        Vec::new(),
        "Do the work.",
        briefing,
    )
    .unwrap();
    let work = temp.path().join(role_key);
    flow::stage(
        &call,
        &roles.roles[role_key],
        &work,
        1_000_000,
        &run,
        &settings,
    )
    .unwrap()
    .turn_prompt
}

#[test]
fn a_reading_briefing_names_each_skill_and_an_inline_one_does_not() {
    let reading = turn_prompt("ideation", calls::Briefing::ReadFirst);
    assert!(reading.starts_with("Do the work."), "{reading}");
    assert!(
        reading.contains("Read skills/create-idea/SKILL.md"),
        "{reading}"
    );
    assert!(reading.contains("Read only inputs/"), "{reading}");

    let inline = turn_prompt("ideation", calls::Briefing::Inline);
    assert_eq!(
        inline,
        "Do the work.\nWrite every deliverable under outputs/. Do not run the official evaluator. Then stop."
    );
    assert_eq!(calls::Briefing::default(), calls::Briefing::ReadFirst);
}
