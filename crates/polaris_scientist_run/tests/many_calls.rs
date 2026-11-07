//! Many calls through one runner do not make it grow.
//!
//! A campaign makes hundreds of role calls through one runner. After a warm-up
//! the runner's memory, open files and threads must stay level.

mod support;

use polaris_cosmos::flow::{StopCause, TaskRunner as _};
use serde_json::json;

use support::{Fixture, fixture, request, text, tool};

/// Resident memory in KiB, from the kernel's view of this process.
fn resident_kib() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    // SAFETY: `sysconf` has no preconditions.
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
    pages * page / 1024
}

fn open_files() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

fn threads() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

const WARM_UP: usize = 50;
const CALLS: usize = 300;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hundreds_of_calls_through_one_runner_leave_memory_files_and_threads_level() {
    let fx = fixture().await;
    let mut after_warm_up = None;
    for number in 0..CALLS {
        let folder = fx.folder(&format!("call-{number:04}"));
        fx.provider.script(
            &Fixture::key(&folder),
            vec![
                tool("list_files", json!({ "path": "." })),
                tool("list_files", json!({ "path": "." })),
                text("done"),
            ],
        );
        let report = fx
            .runner
            .run(request("many", &folder))
            .await
            .expect("a report");
        assert_eq!(report.stop, StopCause::Finished);
        assert_eq!(report.usage.requests, 3);
        // The test's own record of requests is not the runner's growth.
        fx.provider.forget();
        if number + 1 == WARM_UP {
            after_warm_up = Some((resident_kib(), open_files(), threads()));
        }
    }
    let (memory, files, thread_count) = after_warm_up.expect("the warm-up ended");
    let (memory_now, files_now, threads_now) = (resident_kib(), open_files(), threads());
    println!(
        "after {WARM_UP} calls: {memory} KiB, {files} files, {thread_count} threads; \
         after {CALLS} calls: {memory_now} KiB, {files_now} files, {threads_now} threads"
    );

    assert_eq!(fx.runner.live_sessions(), 0);
    assert_eq!(fx.runner.server_meter().total_tokens(), 0);
    assert!(
        files_now <= files + 4,
        "open files grew: {files} -> {files_now}"
    );
    assert!(
        threads_now <= thread_count + 4,
        "threads grew: {thread_count} -> {threads_now}"
    );
    // The agent's record of its own decisions stays in memory for the life of
    // the process and adds some tens of KiB per call (see the README). A
    // session's whole state kept per call would add far more than the room
    // left here.
    assert!(
        memory_now <= memory + 32 * 1024,
        "resident memory grew from {memory} KiB to {memory_now} KiB"
    );
}
