//! A program's OSC 9 and OSC 777 reach the UI as notifications through the real PTY: the
//! pane loop reads them, the scanner picks them out beside alacritty's parser, and the
//! session's events carry them.

mod common;

use std::{
    sync::mpsc::RecvTimeoutError,
    time::{Duration, Instant},
};

use common::{sh, start};
use kil_term::{Notification, SessionEvent};

#[test]
fn osc_9_and_777_from_a_program_arrive_as_notifications() {
    let probe = start(&sh(
        "printf '\\033]9;Done\\a'; printf '\\033]777;notify;killi;Fixed\\033\\\\'; exit 0",
    ));
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while got.len() < 2 && Instant::now() < deadline {
        match probe.events.recv_timeout(Duration::from_millis(200)) {
            Ok(SessionEvent::Notify(notification)) => got.push(notification),
            Ok(SessionEvent::Ended) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
        }
    }
    assert_eq!(
        got,
        [
            Notification {
                title: String::new(),
                body: "Done".into()
            },
            Notification {
                title: "killi".into(),
                body: "Fixed".into()
            },
        ]
    );
}
