//! Campaign-level edges; phase internals remain replaceable graph modules.
use super::*;
use crate::phases;
use polaris::{graph::Graph, system::param::Res};
use polaris_cosmos::flow::{
    self, EventLog, PhaseControl, PhasePolicy, PhaseStatus, RunHandle, bind,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
/// Builds every registered phase through its public phase boundary.
pub fn build(cfg: &ScientistConfig, roles: &RoleBook) -> Result<Graph, CampaignError> {
    let mut bodies = BTreeMap::new();
    for (name, builder) in [
        (
            "task_drafting",
            phases::task::build as fn(Graph, &PhaseCtx) -> Result<Graph, BuildError>,
        ),
        ("investigation", phases::investigation::build),
        ("ablations", phases::ablations::build),
        ("final_evaluation", phases::final_eval::build),
        ("posthoc", phases::posthoc::build),
        ("paper", phases::paper::build),
        ("review", phases::review::build),
    ] {
        let ctx = PhaseCtx {
            config: Arc::new(cfg.clone()),
            roles: Arc::new(roles.clone()),
            prefix: StepKey::new(name)?,
        };
        bodies.insert(name.into(), builder(Graph::new(), &ctx)?);
    }
    build_with_phases(cfg, roles, bodies)
}
/// Builds the campaign with replaceable phase bodies, useful for offline assemblies.
pub fn build_with_phases(
    _cfg: &ScientistConfig,
    roles: &RoleBook,
    mut bodies: BTreeMap<String, Graph>,
) -> Result<Graph, CampaignError> {
    let mut graph = Graph::new();
    graph.system(super::open_node).named("run.open").done();
    for (name, policy) in [
        ("task_drafting", PhasePolicy::Fatal),
        ("investigation", PhasePolicy::Fatal),
    ] {
        graph.append(flow::phase(
            name,
            policy,
            bodies.remove(name).unwrap_or_default(),
        ))?;
    }
    graph
        .system(bind::<
            (Res<RunHandle>, Res<ScientistConfig>, Res<PhaseControl>),
            bool,
            _,
        >("win", |(handle, cfg, control), _| {
            Box::pin(async move {
                Ok(!control.halted()
                    && (generated_winner(&handle.root)
                        || cfg.string("campaign.no_winner_policy") == "best_available"))
            })
        }))
        .named("win")
        .done();
    graph.add_conditional_branch::<bool,_,_,_>("win.branch",|go|*go,|g|{g.append(flow::phase("ablations",PhasePolicy::Degrade,bodies.remove("ablations").unwrap_or_default())).expect("fresh phase");},|g|{g.add_system(bind::<(Res<PhaseControl>,Res<EventLog>),(),_>("ablations.skip",|(control,events),_|Box::pin(async move{if !control.halted(){let note="no generated winner";control.set_status("ablations",PhaseStatus::Skipped(note.into()))?;events.emit("phase_end",Some("ablations"),json!({"phase_name":"ablations","label":"ablations","status":"skipped","return_code":0,"elapsed":"0s","elapsed_seconds":0,"note":note}))?;}Ok(())})));});
    for (name, policy) in [
        ("final_evaluation", PhasePolicy::Degrade),
        ("posthoc", PhasePolicy::Degrade),
        ("paper", PhasePolicy::Incomplete),
    ] {
        graph.append(flow::phase(
            name,
            policy,
            bodies.remove(name).unwrap_or_default(),
        ))?;
    }
    graph.add_system(bind::<Res<PhaseControl>, bool, _>(
        "review.ready",
        |control, _| Box::pin(async move { Ok(!control.halted() && !control.incomplete()) }),
    ));
    graph.add_conditional_branch::<bool,_,_,_>("review.branch",|ready|*ready,|g|{g.append(flow::phase("review",PhasePolicy::Degrade,bodies.remove("review").unwrap_or_default())).expect("fresh review phase");},|g|{g.add_system(bind::<(Res<PhaseControl>,Res<EventLog>),(),_>("review.unavailable",|(control,events),_|Box::pin(async move{if !control.halted(){control.set_status("review",PhaseStatus::Skipped("paper incomplete".into()))?;events.emit("phase_end",Some("review"),json!({"phase_name":"review","label":"review","status":"skipped","return_code":0,"elapsed":"0s","elapsed_seconds":0,"note":"paper incomplete"}))?;}Ok(())})));});
    graph.system(super::close_node).named("run.close").done();
    let used = flow::agent_roles(&graph);
    let missing = used
        .iter()
        .filter(|role| !roles.roles.contains_key(*role))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!("graph roles missing from role book: {}", missing.join(", ")).into());
    }
    Ok(graph)
}
/// Lists role-book entries unused by a built graph without silently discarding them.
pub fn unused_roles(graph: &Graph, roles: &RoleBook) -> BTreeSet<String> {
    let used = flow::agent_roles(graph);
    roles
        .roles
        .keys()
        .filter(|key| !used.contains(*key))
        .cloned()
        .collect()
}
/// Determines whether the saved generated best strictly improves the valid seed.
pub fn generated_winner(root: &Path) -> bool {
    let best = std::fs::read(root.join("investigation/best_program_manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let seed = std::fs::read(root.join("task/baseline/metrics.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    match (best, seed) {
        (Some(best), Some(seed)) => {
            best["correct"] == true
                && best["combined_score"]
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .zip(seed["combined_score"].as_f64().filter(|v| v.is_finite()))
                    .is_some_and(|(best, seed)| best > seed)
        }
        _ => false,
    }
}
