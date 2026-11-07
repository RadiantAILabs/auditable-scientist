//! One role call, run as one agent session.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use polaris::graph::GraphExecutor;
use polaris::plugins::{IOContent, IOMessage, IOProvider as _, UserIO};
use polaris::sessions::{AgentTypeId, SessionId};
use polaris_code::agents::{
    AgentConfig, CodeAgent, PermissionMode, PolicySnapshot, UnattendedGrants,
};
use polaris_code::exec::ExecIOProvider;
use polaris_code::run::{self, TurnOutcome};
use polaris_code::usage::TokenMeter;
use polaris_cosmos::flow::{AgentFailure, AgentReport, AgentRequest, StopCause, Usage};

use super::report::{self, Ended, Why};
use super::{Inner, transcript};

/// How often an interrupt is raised again while the turn has not stopped. An
/// interrupt raised before the turn reaches its first wait point is lost, so
/// one request is not enough.
const INTERRUPT_RETRY: Duration = Duration::from_millis(100);

/// How long to wait for the transcript writer after the session has closed its
/// output.
const TRANSCRIPT_FLUSH: Duration = Duration::from_secs(5);

/// A report for a call that ended before it spent anything.
fn unspent(inner: &Inner, stop: StopCause) -> AgentReport {
    AgentReport {
        stop,
        usage: Usage {
            model_id: inner.config.model.clone(),
            ..Usage::default()
        },
        final_text: String::new(),
        transcript: None,
    }
}

/// The call's folder, resolved, if it exists and lies inside the results root.
fn call_folder(inner: &Inner, workspace: &Path) -> Result<PathBuf, AgentFailure> {
    let folder = workspace.canonicalize().map_err(|error| {
        AgentFailure::Setup(format!(
            "the call's folder {} cannot be used: {error}",
            workspace.display()
        ))
    })?;
    if !folder.is_dir() {
        return Err(AgentFailure::Setup(format!(
            "the call's folder {} is not a directory",
            workspace.display()
        )));
    }
    if !folder.starts_with(&inner.results_root) {
        return Err(AgentFailure::Setup(format!(
            "the call's folder {} is outside the results root {}",
            workspace.display(),
            inner.results_root.display()
        )));
    }
    Ok(folder)
}

/// The agent configuration for one call.
///
/// The call's own output limit, when its role sets one, replaces the runner's
/// `max_output_tokens` for this call only (see
/// [`super::RunnerConfig::call_output_limit`]).
fn agent_config(inner: &Inner, req: &AgentRequest, folder: PathBuf) -> AgentConfig {
    let mut config = AgentConfig::new(inner.config.model.clone(), folder)
        .with_command_globs(req.grants.commands.clone());
    if !req.system_prompt.is_empty() {
        config = config.with_system_prompt(req.system_prompt.clone());
    }
    let limit = inner.config.call_output_limit(req.limits.max_output_tokens);
    if let Some(tokens) = NonZeroU32::new(limit) {
        config = config.with_max_output_tokens(tokens);
    }
    config
}

/// The permission posture of one call: unattended, so anything not granted is
/// refused rather than asked about.
fn policy(req: &AgentRequest) -> PolicySnapshot {
    let mut policy = PolicySnapshot::defaults();
    policy.permission = PermissionMode::Unattended(UnattendedGrants {
        edit_file: req.grants.edit,
        run_command_patterns: req.grants.commands.clone(),
    })
    .permission_policy();
    policy
}

/// Drive the turn until it ends, the time limit runs out, or the campaign asks
/// every call to stop.
///
/// When the turn is asked to stop, it is given `grace` to end on its own and
/// is then dropped. Returns `None` for the outcome when it was dropped.
async fn drive(
    inner: &Inner,
    id: &SessionId,
    timeout: Duration,
) -> (Option<TurnOutcome>, Option<Why>) {
    let mut turn = pin!(run::drive_turn(&inner.sessions, id));
    let mut deadline = pin!(tokio::time::sleep(timeout));
    let stop = async {
        match &inner.stop {
            Some(flag) => flag.requested().await,
            None => std::future::pending().await,
        }
    };
    let mut stop = pin!(stop);
    let why = tokio::select! {
        outcome = &mut turn => return (Some(outcome), None),
        () = &mut deadline => Why::Timeout,
        () = &mut stop => Why::Stop,
    };

    let mut grace = pin!(tokio::time::sleep(inner.config.interrupt_grace));
    let mut retry = tokio::time::interval(INTERRUPT_RETRY);
    loop {
        tokio::select! {
            biased;
            outcome = &mut turn => return (Some(outcome), Some(why)),
            () = &mut grace => {
                tracing::warn!("the session did not stop when interrupted and was dropped");
                return (None, Some(why));
            }
            _ = retry.tick() => {
                run::request_interrupt(&inner.sessions, id);
            }
        }
    }
}

/// Run one role call.
///
/// Failures are values: nothing here panics for a bad request, a model error
/// or a spend limit, and every ending that spent money says what it spent.
pub(super) async fn run_call(
    inner: &Inner,
    req: AgentRequest,
) -> Result<AgentReport, AgentFailure> {
    if inner.closed.load(Ordering::Acquire) {
        return Err(AgentFailure::Setup(
            "the runner has been shut down".to_string(),
        ));
    }
    if req.limits.max_turns == 0 {
        return Err(AgentFailure::Setup(
            "a call must be allowed at least one turn".to_string(),
        ));
    }
    if req.limits.timeout.is_zero() {
        return Err(AgentFailure::Setup(
            "a call must be given a time limit".to_string(),
        ));
    }
    if inner.stop.as_ref().is_some_and(|flag| flag.is_requested()) {
        return Ok(unspent(inner, StopCause::Interrupted));
    }
    if req.limits.max_micros == 0 {
        return Ok(unspent(inner, StopCause::CostCap));
    }
    let folder = call_folder(inner, &req.workspace)?;
    let root = inner
        .config
        .venv
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| AgentFailure::Setup("missing campaign root".into()))?;
    let tests = inner.tests.enable(
        &req,
        &folder,
        root,
        inner.config.author_test_config.as_ref(),
    );

    // A model with no price cannot be metered or capped, so no request is made.
    let meter =
        TokenMeter::new().with_pricing_store(polaris::models::pricing::PriceStore::builtin());
    report::usage_of(&meter.snapshot(&inner.config.model), None)?;
    meter
        .set_task_cost_cap(report::usd_of(req.limits.max_micros))
        .map_err(|error| AgentFailure::Setup(format!("the spend limit is unusable: {error}")))?;

    // The task text is queued before the session starts reading it.
    let (io, input, output) = ExecIOProvider::new(256);
    let io = Arc::new(io);
    input
        .send(IOMessage::from_external(
            "polaris-scientist",
            IOContent::Text(req.turn_prompt.clone()),
        ))
        .await
        .map_err(|_| AgentFailure::Setup("the task text could not be queued".to_string()))?;
    drop(input);

    let transcript_path = folder.join(transcript::TRANSCRIPT_FILE);
    let file = transcript::create(&transcript_path)
        .await
        .map_err(|error| {
            AgentFailure::Setup(format!(
                "the transcript {} cannot be created: {error}",
                transcript_path.display()
            ))
        })?;
    let writer = transcript::spawn(file, output);

    let config = agent_config(inner, &req, folder);
    let user_io = UserIO::new(io.clone());
    let call_meter = meter.clone();
    // Held for the rest of the call: if this future is dropped at any point,
    // the guard removes the session.
    let guard = inner
        .sessions
        .scoped_session_with_executor(
            &AgentTypeId::from_name(CodeAgent::NAME),
            GraphExecutor::new().with_default_max_iterations(req.limits.max_turns as usize),
            move |ctx| {
                ctx.insert(config);
                ctx.insert(user_io);
                ctx.insert(call_meter);
            },
        )
        .map_err(|error| AgentFailure::Setup(format!("a session cannot be created: {error}")))?;
    let id = guard.id().clone();
    let mut posture = policy(&req);
    if tests.is_none() {
        posture
            .tool_palette
            .denied
            .push(super::testing::TOOL.into());
    }
    inner
        .sessions
        .with_context(&id, move |ctx| {
            ctx.insert(posture);
        })
        .await
        .map_err(|error| {
            AgentFailure::Setup(format!("the session's permissions cannot be set: {error}"))
        })?;

    let (outcome, why) = drive(inner, &id, req.limits.timeout).await;

    io.close().await;
    if tokio::time::timeout(TRANSCRIPT_FLUSH, writer)
        .await
        .is_err()
    {
        tracing::warn!("the transcript writer did not finish");
    }
    if let Err(error) = guard.delete().await {
        tracing::warn!("a finished session could not be deleted: {error}");
    }

    let ended = outcome.as_ref().map_or(Ended::Abandoned, Ended::Turn);
    let stop = report::stop_cause(ended, why, meter.cost_cap_stopped(), req.limits.max_turns)?;
    let snapshot = meter.snapshot(&inner.config.model);
    report::require_usage(&stop, &snapshot)?;
    let usage = report::usage_of(&snapshot, meter.task_cost_spent_usd())?;
    tracing::info!(
        step = %req.step,
        role = %req.role,
        stop = ?stop,
        micros = usage.micros,
        requests = usage.requests,
        "role call ended"
    );
    Ok(AgentReport {
        stop,
        usage,
        final_text: outcome
            .and_then(|outcome| outcome.final_text)
            .unwrap_or_default(),
        transcript: Some(transcript_path),
    })
}
