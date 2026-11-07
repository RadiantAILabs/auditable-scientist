//! Explicit degraded outcome for unavailable sealed final evaluation.
use crate::{
    phases::task::files::write_json,
    phases::{BuildError, PhaseCtx, PhaseRegistry, RegisterError},
};
use polaris::{
    graph::Graph,
    system::{param::Res, system},
};
use polaris_cosmos::flow::{PhaseStatus, RunHandle};
#[system]
async fn final_record(
    run: Res<RunHandle>,
) -> Result<PhaseStatus, polaris::system::system::SystemError> {
    let present = run.root.join("task/task_contract.json").is_file();
    let reason = if present {
        "unsupported: sealed final evaluation is not built"
    } else {
        "no task_contract.json; sealed final evaluation not run"
    };
    write_json(
        &run.root.join("final_evaluation/final_launch_failure.json"),
        &serde_json::json!({"status":if present {"unsupported"} else {"degraded"},"reason":reason,"task_contract_present":present}),
    )?;
    Ok(PhaseStatus::Degraded(reason.into()))
}
/// Register final-evaluation capability resources.
pub fn register(_reg: &mut PhaseRegistry) -> Result<(), RegisterError> {
    Ok(())
}
/// Add the final status system without running a replacement scorer.
pub fn build(mut graph: Graph, _ctx: &PhaseCtx) -> Result<Graph, BuildError> {
    graph.system(final_record).named("final.record").done();
    Ok(graph)
}
