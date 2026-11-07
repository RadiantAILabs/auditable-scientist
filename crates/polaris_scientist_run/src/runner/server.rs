//! The one agent server every call runs on.
//!
//! Built once. It holds what is the same for every call — the model provider,
//! the file tools, the shell, the agent — and nothing that belongs to a single
//! call. A call's own prompt, folder, tools, spend limit and permissions are
//! given to its session when the session is created.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use polaris::models::ModelsPlugin;
use polaris::models::pricing::PriceStore;
use polaris::plugins::{PersistenceAPI, PersistencePlugin};
use polaris::sessions::{InMemoryStore, SessionsAPI, SessionsPlugin};
use polaris::shell::{ShellConfig, ShellPlugin};
use polaris::system::server::Server;
use polaris::tools::ToolsPlugin;
use polaris_code::agents::{CodeAgent, CodeAgentPlugin};
use polaris_code::tools::approval::ApprovalShellPlugin;
use polaris_code::tools::{CodeToolsConfig, CodeToolsPlugin};
use polaris_code::usage::TokenMeter;
use polaris_ontrace::oxigraph::OxigraphPlugin;

use super::{RunnerConfig, RunnerError};

/// Command patterns the shell refuses whatever a call is granted.
///
/// The same list the interactive agent refuses.
pub(super) const DENIED_COMMANDS: &[&str] =
    &["rm -rf *", "rm -rf /", "sudo *", ":(){ :|:& };:", "mkfs *"];

/// What [`build`] gives back.
pub(super) struct Built {
    pub tests: super::testing::AuthorTests,
    /// The started server. Kept so it can be cleaned up.
    pub server: Server,
    /// The handle sessions are created through.
    pub sessions: SessionsAPI,
    /// The server's own meter. Every call carries a meter of its own, so this
    /// one stays at zero.
    pub meter: TokenMeter,
    /// The results root with symbolic links resolved.
    pub results_root: PathBuf,
}

/// The search path a call's shell commands see: the task's Python environment
/// first, then the usual system directories.
pub(super) fn shell_path(venv: &Path) -> String {
    format!("{}/bin:/usr/local/bin:/usr/bin:/bin", venv.display())
}

/// The shell every call shares: confined to the results root, with a cleared
/// environment that holds only `PATH` and `VIRTUAL_ENV` beyond the few
/// harmless names the shell always passes (`HOME`, `LANG` and the like).
///
/// Nothing from the process environment is copied in, so a credential the
/// process holds never reaches a command the model writes.
pub(super) fn shell_config(config: &RunnerConfig, results_root: &Path) -> ShellConfig {
    ShellConfig::new()
        .with_working_dir(results_root)
        .with_timeout(config.shell_timeout.as_secs().max(1))
        .with_max_output_bytes(config.shell_max_output_bytes)
        .with_allowed_dirs(vec![results_root.to_path_buf()])
        // Each call narrows this to its own patterns.
        .with_allowed_commands(vec!["*".to_string()])
        .with_denied_commands(DENIED_COMMANDS.iter().map(|s| (*s).to_string()).collect())
        .with_env_var("PATH", shell_path(&config.venv))
        .with_env_var("VIRTUAL_ENV", config.venv.display().to_string())
        .disable_cache()
}

/// Build and start the server, with `install_model` registering the model
/// provider after the model registry exists.
pub(super) async fn build(
    config: &RunnerConfig,
    install_model: impl FnOnce(&mut Server),
) -> Result<Built, RunnerError> {
    let results_root = config.results_root.canonicalize().map_err(|error| {
        RunnerError::Config(format!(
            "results root {} cannot be used: {error}",
            config.results_root.display()
        ))
    })?;
    let meter = TokenMeter::new().with_pricing_store(PriceStore::builtin());

    let mut server = Server::new();
    let tests = super::testing::AuthorTests::default();
    server.insert_global(meter.clone());
    server.add_plugins(ModelsPlugin);
    install_model(&mut server);
    server
        .add_plugins(ToolsPlugin)
        .add_plugins(super::testing::AuthorTestsPlugin(tests.clone()))
        .add_plugins(CodeToolsPlugin::new(CodeToolsConfig::new(&results_root)))
        .add_plugins(ShellPlugin::new(shell_config(config, &results_root)))
        .add_plugins(ApprovalShellPlugin)
        .add_plugins(OxigraphPlugin::in_memory())
        .add_plugins(PersistencePlugin)
        .add_plugins(SessionsPlugin::new(Arc::new(InMemoryStore::new())))
        .add_plugins(CodeAgentPlugin::new());
    server
        .finish()
        .await
        .map_err(|error| RunnerError::Server(error.to_string()))?;

    let persistence = server
        .api::<PersistenceAPI>()
        .ok_or_else(|| RunnerError::Server("the persistence API is missing".to_string()))?;
    let serializers = persistence.serializers();
    let sessions = server
        .api::<SessionsAPI>()
        .ok_or_else(|| RunnerError::Server("the sessions API is missing".to_string()))?
        .clone();
    sessions.set_serializers(serializers);
    sessions
        .register_agent(CodeAgent)
        .map_err(|error| RunnerError::Server(format!("the agent cannot be registered: {error}")))?;

    Ok(Built {
        tests,
        server,
        sessions,
        meter,
        results_root,
    })
}
