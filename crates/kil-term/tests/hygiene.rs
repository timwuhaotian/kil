//! What a child inherits (DESIGN.md §4.2, F5, F8, F15). Mutates process-wide state, so it runs in
//! its own test binary.
#![expect(
    unsafe_code,
    reason = "tests plant inheritable descriptors, ignored and blocked signals"
)]

mod common;

use std::{mem::MaybeUninit, os::unix::process::ExitStatusExt, ptr, thread, time::Duration};

use common::{Probe, sh, start, wait_for_foreground};
use kil_term::{SessionEvent, diagnostics};

/// A descriptor number no shell uses for itself: a child opens its own (its tty, for one)
/// low, so a planted descriptor high is told apart from them.
const PLANTED_FD: i32 = 60;

#[test]
fn spawn_should_give_child_only_stdio_when_parent_has_inheritable_fd() {
    // SAFETY: open(2) of a constant path; closed below.
    let planted = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
    assert!(planted >= 3);
    // SAFETY: F_GETFD on any number; it fails when the number is free.
    let free = unsafe { libc::fcntl(PLANTED_FD, libc::F_GETFD) } == -1;
    assert!(free, "descriptor {PLANTED_FD} is in use");
    // SAFETY: dup2 to a free number; the descriptor it replaces is closed.
    let planted = unsafe { libc::dup2(planted, PLANTED_FD) };
    assert_eq!(planted, PLANTED_FD);
    let probe = start(&sh("exec /bin/sleep 600"));
    assert!(wait_for_foreground(&probe.session, "sleep"));
    let fds = diagnostics::open_fds_of(probe.session.pid()).unwrap();
    // SAFETY: closes the descriptor opened above.
    unsafe { libc::close(PLANTED_FD) };
    // The child may have descriptors of its own; the planted one must not be among them.
    assert!(
        !fds.contains(&PLANTED_FD),
        "the child inherited the planted descriptor: {fds:?}"
    );
}

/// Waits for the child's exit and returns the signal that ended it.
fn exit_signal(probe: &Probe) -> Option<i32> {
    std::iter::from_fn(|| probe.events.recv_timeout(Duration::from_secs(2)).ok()).find_map(
        |event| match event {
            SessionEvent::ChildExited(status) => status.signal(),
            _ => None,
        },
    )
}

/// Sends `signal` to the session's (unreaped, so still ours) child.
fn signal_child(probe: &Probe, signal: i32) {
    // SAFETY: kill(2) has no memory-safety preconditions; the PID is our unreaped child.
    unsafe { libc::kill(probe.session.pid().get(), signal) };
}

// `ps -o sigignore` does not exist on macOS, so these check behavior: the signal must end
// the child.

#[test]
fn spawn_should_reset_signals_ignored_by_parent() {
    // SAFETY: changes this test process's SIGTERM disposition; nothing here relies on it.
    unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
    let probe = start(&sh("exec /bin/sleep 600"));
    assert!(wait_for_foreground(&probe.session, "sleep"));
    signal_child(&probe, libc::SIGTERM);
    assert_eq!(exit_signal(&probe), Some(libc::SIGTERM));
}

#[test]
fn spawn_should_unblock_signals_blocked_by_the_spawning_thread() {
    // The app spawns from GPUI's executor threads (GCD workers), which block most signals,
    // and a forked child inherits its thread's mask.
    let probe = thread::spawn(|| {
        let mut set = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: initializes `set`, then blocks SIGHUP on this short-lived thread only.
        unsafe {
            libc::sigemptyset(set.as_mut_ptr());
            libc::sigaddset(set.as_mut_ptr(), libc::SIGHUP);
            libc::pthread_sigmask(libc::SIG_BLOCK, set.as_ptr(), ptr::null_mut());
        }
        start(&sh("exec /bin/sleep 600"))
    })
    .join()
    .unwrap();
    assert!(wait_for_foreground(&probe.session, "sleep"));
    signal_child(&probe, libc::SIGHUP);
    assert_eq!(exit_signal(&probe), Some(libc::SIGHUP));
}
