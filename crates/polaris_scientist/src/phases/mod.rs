//! Phase registration boundary and immutable graph construction context.
use crate::config::ScientistConfig;
use polaris_cosmos::flow::{RoleBook, StepKey};
use std::sync::Arc;
pub mod ablations;
pub mod drafting;
pub mod final_eval;
pub mod investigation;
pub mod paper;
pub mod posthoc;
pub mod review;
pub mod task;
/// Failure while registering a phase capability.
pub type RegisterError = Box<dyn std::error::Error + Send + Sync>;
/// Failure while constructing a phase graph.
pub type BuildError = Box<dyn std::error::Error + Send + Sync>;
/// Resource initializers contributed by phase modules.
#[derive(Default)]
pub struct PhaseRegistry {
    installers: Vec<Installer>,
}
type Installer = Box<dyn FnOnce(&mut polaris::system::server::Server) + Send + Sync>;
impl PhaseRegistry {
    /// Registers one immutable shared resource.
    pub fn global<T: polaris::system::resource::GlobalResource>(&mut self, value: T) {
        self.installers.push(Box::new(move |server| {
            server.insert_global(value);
        }));
    }
    /// Registers a fresh local resource factory for each campaign context.
    pub fn local<T: polaris::system::resource::LocalResource>(
        &mut self,
        factory: impl Fn() -> T + Send + Sync + 'static,
    ) {
        self.installers.push(Box::new(move |server| {
            server.register_local(factory);
        }));
    }
    /// Installs contributed resources before creating the campaign context.
    pub fn install(self, server: &mut polaris::system::server::Server) {
        for install in self.installers {
            install(server);
        }
    }
}
/// Read-only values available while composing a phase graph.
#[derive(Clone)]
pub struct PhaseCtx {
    /// Validated immutable campaign configuration.
    pub config: Arc<ScientistConfig>,
    /// Merged immutable role policies.
    pub roles: Arc<RoleBook>,
    /// Durable phase prefix used to construct step keys.
    pub prefix: StepKey,
}
