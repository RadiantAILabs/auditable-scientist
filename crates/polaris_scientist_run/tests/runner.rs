//! The session runner against a scripted model: isolation between calls, how
//! each call ends, what it is charged, and that nothing is left open.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use polaris::models::llm::Usage;
use polaris_cosmos::flow::{AgentFailure, AgentReport, StopCause, StopFlag, TaskRunner as _};
use polaris_scientist_run::runner::{MAX_OUTPUT_TOKENS, RunnerConfig, TRANSCRIPT_FILE};
use serde_json::json;
use tokio::sync::Notify;

use support::{
    Fixture, LogCapture, Reply, billed, fixture, fixture_stoppable, fixture_with, request, text,
    text_billed, tool,
};

/// Nothing is open and nothing was billed to the server's own meter.
fn assert_clean(fx: &Fixture) {
    assert_eq!(fx.runner.live_sessions(), 0, "no session is left open");
    assert_eq!(
        fx.runner.server_meter().total_tokens(),
        0,
        "the server's own meter stays at zero"
    );
}

fn finished(result: Result<AgentReport, AgentFailure>) -> AgentReport {
    let report = result.expect("the call returned a report");
    assert_eq!(report.stop, StopCause::Finished, "{report:?}");
    report
}

// ─────────────────────────────────────────────────────────────────────────────
// Isolation
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_in_sequence_keep_prompt_folder_and_transcript_apart() {
    let fx = fixture().await;
    let alpha = fx.folder("alpha-call");
    let beta = fx.folder("beta-call");
    fx.provider.script(
        &Fixture::key(&alpha),
        vec![
            tool(
                "create_file",
                json!({ "path": "made.txt", "content": "from alpha\n" }),
            ),
            text("alpha done"),
        ],
    );
    fx.provider.script(
        &Fixture::key(&beta),
        vec![
            tool("list_files", json!({ "path": "." })),
            text("beta done"),
        ],
    );
    let mut first = request("alpha", &alpha);
    first.grants.edit = true;
    let second = request("beta", &beta);

    let first = finished(fx.runner.run(first).await);
    let second = finished(fx.runner.run(second).await);

    assert_eq!(first.final_text, "alpha done");
    assert_eq!(second.final_text, "beta done");
    for (name, other, folder) in [("alpha", "beta", &alpha), ("beta", "alpha", &beta)] {
        let seen = fx.provider.requests_for(&Fixture::key(folder));
        assert_eq!(seen.len(), 2);
        for each in &seen {
            assert!(each.system.contains(&format!("You are the {name} role.")));
            assert!(!each.system.contains(&format!("You are the {other} role.")));
            assert!(each.messages.contains(&format!("Do the {name} task.")));
            assert!(!each.messages.contains(&format!("Do the {other} task.")));
        }
    }
    assert_eq!(
        std::fs::read_to_string(alpha.join("made.txt")).unwrap(),
        "from alpha\n"
    );
    assert!(!beta.join("made.txt").exists());
    let beta_last = fx
        .provider
        .requests_for(&Fixture::key(&beta))
        .pop()
        .unwrap();
    assert!(!beta_last.messages.contains("made.txt"));

    assert_eq!(
        first.transcript.as_deref(),
        Some(&*alpha.join(TRANSCRIPT_FILE))
    );
    assert_eq!(
        second.transcript.as_deref(),
        Some(&*beta.join(TRANSCRIPT_FILE))
    );
    let alpha_log = std::fs::read_to_string(alpha.join(TRANSCRIPT_FILE)).unwrap();
    let beta_log = std::fs::read_to_string(beta.join(TRANSCRIPT_FILE)).unwrap();
    assert!(alpha_log.contains("alpha done") && !alpha_log.contains("beta done"));
    assert!(beta_log.contains("beta done") && !beta_log.contains("alpha done"));
    for line in alpha_log.lines().chain(beta_log.lines()) {
        serde_json::from_str::<serde_json::Value>(line).expect("one JSON value per line");
    }
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_in_parallel_overlap_and_keep_to_their_own_scripts() {
    let fx = fixture().await;
    let delay = Duration::from_millis(700);
    let slow = |text: &'static str| Reply::Slow {
        text,
        delay,
        usage: billed(100, 10),
    };
    let alpha = fx.folder("alpha-parallel");
    let beta = fx.folder("beta-parallel");
    fx.provider
        .script(&Fixture::key(&alpha), vec![slow("alpha done")]);
    fx.provider
        .script(&Fixture::key(&beta), vec![slow("beta done")]);

    let started = Instant::now();
    let (first, second) = tokio::join!(
        fx.runner.run(request("alpha", &alpha)),
        fx.runner.run(request("beta", &beta))
    );
    let elapsed = started.elapsed();

    assert_eq!(finished(first).final_text, "alpha done");
    assert_eq!(finished(second).final_text, "beta done");
    assert!(
        elapsed < delay * 2 - Duration::from_millis(100),
        "two {delay:?} replies took {elapsed:?}, so the calls ran one after the other"
    );
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_failure_in_one_call_does_not_end_its_sibling() {
    let fx = fixture().await;
    let failing = fx.folder("failing-call");
    let sibling = fx.folder("sibling-call");
    fx.provider
        .script(&Fixture::key(&failing), vec![Reply::Refuse]);
    fx.provider.script(
        &Fixture::key(&sibling),
        vec![Reply::Slow {
            text: "sibling done",
            delay: Duration::from_millis(300),
            usage: billed(100, 10),
        }],
    );

    let (bad, good) = tokio::join!(
        fx.runner.run(request("failing", &failing)),
        fx.runner.run(request("sibling", &sibling))
    );

    let bad = bad.expect("a model failure is a report");
    assert!(
        matches!(&bad.stop, StopCause::ModelError(m) if m.starts_with("auth")),
        "{bad:?}"
    );
    assert_eq!(finished(good).final_text, "sibling done");
    assert_clean(&fx);
}

// ─────────────────────────────────────────────────────────────────────────────
// Endings and money
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_call_reports_what_it_spent() {
    let fx = fixture().await;
    let folder = fx.folder("finished");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![tool("list_files", json!({ "path": "." })), text("all done")],
    );

    let report = finished(fx.runner.run(request("finished", &folder)).await);

    assert_eq!(report.final_text, "all done");
    assert_eq!(report.usage.model_id, support::MODEL_ID);
    assert_eq!(report.usage.requests, 2);
    assert_eq!(report.usage.input, 200);
    assert_eq!(report.usage.output, 20);
    assert_eq!(report.usage.cache_read, 0);
    assert_eq!(report.usage.cache_write, 0);
    // $2 per million input tokens and $10 per million output tokens.
    assert_eq!(report.usage.micros, 200 * 2 + 20 * 10);
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_calls_own_output_limit_replaces_the_runners_for_that_call_only() {
    let fx = fixture().await;
    let long = fx.folder("long-writer");
    let plain = fx.folder("plain-reader");
    for folder in [&long, &plain] {
        fx.provider
            .script(&Fixture::key(folder), vec![text("done")]);
    }
    let mut long_request = request("long-writer", &long);
    long_request.limits.max_output_tokens = Some(32_000);

    finished(fx.runner.run(long_request).await);
    finished(fx.runner.run(request("plain-reader", &plain)).await);

    let limit = |folder: &std::path::Path| {
        fx.provider
            .requests_for(&Fixture::key(folder))
            .iter()
            .map(|seen| seen.max_output_tokens)
            .collect::<Vec<_>>()
    };
    assert_eq!(limit(&long), vec![Some(32_000)], "the role's own limit");
    assert_eq!(
        limit(&plain),
        vec![Some(4096)],
        "the runner's setting when the role sets none"
    );
    assert_clean(&fx);
}

#[test]
fn the_provider_ceiling_is_never_below_a_limit_a_call_may_send() {
    // A request can lower the provider's output ceiling but never raise it,
    // so a ceiling at the runner's own setting would cut a role's larger
    // limit back to that setting while the cost guard still reserved the
    // larger one.
    let results = tempfile::tempdir().unwrap();
    let mut config = RunnerConfig::new(results.path(), results.path());
    assert_eq!(config.max_output_tokens, 16_384);
    assert_eq!(config.provider_output_ceiling(), MAX_OUTPUT_TOKENS);
    assert_eq!(config.call_output_limit(None), 16_384);
    assert_eq!(config.call_output_limit(Some(32_000)), 32_000);
    assert_eq!(
        config.call_output_limit(Some(0)),
        16_384,
        "zero counts as not set"
    );
    assert_eq!(
        config.call_output_limit(Some(MAX_OUTPUT_TOKENS + 1)),
        MAX_OUTPUT_TOKENS,
        "no call sends more than the ceiling"
    );
    config.max_output_tokens = MAX_OUTPUT_TOKENS + 1000;
    assert_eq!(config.provider_output_ceiling(), MAX_OUTPUT_TOKENS + 1000);
    assert_eq!(config.call_output_limit(None), MAX_OUTPUT_TOKENS + 1000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spend_is_rounded_up_to_a_whole_micro_dollar() {
    let fx = fixture().await;
    // One input token and one cache-read token cost 2.2 micro-dollars.
    let cached = fx.folder("rounded-cached");
    fx.provider.script(
        &Fixture::key(&cached),
        vec![text_billed(
            "ok",
            // Both token counts are reported: usage that lacks either one is
            // charged at the cost estimate instead of being priced.
            Usage {
                input_tokens: Some(1),
                output_tokens: Some(0),
                cache_read_tokens: Some(1),
                total_tokens: Some(2),
                ..Default::default()
            },
        )],
    );
    // One input token and one output token cost exactly 12.
    let exact = fx.folder("rounded-exact");
    fx.provider
        .script(&Fixture::key(&exact), vec![text_billed("ok", billed(1, 1))]);

    let cached = finished(fx.runner.run(request("cached", &cached)).await);
    let exact = finished(fx.runner.run(request("exact", &exact)).await);

    assert_eq!(cached.usage.cache_read, 1);
    assert_eq!(cached.usage.micros, 3);
    assert_eq!(exact.usage.micros, 12);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limit_below_any_request_stops_the_call_before_it_asks() {
    let fx = fixture().await;
    let folder = fx.folder("cap-too-small");
    fx.provider
        .script(&Fixture::key(&folder), vec![text("never sent")]);
    let mut call = request("capped", &folder);
    call.limits.max_micros = 1;

    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::CostCap, "{report:?}");
    assert_eq!(fx.provider.request_count(), 0);
    assert_eq!(report.usage.micros, 0);
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limit_that_admits_one_request_but_not_the_next_ends_as_cost_cap() {
    // Each request reserves its worst case: the prompt plus a full output
    // allowance of 1000 tokens (10 000 micro-dollars). The first reply bills
    // 30 000, which leaves too little room for the next reservation.
    let fx = fixture_with(|config| config.max_output_tokens = 1000).await;
    let folder = fx.folder("cap-second");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![
            Reply::Tool {
                name: "list_files",
                arguments: json!({ "path": "." }),
                usage: billed(10_000, 1_000),
            },
            text("never reached"),
        ],
    );
    let mut call = request("capped", &folder);
    call.limits.max_micros = 40_000;

    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::CostCap, "{report:?}");
    assert_eq!(fx.provider.request_count(), 1);
    assert_eq!(report.usage.micros, 10_000 * 2 + 1_000 * 10);
    assert!(report.usage.micros <= 40_000);
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_out_of_turns_ends_as_turn_cap() {
    let fx = fixture().await;
    let folder = fx.folder("turn-cap");
    fx.provider.script(
        &Fixture::key(&folder),
        (0..6)
            .map(|_| tool("list_files", json!({ "path": "." })))
            .collect(),
    );
    let mut call = request("looping", &folder);
    call.limits.max_turns = 2;

    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::TurnCap, "{report:?}");
    assert_eq!(fx.provider.request_count(), 2);
    assert_eq!(report.usage.requests, 2);
    assert_eq!(report.usage.micros, 2 * (100 * 2 + 10 * 10));
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_credential_ends_as_model_error() {
    let fx = fixture().await;
    let folder = fx.folder("auth");
    fx.provider
        .script(&Fixture::key(&folder), vec![Reply::Refuse]);

    let report = fx
        .runner
        .run(request("auth", &folder))
        .await
        .expect("a report");

    match &report.stop {
        StopCause::ModelError(message) => {
            assert!(message.starts_with("auth: "), "{message}");
        }
        other => panic!("expected a model error, got {other:?}"),
    }
    assert_eq!(report.usage.micros, 0);
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_with_no_price_is_unmeasured_and_never_asked() {
    let fx = fixture_with(|config| config.model = "anthropic/mystery-model".to_string()).await;
    let folder = fx.folder("unpriced");
    fx.provider
        .script(&Fixture::key(&folder), vec![text("never sent")]);

    let result = fx.runner.run(request("unpriced", &folder)).await;

    match result {
        Err(AgentFailure::Unmeasured(message)) => {
            assert!(message.contains("mystery-model"), "{message}");
        }
        other => panic!("expected an unmeasured failure, got {other:?}"),
    }
    assert_eq!(fx.provider.request_count(), 0);
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_call_with_no_reported_usage_is_unmeasured() {
    let fx = fixture().await;
    let folder = fx.folder("no-usage");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![text_billed("done", Usage::default())],
    );

    let result = fx.runner.run(request("no-usage", &folder)).await;

    assert!(
        matches!(&result, Err(AgentFailure::Unmeasured(message)) if message.contains("no token usage")),
        "{result:?}"
    );
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_that_cannot_be_set_up_is_a_setup_failure_and_asks_nothing() {
    let fx = fixture().await;
    let folder = fx.folder("setup");
    fx.provider
        .script(&Fixture::key(&folder), vec![text("never sent")]);
    let outside = tempfile::tempdir().unwrap();

    let mut missing = request("missing", &folder);
    missing.workspace = folder.join("not-there");
    let mut elsewhere = request("elsewhere", &folder);
    elsewhere.workspace = outside.path().to_path_buf();
    let mut file = request("file", &folder);
    std::fs::write(folder.join("plain.txt"), "x").unwrap();
    file.workspace = folder.join("plain.txt");
    let mut no_turns = request("no-turns", &folder);
    no_turns.limits.max_turns = 0;
    let mut no_time = request("no-time", &folder);
    no_time.limits.timeout = Duration::ZERO;

    for call in [missing, elsewhere, file, no_turns, no_time] {
        let role = call.role.clone();
        let result = fx.runner.run(call).await;
        assert!(
            matches!(result, Err(AgentFailure::Setup(_))),
            "{role}: {result:?}"
        );
    }
    assert_eq!(fx.provider.request_count(), 0);
    assert!(!folder.join(TRANSCRIPT_FILE).exists());
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_after_shutdown_is_a_setup_failure() {
    let fx = fixture().await;
    let folder = fx.folder("after-shutdown");
    fx.runner.shutdown().await;

    let result = fx.runner.run(request("late", &folder)).await;

    assert!(
        matches!(&result, Err(AgentFailure::Setup(message)) if message.contains("shut down")),
        "{result:?}"
    );
    assert_eq!(fx.provider.request_count(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Time and stop
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_that_never_ends_times_out_and_the_session_is_gone() {
    let fx = fixture().await;
    let folder = fx.folder("timeout");
    let entered = Arc::new(Notify::new());
    fx.provider.script(
        &Fixture::key(&folder),
        vec![Reply::Stall {
            entered: entered.clone(),
        }],
    );
    let mut call = request("timeout", &folder);
    call.limits.timeout = Duration::from_millis(300);

    let started = Instant::now();
    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::Timeout, "{report:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(fx.provider.request_count(), 1);
    assert!(
        report.usage.micros > 0,
        "the request in flight is charged at its estimate"
    );
    assert!(report.usage.micros <= 5_000_000);
    assert_clean(&fx);
}

#[tokio::test(flavor = "current_thread")]
async fn a_turn_that_ignores_the_interrupt_is_dropped_after_the_grace() {
    let logs = LogCapture::default();
    let _installed = logs.install();
    let fx = fixture_with(|config| config.interrupt_grace = Duration::ZERO).await;
    let folder = fx.folder("dropped");
    fx.provider.script(
        &Fixture::key(&folder),
        vec![Reply::Stall {
            entered: Arc::new(Notify::new()),
        }],
    );
    let mut call = request("dropped", &folder);
    call.limits.timeout = Duration::from_millis(200);

    let report = fx.runner.run(call).await.expect("a report");

    assert_eq!(report.stop, StopCause::Timeout, "{report:?}");
    assert!(
        logs.text().contains("did not stop when interrupted"),
        "the drop is logged"
    );
    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_call_leaves_no_session_behind() {
    let fx = fixture().await;
    let folder = fx.folder("future-dropped");
    let entered = Arc::new(Notify::new());
    fx.provider.script(
        &Fixture::key(&folder),
        vec![Reply::Stall {
            entered: entered.clone(),
        }],
    );
    let runner = fx.runner.clone();
    let call = request("future-dropped", &folder);
    let task = tokio::spawn(async move { runner.run(call).await });
    tokio::time::timeout(Duration::from_secs(20), entered.notified())
        .await
        .expect("the request reached the model");
    assert_eq!(fx.runner.live_sessions(), 1);

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    assert_clean(&fx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_stop_flag_interrupts_calls_in_flight_and_later_calls_start_nothing() {
    let flag = StopFlag::default();
    let fx = fixture_stoppable(&flag).await;
    let running = fx.folder("stop-running");
    let later = fx.folder("stop-later");
    let entered = Arc::new(Notify::new());
    fx.provider.script(
        &Fixture::key(&running),
        vec![Reply::Stall {
            entered: entered.clone(),
        }],
    );
    fx.provider
        .script(&Fixture::key(&later), vec![text("never sent")]);
    let runner = fx.runner.clone();
    let call = request("running", &running);
    let task = tokio::spawn(async move { runner.run(call).await });
    tokio::time::timeout(Duration::from_secs(20), entered.notified())
        .await
        .expect("the request reached the model");

    flag.request();
    let started = Instant::now();
    let in_flight = task.await.unwrap().expect("a report");
    assert_eq!(in_flight.stop, StopCause::Interrupted, "{in_flight:?}");
    assert!(
        in_flight.usage.micros > 0,
        "the request cut off in flight is charged at its estimate"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(fx.provider.request_count(), 1);

    let after = fx
        .runner
        .run(request("later", &later))
        .await
        .expect("a report");
    assert_eq!(after.stop, StopCause::Interrupted, "{after:?}");
    assert_eq!(after.usage.micros, 0);
    assert_eq!(after.usage.model_id, support::MODEL_ID);
    assert_eq!(
        fx.provider.request_count(),
        1,
        "no request for a later call"
    );
    assert!(
        !later.join(TRANSCRIPT_FILE).exists(),
        "no session was created for it"
    );
    assert_clean(&fx);
}
