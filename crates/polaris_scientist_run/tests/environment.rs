//! A credential in the process environment never reaches a command the model
//! runs, and never appears in anything the runner returns, logs or writes.
//!
//! The environment is process-wide, so this is a test file of its own with one
//! test.

mod support;

use std::path::Path;

use polaris_cosmos::flow::{AgentFailure, StopCause, TaskRunner as _};
use serde_json::json;

use support::{Fixture, LogCapture, Reply, fixture, request, text, tool};

const SECRET: &str = "sentinel-secret-9f3a1c7e5b2d4086";
const KEY_ID: &str = "sentinel-key-id-41d8cd98f00b204e";
const TOKEN: &str = "sentinel-session-token-0e2b6a9c7d13f845";

/// Every file under `dir`, with its bytes.
fn files_under(dir: &Path, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            let bytes = std::fs::read(&path).unwrap();
            out.push((path, bytes));
        }
    }
}

fn holds_a_secret(haystack: &[u8]) -> bool {
    [SECRET, KEY_ID, TOKEN].iter().any(|secret| {
        haystack
            .windows(secret.len())
            .any(|w| w == secret.as_bytes())
    })
}

#[test]
fn a_credential_in_the_process_never_reaches_the_shell_or_any_output() {
    // SAFETY: nothing else runs yet; the runtime that reads the environment is
    // built after these are set.
    unsafe {
        std::env::set_var("AWS_SECRET_ACCESS_KEY", SECRET);
        std::env::set_var("AWS_ACCESS_KEY_ID", KEY_ID);
        std::env::set_var("AWS_SESSION_TOKEN", TOKEN);
    }
    let logs = LogCapture::install_global();

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let fx = fixture().await;
            let listing = fx.folder("lists-environment");
            let failing = fx.folder("fails");
            fx.provider.script(
                &Fixture::key(&listing),
                vec![
                    tool("run_command", json!({ "command": "env" })),
                    tool(
                        "run_command",
                        json!({ "command": "printenv AWS_SECRET_ACCESS_KEY" }),
                    ),
                    tool(
                        "run_command",
                        json!({ "command": "echo \"key=$AWS_SECRET_ACCESS_KEY\"" }),
                    ),
                    text("listed"),
                ],
            );
            fx.provider
                .script(&Fixture::key(&failing), vec![Reply::Refuse]);

            let mut listing_call = request("lists-environment", &listing);
            listing_call.grants.commands = vec![
                "env".to_string(),
                "printenv *".to_string(),
                "echo *".to_string(),
            ];
            let report = fx.runner.run(listing_call).await.expect("a report");
            assert_eq!(report.stop, StopCause::Finished, "{report:?}");

            let failed = fx
                .runner
                .run(request("fails", &failing))
                .await
                .expect("a report");
            let setup = fx
                .runner
                .run(request("missing", &listing.join("nowhere")))
                .await;
            assert!(matches!(setup, Err(AgentFailure::Setup(_))));

            // The shell saw its configured path and environment, and no
            // credential.
            let last = fx
                .provider
                .requests_for(&Fixture::key(&listing))
                .pop()
                .unwrap()
                .messages;
            let venv = fx.run_root.path().join("task").join(".venv");
            assert!(
                last.contains(&format!(
                    "PATH={}/bin:/usr/local/bin:/usr/bin:/bin",
                    venv.display()
                )),
                "the shell's PATH is the configured one: {last}"
            );
            assert!(
                last.contains(&format!("VIRTUAL_ENV={}", venv.display())),
                "the shell's VIRTUAL_ENV is the configured one: {last}"
            );
            // The model's own commands name the variable, so look for it as a
            // line of the listing: a line starts after an escaped newline.
            assert!(
                last.contains("\\nHOME=") && !last.contains("\\nAWS_"),
                "no AWS variable is listed: {last}"
            );
            assert!(last.contains("key="), "the echo ran: {last}");

            // Nothing the runner returned, logged or wrote holds a secret.
            let everything = format!("{report:?}\n{failed:?}\n{setup:?}");
            assert!(!holds_a_secret(everything.as_bytes()));
            for seen in fx.provider.requests() {
                assert!(!holds_a_secret(seen.system.as_bytes()));
                assert!(!holds_a_secret(seen.messages.as_bytes()));
            }
            assert!(!holds_a_secret(logs.text().as_bytes()), "no log line");
            let mut files = Vec::new();
            files_under(fx.results.path(), &mut files);
            files_under(fx.run_root.path(), &mut files);
            assert!(
                files
                    .iter()
                    .any(|(path, _)| path.ends_with("transcript.jsonl")),
                "the transcripts were written"
            );
            for (path, bytes) in &files {
                assert!(!holds_a_secret(bytes), "{} holds a secret", path.display());
            }
        });
}
