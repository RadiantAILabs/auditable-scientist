//! Runs each role call of a campaign as one agent session.
//!
//! [`SessionRunner`] implements [`TaskRunner`]. It owns one agent server and
//! gives every call a session of its own on that server: its own role prompt,
//! its own folder, its own tools and permissions, its own spend limit and its
//! own transcript. Nothing of one call is visible to another, so calls can run
//! side by side.
//!
//! # How a call ends
//!
//! A call returns a report whose [`StopCause`] says how it ended — finished,
//! out of turns, out of money, out of time, interrupted, or stopped by a model
//! error — together with what it spent. Anything that stops the spend from
//! being known is an [`AgentFailure`] instead: [`AgentFailure::Unmeasured`]
//! when the model has no price or reported no usage, [`AgentFailure::Setup`]
//! when no session could be started.
//!
//! | Ending | Result |
//! |--------|--------|
//! | The agent gave a final answer | `Finished` |
//! | The session's turn limit ran out | `TurnCap` |
//! | The call's spend limit refused a request | `CostCap` |
//! | The call's time limit ran out | `Timeout` |
//! | The campaign was told to stop | `Interrupted` |
//! | The provider failed, for example a rejected credential | `ModelError` |
//! | The model has no price, or a finished call reported no tokens | `Unmeasured` |
//! | The folder, limits or session could not be set up | `Setup` |
//!
//! # Money
//!
//! Each call has a meter of its own with a spend limit equal to the amount
//! granted. The meter prices tokens from the built-in price table, so no price
//! file is read or written. The cost is rounded up to a whole micro-dollar. A
//! request that is cut off while in flight, by the time limit or a stop, leaves
//! no token count behind; it is charged at its estimate, so a campaign's total
//! never falls below what was spent.
//!
//! # Credentials
//!
//! The model provider reads its credentials once, when the runner starts. The
//! shell the agent uses starts from an empty environment that holds only
//! `PATH` and `VIRTUAL_ENV` (plus the few harmless names the shell always
//! passes), so a command the model writes cannot read them.

mod call;
mod report;
mod server;
mod testing;
mod transcript;

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use polaris::models::pricing::PriceStore;
use polaris::sessions::SessionsAPI;
use polaris::system::server::Server;
use polaris_code::credentials::CredentialStore;
use polaris_code::providers::{self, Provider};
use polaris_code::usage::TokenMeter;
use polaris_cosmos::flow::{AgentFailure, AgentReport, AgentRequest, StopFlag, TaskRunner};
use polaris_scientist::config::ScientistConfig;

pub use transcript::TRANSCRIPT_FILE;

/// The model every campaign call uses.
pub const DEFAULT_MODEL: &str = "aws.bedrock/global.anthropic.claude-sonnet-5-5";

/// The most output tokens one response may hold, whatever a role asks for.
///
/// The model provider is registered with this as its ceiling. A request can
/// lower the provider's ceiling but never raise it, so the ceiling has to be
/// at least the largest limit any call sends.
pub const MAX_OUTPUT_TOKENS: u32 = 128_000;

/// What the runner needs to start.
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    /// Verified campaign settings for advisory author tests. Standalone calls
    /// have no evaluator capability unless the embedding supplies settings.
    pub author_test_config: Option<ScientistConfig>,
    /// The AWS region the model is called in.
    pub region: String,
    /// The model id, with its provider prefix.
    pub model: String,
    /// The most tokens the model may write in one response.
    pub max_output_tokens: u32,
    /// How long a call that has been told to stop is given to end on its own
    /// before it is dropped.
    pub interrupt_grace: Duration,
    /// The directory that holds every call's folder. File tools and the shell
    /// are confined to it.
    pub results_root: PathBuf,
    /// The Python environment commands run in. Its `bin` directory leads
    /// `PATH`, and `VIRTUAL_ENV` names it.
    pub venv: PathBuf,
    /// How long one shell command may run unless the model asks for another
    /// time with the `timeout_secs` argument of the shell tool. The call's own
    /// time limit bounds a command in either case.
    pub shell_timeout: Duration,
    /// The most output of one shell command that is kept.
    pub shell_max_output_bytes: usize,
}

impl RunnerConfig {
    /// A configuration with the campaign defaults: Sonnet 5.5 on Bedrock in
    /// `eu-west-2`, 16384 output tokens, a 15 second grace, and shell commands
    /// that by default run for at most 120 seconds and keep 64 KiB of output.
    ///
    /// `run_root` is the directory of the run; the task's Python environment is
    /// `<run_root>/task/.venv`.
    #[must_use]
    pub fn new(results_root: impl Into<PathBuf>, run_root: &Path) -> Self {
        Self {
            author_test_config: None,
            region: "eu-west-2".to_string(),
            model: DEFAULT_MODEL.to_string(),
            max_output_tokens: 16_384,
            interrupt_grace: Duration::from_secs(15),
            results_root: results_root.into(),
            venv: run_root.join("task").join(".venv"),
            shell_timeout: Duration::from_secs(120),
            shell_max_output_bytes: 64 * 1024,
        }
    }

    /// The output ceiling the model provider is registered with: the larger
    /// of [`MAX_OUTPUT_TOKENS`] and this runner's own setting.
    ///
    /// Every call sends a limit of its own (see
    /// [`call_output_limit`](Self::call_output_limit)); the provider lowers its
    /// ceiling to that limit but never raises it, so a call can use up to this
    /// many tokens and no more.
    #[must_use]
    pub fn provider_output_ceiling(&self) -> u32 {
        self.max_output_tokens.max(MAX_OUTPUT_TOKENS)
    }

    /// The output limit one call sends with every request, and the cost guard
    /// reserves for it: the call's own limit when its role sets one, else this
    /// runner's setting, and never more than the provider's ceiling.
    ///
    /// A role limit of zero counts as not set.
    #[must_use]
    pub fn call_output_limit(&self, role_limit: Option<u32>) -> u32 {
        role_limit
            .filter(|tokens| *tokens > 0)
            .unwrap_or(self.max_output_tokens)
            .min(self.provider_output_ceiling())
    }

    /// The `runner.*` settings of a resolved campaign configuration.
    ///
    /// # Errors
    ///
    /// [`RunnerError::Config`] if a setting is missing or out of range.
    pub fn from_config(
        config: &ScientistConfig,
        results_root: impl Into<PathBuf>,
        run_root: &Path,
    ) -> Result<Self, RunnerError> {
        let get = |key: &str| {
            config
                .value(key)
                .ok_or_else(|| RunnerError::Config(format!("missing configuration key {key}")))
        };
        let text = |key: &str| {
            get(key)?
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| RunnerError::Config(format!("{key} must be text")))
        };
        let number = |key: &str| {
            get(key)?
                .as_integer()
                .and_then(|n| u64::try_from(n).ok())
                .ok_or_else(|| RunnerError::Config(format!("{key} must be a whole number")))
        };
        let mut runner = Self::new(results_root, run_root);
        runner.author_test_config = Some(config.clone());
        runner.region = text("runner.region")?;
        runner.model = text("runner.model")?;
        runner.max_output_tokens = u32::try_from(number("runner.max_output_tokens")?)
            .map_err(|_| RunnerError::Config("runner.max_output_tokens is too large".into()))?;
        runner.interrupt_grace = Duration::from_secs(number("runner.interrupt_grace_seconds")?);
        Ok(runner)
    }
}

/// Why the runner could not start.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// A setting is missing, out of range, or names something unusable.
    #[error("{0}")]
    Config(String),
    /// The model provider failed its startup check, for example because no
    /// credentials were found.
    #[error("{0}")]
    Provider(String),
    /// The agent server could not be built.
    #[error("the agent server could not start: {0}")]
    Server(String),
}

/// Shared by every call.
struct Inner {
    tests: testing::AuthorTests,
    sessions: SessionsAPI,
    server: tokio::sync::Mutex<Server>,
    meter: TokenMeter,
    config: RunnerConfig,
    /// The results root with symbolic links resolved.
    results_root: PathBuf,
    stop: Option<StopFlag>,
    closed: AtomicBool,
}

/// Runs each role call as one session on one agent server.
///
/// Build it with [`SessionRunner::start`]. It is `Send + Sync`, and calls may
/// run at the same time.
pub struct SessionRunner {
    inner: Inner,
}

impl SessionRunner {
    /// Start the runner on the Bedrock provider.
    ///
    /// Credentials are read from the process environment, here and nowhere
    /// else. No stored credentials file is read.
    ///
    /// # Errors
    ///
    /// [`RunnerError`] if the settings are unusable, the provider fails its
    /// startup check, or the server cannot start.
    pub async fn start(config: &RunnerConfig) -> Result<Self, RunnerError> {
        NonZeroU32::new(config.max_output_tokens).ok_or_else(|| {
            RunnerError::Config("the output allowance must be at least one token".to_string())
        })?;
        // The provider's ceiling is only an upper bound: each call sends its
        // own limit, which can lower the ceiling but never raise it.
        let ceiling = NonZeroU32::new(config.provider_output_ceiling())
            .expect("the ceiling is at least MAX_OUTPUT_TOKENS");
        let ready = providers::prepare(
            Provider::Bedrock,
            Some(&config.region),
            &providers::process_env,
            &None::<CredentialStore>,
        )
        .await
        .map_err(|error| RunnerError::Provider(error.to_string()))?;
        tracing::info!("{}", ready.report().line());
        Self::start_with(config, |server| {
            ready.install(server, ceiling, PriceStore::builtin());
        })
        .await
    }

    /// Start the runner with `install_model` registering the model provider.
    ///
    /// The closure runs once the model registry exists. [`start`](Self::start)
    /// passes the Bedrock provider; a test passes a scripted one.
    ///
    /// # Errors
    ///
    /// [`RunnerError`] if the results root is unusable or the server cannot
    /// start.
    pub async fn start_with(
        config: &RunnerConfig,
        install_model: impl FnOnce(&mut Server),
    ) -> Result<Self, RunnerError> {
        let built = server::build(config, install_model).await?;
        Ok(Self {
            inner: Inner {
                tests: built.tests,
                sessions: built.sessions,
                server: tokio::sync::Mutex::new(built.server),
                meter: built.meter,
                config: config.clone(),
                results_root: built.results_root,
                stop: None,
                closed: AtomicBool::new(false),
            },
        })
    }

    /// Stop every call when `flag` is raised.
    ///
    /// A call in flight is asked to stop and ends as `Interrupted`; a call made
    /// afterwards returns `Interrupted` at once, without a session and without
    /// any request to the model.
    #[must_use]
    pub fn with_stop(mut self, flag: StopFlag) -> Self {
        self.inner.stop = Some(flag);
        self
    }

    /// How many sessions are open right now. Zero between calls.
    #[must_use]
    pub fn live_sessions(&self) -> usize {
        self.inner.sessions.list_live_sessions().len()
    }

    /// The server's own meter. Every call carries a meter of its own, so this
    /// one stays at zero; a figure above zero means a call was billed to the
    /// wrong place.
    #[must_use]
    pub fn server_meter(&self) -> &TokenMeter {
        &self.inner.meter
    }

    /// Release what the server holds. Calls made afterwards return
    /// [`AgentFailure::Setup`].
    pub async fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.server.lock().await.cleanup().await;
    }
}

#[async_trait::async_trait]
impl TaskRunner for SessionRunner {
    async fn run(&self, req: AgentRequest) -> Result<AgentReport, AgentFailure> {
        call::run_call(&self.inner, req).await
    }
}
