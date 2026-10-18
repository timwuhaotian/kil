//! Children that exit on their own, with nobody closing their session, are still reaped and
//! still report their status and last output. Covers the "session died during launch" path,
//! which never goes through close (DESIGN.md §4.6). Alone in its binary because it counts
//! process-wide children.

mod common;

use std::{
    sync::mpsc::Receiver,
    thread,
    time::{Duration, Instant},
};

use common::{program, screen, sh, start, wait_until};
use kil_term::{Session, SessionEvent, diagnostics};

/// Waits for [`SessionEvent::Ended`] and returns the exit code reported before it.
fn exit_code(events: &Receiver<SessionEvent>) -> Result<Option<i32>, &'static str> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut code = None;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left).map_err(|_| "no Ended")? {
            SessionEvent::ChildExited(status) => code = status.code(),
            SessionEvent::Ended => return Ok(code),
            _ => {}
        }
    }
    Err("no Ended")
}

#[test]
fn sessions_that_exit_at_startup_should_be_reaped_and_keep_status_and_output() {
    const SESSIONS: usize = 64;
    let probes: Vec<_> = thread::scope(|scope| {
        #[expect(
            clippy::needless_collect,
            reason = "start every session before joining any, so they spawn concurrently"
        )]
        let handles: Vec<_> = (0..SESSIONS)
            .map(|i| {
                scope.spawn(move || {
                    let config = if i % 2 == 0 {
                        program("/usr/bin/true", &[])
                    } else {
                        sh("printf last-words; exit 3")
                    };
                    (i, start(&config))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut wrong = Vec::new();
    for (i, probe) in &probes {
        let expected = if i % 2 == 0 { 0 } else { 3 };
        let got = exit_code(&probe.events);
        let output_kept = expected == 0 || screen(&probe.session).contains("last-words");
        if got != Ok(Some(expected)) || !output_kept {
            wrong.push((*i, got, output_kept));
        }
    }
    // Every `Session` is still alive here: reaping must not wait for the UI to close it.
    let zombies_gone = wait_until(Duration::from_secs(2), || {
        diagnostics::child_pids().is_empty()
    });
    let children = diagnostics::child_pids();
    let sessions: Vec<Session> = probes.into_iter().map(|(_, probe)| probe.session).collect();
    drop(sessions);
    assert!(
        wrong.is_empty(),
        "(session, exit code, output kept): {wrong:?}"
    );
    assert!(zombies_gone, "unreaped children: {children:?}");
    assert_eq!(kil_term::live_children(), 0);
}
