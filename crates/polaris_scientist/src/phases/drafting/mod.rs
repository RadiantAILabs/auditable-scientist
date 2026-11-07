//! Optional task drafting reports an explicit unsupported capability.
use crate::{
    eval::EvalError,
    phases::{BuildError, PhaseCtx, PhaseRegistry, RegisterError},
};
use polaris::{graph::Graph, system::system};
#[system]
async fn drafting_not_built() -> Result<(), polaris::system::system::SystemError> {
    Err(EvalError("NotBuilt: task drafting".into()).into())
}
/// Register optional task-drafting resources.
pub fn register(_reg: &mut PhaseRegistry) -> Result<(), RegisterError> {
    Ok(())
}
/// Add an explicit terminal outcome for unimplemented task drafting.
pub fn build(mut graph: Graph, _ctx: &PhaseCtx) -> Result<Graph, BuildError> {
    graph
        .system(drafting_not_built)
        .named("draft.not_built")
        .done();
    Ok(graph)
}
