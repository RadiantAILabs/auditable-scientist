//! Once the process is protected, a command it starts as the same user cannot
//! read the credentials the process holds.
//!
//! This is the only test in its binary, because protecting the process changes
//! it for good.

use std::process::Command;

use polaris_scientist_run::hardening::{is_dumpable, make_undumpable};

/// Read `/proc/<pid>/environ` of this process from a child, the way a command
/// run by the agent would. `Ok` holds what the child read.
fn read_environment_from_a_child() -> Result<Vec<u8>, String> {
    let output = Command::new("cat")
        .arg(format!("/proc/{}/environ", std::process::id()))
        .output()
        .expect("cat runs");
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn running_as_root() -> bool {
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[test]
fn a_protected_process_keeps_its_environment_from_its_commands() {
    // The reading works before the protection, so a failure afterwards is the
    // protection and not a missing `cat` or a locked-down `/proc`.
    let can_check_reading = !running_as_root();
    if can_check_reading {
        let before = read_environment_from_a_child().expect("the child reads it first");
        assert!(!before.is_empty(), "the environment is not empty");
        assert!(is_dumpable(), "a new process is dumpable");
    }

    make_undumpable().expect("the setting is accepted");
    assert!(!is_dumpable());

    if can_check_reading {
        let refused = read_environment_from_a_child().expect_err("the child is refused");
        assert!(
            refused.contains("Permission denied"),
            "the read is refused for permission: {refused}"
        );
    }

    // Asking again is harmless.
    make_undumpable().expect("the setting can be repeated");
    assert!(!is_dumpable());
}
