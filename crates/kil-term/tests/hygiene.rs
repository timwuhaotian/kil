//! What a child inherits (DESIGN.md §4.2, F5, F8). Mutates process-wide state, so it runs in
//! its own test binary.
#![expect(
    unsafe_code,
    reason = "tests plant inheritable descriptors and ignored signals"
)]

mod common;

use common::{sh, start, wait_for_foreground};
use kil_term::diagnostics;

#[test]
fn spawn_should_give_child_only_stdio_when_parent_has_inheritable_fd() {
    // SAFETY: open(2) of a constant path; closed below.
    let planted = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
    assert!(planted >= 3);
    let probe = start(&sh("exec /bin/sleep 600"));
    assert!(wait_for_foreground(&probe.session, "sleep"));
    let fds = diagnostics::open_fds_of(probe.session.pid()).unwrap();
    // SAFETY: closes the descriptor opened above.
    unsafe { libc::close(planted) };
    assert_eq!(fds, [0, 1, 2]);
}

#[test]
fn spawn_should_reset_signals_ignored_by_parent() {
    // SAFETY: changes this test process's SIGTERM disposition; nothing here relies on it.
    unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
    let probe = start(&sh("exec /bin/sleep 600"));
    assert!(wait_for_foreground(&probe.session, "sleep"));
    let output = std::process::Command::new("/bin/ps")
        .args([
            "-o",
            "sigignore=,sigmask=",
            "-p",
            &probe.session.pid().get().to_string(),
        ])
        .output()
        .unwrap();
    let masks = String::from_utf8(output.stdout).unwrap();
    let all_clear = masks
        .split_whitespace()
        .all(|mask| u64::from_str_radix(mask, 16) == Ok(0));
    assert!(all_clear, "child sigignore/sigmask: {masks:?}");
}
