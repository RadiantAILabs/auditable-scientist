//! Inherited Linux restrictions for advisory candidate execution.
use std::{ffi::CString, io, os::unix::ffi::OsStrExt, path::Path};
use tokio::process::Command;

#[repr(C)]
struct Ruleset {
    access: u64,
}
#[repr(C, packed)]
struct PathRule {
    access: u64,
    parent: i32,
}

// Landlock ABI 3 covers writes, creation, removal, renames and truncation.
// Reads and execution use the existing runtime, without write permission.
const WRITES: u64 = (1 << 1) | (0x7ff << 4);

/// Configure only the child; the model client retains its network access.
pub(super) fn restrict(command: &mut Command, scratch: &Path) -> io::Result<()> {
    let scratch = CString::new(scratch.as_os_str().as_bytes())?;
    // Verify the syscall architecture before inspecting syscall numbers.
    // Refusing every socket also prevents metadata-service connections.
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc00000b7;
    let mut filter = vec![
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 4,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 1,
            jf: 0,
            k: ARCH,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x80000000,
        },
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        },
    ];
    filter.push(libc::sock_filter {
        code: 0x35,
        jt: 0,
        jf: 1,
        k: 0x40000000,
    });
    filter.push(libc::sock_filter {
        code: 0x06,
        jt: 0,
        jf: 0,
        k: 0x80000000,
    });
    for syscall in [
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_chmod,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_fchmodat2,
        libc::SYS_ptrace,
        libc::SYS_process_vm_writev,
        libc::SYS_io_uring_setup,
    ] {
        filter.push(libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: syscall as u32,
        });
        filter.push(libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x00050000 | libc::EPERM as u32,
        });
    }
    filter.push(libc::sock_filter {
        code: 0x06,
        jt: 0,
        jf: 0,
        k: 0x7fff0000,
    });
    // SAFETY: the closure uses only syscalls and stack data between fork and exec.
    unsafe {
        command.pre_exec(move || isolate(&scratch, &mut filter));
    }
    Ok(())
}

fn checked(result: libc::c_long) -> io::Result<libc::c_long> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

fn isolate(scratch: &CString, filter: &mut [libc::sock_filter]) -> io::Result<()> {
    // SAFETY: all syscall structures have the Linux UAPI layouts; descriptors
    // belong to this child and all pointed-to values live across their syscall.
    unsafe {
        let abi = checked(libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<Ruleset>(),
            0,
            1,
        ))?;
        if abi < 3 {
            return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
        }
        checked(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) as _)?;
        let rules = checked(libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &Ruleset { access: WRITES },
            size_of::<Ruleset>(),
            0,
        ))? as i32;
        // Python multiprocessing uses POSIX semaphores in /dev/shm even when
        // TMPDIR is set. These runtime devices are separate from task inputs.
        let mut added = 0;
        for (path, access) in [
            (scratch.as_ptr(), WRITES),
            (c"/dev/shm".as_ptr(), WRITES),
            (c"/dev/null".as_ptr(), 1 << 1),
        ] {
            let parent = libc::open(path, libc::O_PATH | libc::O_CLOEXEC);
            if parent < 0 {
                added = -1;
                break;
            }
            added = libc::syscall(
                libc::SYS_landlock_add_rule,
                rules,
                1,
                &PathRule { access, parent },
                0,
            );
            libc::close(parent);
            if added < 0 {
                break;
            }
        }
        let applied = if added < 0 {
            added
        } else {
            libc::syscall(libc::SYS_landlock_restrict_self, rules, 0)
        };
        let result = checked(applied);
        libc::close(rules);
        result?;

        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_mut_ptr(),
        };
        checked(libc::prctl(libc::PR_SET_SECCOMP, 2, &program) as _)?;
    }
    Ok(())
}
