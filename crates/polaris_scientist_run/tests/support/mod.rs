//! A scripted model and a ready-made runner for the tests. Nothing here
//! reaches a network or reads a credential.
#![allow(dead_code)] // each test file uses its own part

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use polaris::models::llm::{
    ContentBlockDelta, ContentBlockStartData, GenerationError, LlmProvider, LlmRequest,
    LlmResponse, LlmStream, StopReason, StreamEvent, Usage,
};
use polaris::models::{ModelRegistry, ModelsPlugin};
use polaris::system::plugin::{Plugin, PluginId, Version};
use polaris::system::server::Server;
use polaris_cosmos::flow::{AgentLimits, AgentRequest, StepKey, StopFlag, ToolGrant};
use polaris_scientist_run::runner::{RunnerConfig, SessionRunner};
use tokio::sync::Notify;

/// The provider name the scripted model registers under.
pub const PROVIDER: &str = "anthropic";

/// A model id the built-in price table knows: $2 in and $10 out per million
/// tokens, $0.20 per million cached tokens read.
pub const MODEL_ID: &str = "anthropic/claude-sonnet-5-5";

/// What the scripted model says next.
pub enum Reply {
    /// One tool call.
    Tool {
        name: &'static str,
        arguments: serde_json::Value,
        usage: Usage,
    },
    /// A text answer.
    Text { text: &'static str, usage: Usage },
    /// A text answer after `delay`.
    Slow {
        text: &'static str,
        delay: Duration,
        usage: Usage,
    },
    /// Signals `entered`, then never yields again.
    Stall { entered: Arc<Notify> },
    /// The provider refuses the request.
    Refuse,
}

/// Usage with only input and output tokens.
pub fn billed(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input + output),
        ..Default::default()
    }
}

pub fn tool(name: &'static str, arguments: serde_json::Value) -> Reply {
    Reply::Tool {
        name,
        arguments,
        usage: billed(100, 10),
    }
}

pub fn text(text: &'static str) -> Reply {
    Reply::Text {
        text,
        usage: billed(100, 10),
    }
}

pub fn text_billed(text: &'static str, usage: Usage) -> Reply {
    Reply::Text { text, usage }
}

/// One request the model received.
#[derive(Clone, Debug)]
pub struct Seen {
    pub system: String,
    /// The conversation as the model read it, in debug form so a test can
    /// search it for what a tool answered.
    pub messages: String,
    pub tools: Vec<String>,
    pub max_output_tokens: Option<u32>,
}

type Script = (String, VecDeque<Reply>);

/// A scripted [`LlmProvider`]. Each script is picked by the text its needle
/// finds in the request's system prompt, which carries the call's folder, so
/// several calls on one runner follow their own scripts.
#[derive(Clone, Default)]
pub struct ScriptedProvider {
    scripts: Arc<Mutex<Vec<Script>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl ScriptedProvider {
    pub fn script(&self, needle: &str, replies: Vec<Reply>) {
        self.scripts
            .lock()
            .unwrap()
            .push((needle.to_string(), replies.into()));
    }

    /// Drop every script and every recorded request, for a test that makes so
    /// many calls that the record itself would show up as growth.
    pub fn forget(&self) {
        self.scripts.lock().unwrap().clear();
        self.seen.lock().unwrap().clear();
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn request_count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    pub fn requests_for(&self, needle: &str) -> Vec<Seen> {
        self.requests()
            .into_iter()
            .filter(|seen| seen.system.contains(needle))
            .collect()
    }
}

fn single_block(
    block: ContentBlockStartData,
    delta: ContentBlockDelta,
    stop_reason: StopReason,
    usage: Usage,
) -> impl futures_util::Stream<Item = Result<StreamEvent, GenerationError>> {
    futures_util::stream::iter([
        Ok(StreamEvent::ContentBlockStart { index: 0, block }),
        Ok(StreamEvent::ContentBlockDelta { index: 0, delta }),
        Ok(StreamEvent::ContentBlockStop { index: 0 }),
        Ok(StreamEvent::MessageStop {
            stop_reason,
            usage,
            id: None,
            model: None,
        }),
    ])
}

impl LlmProvider for ScriptedProvider {
    fn name(&self) -> &'static str {
        PROVIDER
    }

    async fn generate(&self, _: &str, _: LlmRequest) -> Result<LlmResponse, GenerationError> {
        unreachable!("the agent streams its model calls")
    }

    async fn stream(&self, _: &str, request: LlmRequest) -> Result<LlmStream, GenerationError> {
        let system = request.system.clone().unwrap_or_default();
        self.seen.lock().unwrap().push(Seen {
            system: system.clone(),
            messages: format!("{:?}", request.messages),
            tools: request
                .tools
                .iter()
                .flatten()
                .map(|tool| tool.name.clone())
                .collect(),
            max_output_tokens: request.max_output_tokens.map(std::num::NonZeroU32::get),
        });
        let reply = self
            .scripts
            .lock()
            .unwrap()
            .iter_mut()
            .find(|(needle, _)| system.contains(needle.as_str()))
            .and_then(|(_, replies)| replies.pop_front())
            .expect("a scripted reply is left for this request");
        let stream: LlmStream = match reply {
            Reply::Tool {
                name,
                arguments,
                usage,
            } => single_block(
                ContentBlockStartData::ToolCall {
                    id: format!("{name}-call"),
                    call_id: None,
                    name: name.to_string(),
                },
                ContentBlockDelta::ToolCall {
                    arguments: arguments.to_string(),
                },
                StopReason::ToolUse,
                usage,
            )
            .boxed(),
            Reply::Text { text, usage } => single_block(
                ContentBlockStartData::Text,
                ContentBlockDelta::Text(text.to_string()),
                StopReason::EndTurn,
                usage,
            )
            .boxed(),
            Reply::Slow { text, delay, usage } => futures_util::stream::once(async move {
                tokio::time::sleep(delay).await;
            })
            .flat_map(move |()| {
                single_block(
                    ContentBlockStartData::Text,
                    ContentBlockDelta::Text(text.to_string()),
                    StopReason::EndTurn,
                    usage.clone(),
                )
            })
            .boxed(),
            Reply::Stall { entered } => futures_util::stream::once(async move {
                entered.notify_one();
                Ok(StreamEvent::ContentBlockStart {
                    index: 0,
                    block: ContentBlockStartData::Text,
                })
            })
            .chain(futures_util::stream::pending())
            .boxed(),
            Reply::Refuse => {
                return Err(GenerationError::Auth("scripted refusal".into()));
            }
        };
        Ok(stream)
    }
}

/// Registers a [`ScriptedProvider`] with the model registry.
pub struct ScriptedModelPlugin {
    pub provider: ScriptedProvider,
}

impl Plugin for ScriptedModelPlugin {
    const ID: &'static str = "test::scripted_model";
    const VERSION: Version = Version::new(0, 1, 0);

    fn dependencies(&self) -> Vec<PluginId> {
        vec![PluginId::of::<ModelsPlugin>()]
    }

    fn build(&self, server: &mut Server) {
        server
            .get_resource_mut::<ModelRegistry>()
            .expect("ModelsPlugin builds first")
            .register_llm_provider(self.provider.clone());
    }
}

/// Collects everything the process logs, so a test can search it.
#[derive(Clone, Default)]
pub struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    /// What has been logged so far.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    /// Log to this capture on the current thread until the guard drops. Use it
    /// in a test that runs on a single-threaded runtime.
    pub fn install(&self) -> tracing::subscriber::DefaultGuard {
        tracing::subscriber::set_default(self.subscriber())
    }

    /// Log to one capture for the rest of the process, whichever thread logs.
    pub fn install_global() -> &'static LogCapture {
        static GLOBAL: std::sync::OnceLock<LogCapture> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(|| {
            let capture = LogCapture::default();
            tracing::subscriber::set_global_default(capture.subscriber())
                .expect("no other subscriber is installed");
            capture
        })
    }

    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
        tracing_subscriber::fmt()
            .with_writer(self.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A runner on a scripted model, and the folder that holds its calls.
pub struct Fixture {
    pub runner: Arc<SessionRunner>,
    pub provider: ScriptedProvider,
    /// The results root.
    pub results: tempfile::TempDir,
    /// The run directory, which holds the task's Python environment.
    pub run_root: tempfile::TempDir,
}

/// The configuration the fixture starts with.
pub fn config_for(results: &Path, run_root: &Path) -> RunnerConfig {
    let mut config = RunnerConfig::new(results, run_root);
    config.model = MODEL_ID.to_string();
    config.max_output_tokens = 4096;
    config.shell_timeout = Duration::from_secs(20);
    config
}

pub async fn fixture() -> Fixture {
    fixture_with(|_| {}).await
}

pub async fn fixture_with(adjust: impl FnOnce(&mut RunnerConfig)) -> Fixture {
    build_fixture(adjust, None).await
}

/// A fixture whose runner stops every call when `flag` is raised.
pub async fn fixture_stoppable(flag: &StopFlag) -> Fixture {
    build_fixture(|_| {}, Some(flag.clone())).await
}

async fn build_fixture(adjust: impl FnOnce(&mut RunnerConfig), stop: Option<StopFlag>) -> Fixture {
    let results = tempfile::tempdir().unwrap();
    let run_root = tempfile::tempdir().unwrap();
    let mut config = config_for(results.path(), run_root.path());
    adjust(&mut config);
    let provider = ScriptedProvider::default();
    let runner = SessionRunner::start_with(&config, {
        let provider = provider.clone();
        move |server| {
            server.add_plugins(ScriptedModelPlugin { provider });
        }
    })
    .await
    .expect("the runner starts");
    let runner = match stop {
        Some(flag) => runner.with_stop(flag),
        None => runner,
    };
    Fixture {
        runner: Arc::new(runner),
        provider,
        results,
        run_root,
    }
}

impl Fixture {
    /// A new folder for one call, under the results root.
    pub fn folder(&self, name: &str) -> PathBuf {
        let folder = self.results.path().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        folder.canonicalize().unwrap()
    }

    /// The needle that picks the scripted replies for the call in `folder`.
    pub fn key(folder: &Path) -> String {
        format!("Working directory: {}", folder.display())
    }

    /// A request for the call in `folder`, with generous limits.
    pub fn request(&self, name: &str, folder: &Path) -> AgentRequest {
        request(name, folder)
    }
}

/// A request for the call in `folder`, with generous limits and no tools.
pub fn request(name: &str, folder: &Path) -> AgentRequest {
    AgentRequest {
        step: StepKey::new(name).expect("a valid step key"),
        role: name.to_string(),
        workspace: folder.to_path_buf(),
        system_prompt: format!("You are the {name} role."),
        turn_prompt: format!("Do the {name} task."),
        limits: AgentLimits {
            max_turns: 20,
            max_micros: 5_000_000,
            timeout: Duration::from_secs(30),
            max_output_tokens: None,
        },
        grants: ToolGrant::default(),
    }
}
