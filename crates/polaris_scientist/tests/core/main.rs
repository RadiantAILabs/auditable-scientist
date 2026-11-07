//! Offline configuration, role and graph-contract proofs.
use polaris_cosmos::flow::{RoleBook, parse_micros, render};
use polaris_scientist::{
    campaign,
    config::{ConfigFlags, ScientistConfig},
    roles,
};
use std::{collections::BTreeMap, path::PathBuf};
#[test]
fn config_sources_flags_and_resolved_document_preserve_exact_money() {
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first.toml");
    let second = tmp.path().join("second.toml");
    std::fs::write(&first, "[paper]\nmin_refs=10\n[budget]\nmax_usd=1.000003\n").unwrap();
    std::fs::write(&second, "[paper]\nmin_refs=12\n").unwrap();
    let cfg = ScientistConfig::resolve(
        &[first, second],
        &["paper.min_refs=15".into(), "budget.max_usd=2.000004".into()],
        ConfigFlags {
            max_usd: Some(3_000_005),
            results_root: Some(tmp.path().into()),
            ..ConfigFlags::default()
        },
    )
    .unwrap();
    assert_eq!(cfg.u64("paper.min_refs"), 15);
    assert_eq!(cfg.micros("budget.max_usd").unwrap(), 3_000_005);
    assert_eq!(cfg.path("paths.results_root"), tmp.path());
    let resolved = tmp.path().join("config.resolved.toml");
    cfg.write(&resolved).unwrap();
    let loaded = ScientistConfig::resolve(&[resolved], &[], ConfigFlags::default()).unwrap();
    assert_eq!(loaded.values, cfg.values);
}
#[test]
fn unknown_keys_invalid_types_and_shape_changes_are_refused() {
    for set in [
        "paper.min_ref=2",
        "task.drafting=1",
        "investigation.execution.workers=0",
        "budget.max_usd=0.0000001",
        "campaign.phases=['paper','investigation']",
    ] {
        assert!(
            ScientistConfig::resolve(&[], &[set.into()], ConfigFlags::default()).is_err(),
            "{set}"
        );
    }
    let old = ScientistConfig::default();
    let changed = ScientistConfig::resolve(
        &[],
        &["investigation.execution.workers=2".into()],
        ConfigFlags::default(),
    )
    .unwrap();
    assert!(
        old.compatible_resume(&changed)
            .unwrap_err()
            .to_string()
            .contains("shape key")
    );
    let lower = ScientistConfig::resolve(
        &[],
        &["investigation.execution.max_idea_cycles=2".into()],
        ConfigFlags::default(),
    )
    .unwrap();
    assert!(old.compatible_resume(&lower).is_err());
    let higher = ScientistConfig::resolve(
        &[],
        &[
            "investigation.execution.max_idea_cycles=4".into(),
            "budget.max_usd=25".into(),
        ],
        ConfigFlags::default(),
    )
    .unwrap();
    assert!(old.compatible_resume(&higher).is_ok());
}
#[test]
fn scripted_backend_is_gated_by_offline_environment() {
    let result = ScientistConfig::resolve(
        &[],
        &["runner.kind='scripted'".into()],
        ConfigFlags::default(),
    );
    if std::env::var("POLARIS_SCIENTIST_OFFLINE").as_deref() == Ok("1") {
        assert!(result.is_ok());
    } else {
        assert!(result.unwrap_err().to_string().contains("OFFLINE"));
    }
}
fn role_text(key: &str) -> String {
    format!(
        "[roles.{key}]\nprompt='worker'\nnode='write'\nmax_turns=4\nmax_usd=0.600003\ntimeout_seconds=20\nshell=[]\nread_only=false\nskills=[]\n"
    )
}
#[test]
fn role_fragments_merge_overrides_and_name_bad_roles() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.toml"), role_text("alpha")).unwrap();
    std::fs::write(tmp.path().join("b.toml"), role_text("beta")).unwrap();
    let cfg = ScientistConfig::resolve(
        &[],
        &[
            "roles.alpha.max_turns=7".into(),
            "roles.beta.max_usd=0.2".into(),
        ],
        ConfigFlags::default(),
    )
    .unwrap();
    let book = roles::load(tmp.path(), &cfg).unwrap();
    assert_eq!(book.roles.len(), 2);
    assert_eq!(book.roles["alpha"].max_turns, 7);
    assert_eq!(book.roles["alpha"].max_micros(), 600_003);
    assert_eq!(book.roles["beta"].max_micros(), 200_000);
    std::fs::write(tmp.path().join("c.toml"), role_text("alpha")).unwrap();
    assert!(
        RoleBook::load(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("duplicate role alpha")
    );
    std::fs::remove_file(tmp.path().join("c.toml")).unwrap();
    std::fs::write(tmp.path().join("b.toml"), "[roles.beta]\nprompt='worker'\n").unwrap();
    let error = RoleBook::load(tmp.path()).unwrap_err().to_string();
    assert!(error.contains("beta") && error.contains("missing field"));
}
#[test]
fn all_registered_graph_roles_exist_in_the_merged_book() {
    let cfg = ScientistConfig::default();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/roles");
    let book = roles::load(&root, &cfg).unwrap();
    let graph = campaign::build(&cfg, &book).unwrap();
    let used = polaris_cosmos::flow::agent_roles(&graph);
    let missing: Vec<_> = used
        .iter()
        .filter(|role| !book.roles.contains_key(*role))
        .collect();
    assert!(
        missing.is_empty(),
        "roles a graph calls and the book lacks: {missing:?}"
    );
    let unused = campaign::unused_roles(&graph, &book);
    assert!(unused.iter().all(|role| !used.contains(role)));
    // Shown with `--nocapture`: what the graphs call, and what the book holds that no graph calls.
    println!("roles the graphs call: {used:?}");
    println!("roles in the book that no graph calls: {unused:?}");
}
#[test]
fn only_the_roles_that_write_long_files_carry_an_output_limit_their_ceiling_can_pay_for() {
    let cfg = ScientistConfig::default();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/roles");
    let book = roles::load(&root, &cfg).unwrap();
    let limited: Vec<_> = book
        .roles
        .iter()
        .filter(|(_, role)| role.max_output_tokens.is_some())
        .map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(limited, ["paper_editor", "paper_repairer", "paper_writer"]);
    for key in limited {
        let role = &book.roles[key];
        // The cost guard reserves a request's prompt and its whole output
        // limit at $10 per million output tokens against the call's ceiling.
        // A limit that takes half the ceiling or more leaves too little room
        // for the prompt and a second request.
        let worst_output = u64::from(role.max_output_tokens.unwrap()) * 10;
        assert_eq!(role.max_output_tokens, Some(32_000), "{key}");
        assert!(
            worst_output * 2 <= role.max_micros(),
            "{key}: {worst_output} micro-dollars of output against {}",
            role.max_micros()
        );
    }
}
#[test]
fn a_role_output_limit_can_be_set_from_the_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("roles.toml"), role_text("ideation")).unwrap();
    let cfg = ScientistConfig::resolve(
        &[],
        &["roles.ideation.max_output_tokens=24000".into()],
        ConfigFlags::default(),
    )
    .unwrap();
    assert_eq!(
        roles::load(tmp.path(), &cfg).unwrap().roles["ideation"].max_output_tokens,
        Some(24_000)
    );
    let cfg = ScientistConfig::resolve(&[], &[], ConfigFlags::default()).unwrap();
    assert_eq!(
        roles::load(tmp.path(), &cfg).unwrap().roles["ideation"].max_output_tokens,
        None
    );
}
#[test]
fn a_role_output_limit_of_zero_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("roles.toml"), role_text("ideation")).unwrap();
    let cfg = ScientistConfig::resolve(
        &[],
        &["roles.ideation.max_output_tokens=0".into()],
        ConfigFlags::default(),
    )
    .unwrap();
    let error = roles::load(tmp.path(), &cfg).unwrap_err().to_string();
    assert!(
        error.contains("ideation") && error.contains("max_output_tokens"),
        "{error}"
    );
    std::fs::write(
        tmp.path().join("roles.toml"),
        role_text("ideation").replace("shell=[]", "max_output_tokens=0\nshell=[]"),
    )
    .unwrap();
    let error = RoleBook::load(tmp.path()).unwrap_err().to_string();
    assert!(
        error.contains("ideation") && error.contains("max_output_tokens"),
        "{error}"
    );
}
#[test]
fn a_missing_graph_role_is_a_build_error() {
    let cfg = ScientistConfig::default();
    let mut bodies = BTreeMap::new();
    bodies.insert(
        "paper".into(),
        polaris_cosmos::flow::agent("write", "unregistered"),
    );
    let result = campaign::build_with_phases(&cfg, &RoleBook::default(), bodies);
    let error = result.err().unwrap().to_string();
    assert!(error.contains("unregistered"));
}
#[test]
fn renderer_and_run_paths_keep_their_contracts() {
    assert!(
        render("alpha", "{{MISSING}}", &BTreeMap::new(), &BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("alpha")
    );
    assert_eq!(parse_micros("0.000001").unwrap(), 1);
    let root = campaign::run_root(
        std::path::Path::new("/results"),
        std::path::Path::new("/tasks/example"),
        "20261106_012345",
    );
    assert_eq!(
        root,
        PathBuf::from("/results/example/polaris/20261106_012345")
    );
    let id = campaign::new_run_id();
    assert_eq!(id.len(), 15);
    assert_eq!(&id[8..9], "_");
}

#[test]
fn phase_timeouts_seed_roles_before_explicit_role_overrides() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("roles.toml"), role_text("ideation")).unwrap();
    let cfg = ScientistConfig::resolve(
        &[],
        &["investigation.execution.idea_timeout_seconds=11".into()],
        ConfigFlags::default(),
    )
    .unwrap();
    assert_eq!(
        roles::load(tmp.path(), &cfg).unwrap().roles["ideation"].timeout_seconds,
        11
    );
    let cfg = ScientistConfig::resolve(
        &[],
        &[
            "investigation.execution.idea_timeout_seconds=11".into(),
            "roles.ideation.timeout_seconds=13".into(),
        ],
        ConfigFlags::default(),
    )
    .unwrap();
    assert_eq!(
        roles::load(tmp.path(), &cfg).unwrap().roles["ideation"].timeout_seconds,
        13
    );
    let cfg = ScientistConfig::resolve(
        &[],
        &["roles.ideation.max_turns=0".into()],
        ConfigFlags::default(),
    )
    .unwrap();
    assert!(
        roles::load(tmp.path(), &cfg)
            .unwrap_err()
            .to_string()
            .contains("ideation")
    );
}
