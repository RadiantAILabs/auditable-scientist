//! The time and the output a command run by the model may take.
//!
//! The runner gives the shell a default time limit and an output limit. These
//! tests use small limits so they stay fast, and show what the model sees when
//! a command goes past them.

mod support;

use std::time::{Duration, Instant};

use polaris_cosmos::flow::{StopCause, TaskRunner as _};
use serde_json::json;

use support::{Fixture, fixture_with, request, text, tool};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_past_the_time_limit_is_stopped_and_the_call_goes_on() {
    let fx = fixture_with(|config| config.shell_timeout = Duration::from_secs(1)).await;
    let folder = fx.folder("slow-command");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![
            tool("run_command", json!({ "command": "sleep 30" })),
            text("gave up on it"),
        ],
    );
    let mut call = request("slow-command", &folder);
    call.grants.commands = vec!["sleep *".to_string()];

    let started = Instant::now();
    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::Finished, "{report:?}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the command was stopped at the limit, not left to run for 30 seconds"
    );
    let last = fx
        .provider
        .requests_for(&Fixture::key(&folder))
        .pop()
        .unwrap()
        .messages;
    assert!(
        last.contains("timed out after") && last.contains("was killed"),
        "the model is told the command timed out and was killed: {last}"
    );
    assert!(
        last.contains("timeout_secs"),
        "the model is told it can ask for more time: {last}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_model_can_ask_for_more_time_for_one_command() {
    // The limit is a default. The shell tool takes a `timeout_secs` argument
    // and honours it, so a command the model expects to be slow can ask for
    // more than the default. Only the call's own time limit bounds it.
    let fx = fixture_with(|config| config.shell_timeout = Duration::from_secs(1)).await;
    let folder = fx.folder("asks-for-time");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![
            tool(
                "run_command",
                json!({ "command": "sleep 3", "timeout_secs": 20 }),
            ),
            text("it finished"),
        ],
    );
    let mut call = request("asks-for-time", &folder);
    call.grants.commands = vec!["sleep *".to_string()];

    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::Finished, "{report:?}");
    let last = fx
        .provider
        .requests_for(&Fixture::key(&folder))
        .pop()
        .unwrap()
        .messages;
    assert!(
        last.contains("[no output; exit code: 0]"),
        "the command ran to its end: {last}"
    );
    assert!(!last.contains("timed out"), "{last}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_calls_own_time_limit_ends_a_command_that_asked_for_more() {
    // The runner's default grace for an interrupt is fifteen seconds. The call
    // ends well before that because the interrupt stops the running command.
    let fx = fixture_with(|config| config.shell_timeout = Duration::from_secs(1)).await;
    let folder = fx.folder("long-ask");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![tool(
            "run_command",
            json!({ "command": "sleep 60", "timeout_secs": 600 }),
        )],
    );
    let mut call = request("long-ask", &folder);
    call.grants.commands = vec!["sleep *".to_string()];
    call.limits.timeout = Duration::from_secs(2);

    let started = Instant::now();
    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::Timeout, "{report:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the command was stopped with the call: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn output_past_the_limit_is_cut_and_the_model_is_told() {
    let fx = fixture_with(|config| config.shell_max_output_bytes = 200).await;
    let folder = fx.folder("long-output");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![
            tool("run_command", json!({ "command": "seq 1 5000" })),
            text("saw the start"),
        ],
    );
    let mut call = request("long-output", &folder);
    call.grants.commands = vec!["seq *".to_string()];

    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::Finished, "{report:?}");
    let last = fx
        .provider
        .requests_for(&Fixture::key(&folder))
        .pop()
        .unwrap()
        .messages;
    assert!(
        last.contains("stdout truncated at the shell's output cap"),
        "the model is told its output was cut: {last}"
    );
    assert!(
        last.contains("1\\n2\\n3\\n"),
        "the start of the output is kept: {last}"
    );
    assert!(
        !last.contains("4999"),
        "the end of the output did not reach the model"
    );
}
