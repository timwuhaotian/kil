//! Shared helpers for kil-term integration tests (real processes, real PTYs).
#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::{
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use kil_term::{Frame, GridSize, Palette, Program, Session, SessionConfig, SessionEvent};

pub const SIZE: GridSize = GridSize {
    rows: 24,
    cols: 80,
    cell_width: 8,
    cell_height: 16,
};

pub fn program(path: &str, args: &[&str]) -> SessionConfig {
    let mut config = SessionConfig::new(SIZE);
    config.program = Program::Command {
        program: path.into(),
        args: args.iter().map(Into::into).collect(),
    };
    config.scrollback_lines = 1_000;
    config
}

pub fn sh(script: &str) -> SessionConfig {
    program("/bin/sh", &["-c", script])
}

/// An interactive zsh without user rc files, so job control behaves like a real shell.
pub fn zsh() -> SessionConfig {
    program("/bin/zsh", &["-f", "-i"])
}

pub struct Probe {
    pub session: Session,
    pub events: Receiver<SessionEvent>,
}

#[expect(
    clippy::expect_used,
    reason = "test helper: a failed spawn fails the test"
)]
pub fn start(config: &SessionConfig) -> Probe {
    let (tx, events) = mpsc::channel();
    let session = Session::spawn(config, move |event| {
        let _ = tx.send(event);
    })
    .expect("spawn session");
    Probe { session, events }
}

pub fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    done()
}

pub fn screen(session: &Session) -> String {
    let mut frame = Frame::default();
    session.snapshot_into(&mut frame, &Palette::default());
    (0..frame.rows)
        .map(|row| frame.row_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn wait_for_text(session: &Session, needle: &str) -> bool {
    wait_until(Duration::from_secs(5), || screen(session).contains(needle))
}

pub fn wait_for_foreground(session: &Session, name: &str) -> bool {
    wait_until(Duration::from_secs(5), || {
        session.foreground().is_some_and(|fg| fg.name == name)
    })
}
