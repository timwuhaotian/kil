//! Terminal plumbing that must work without a UI (DESIGN.md §6.1).

mod common;

use std::time::Duration;

use common::{screen, sh, start, wait_for_text, wait_until};
use kil_term::{CursorShape, CursorSnapshot, Frame, GridSize, Palette, Rgba, SessionEvent};

#[test]
fn cursor_position_query_should_be_answered_without_ui() {
    let script = "stty raw -echo; printf '\\033[6n'; r=$(dd bs=1 count=6 2>/dev/null); \
                  stty sane; printf 'GOT:%s' \"$r\" | tr -d '\\033'; exec /bin/sleep 600";
    let probe = start(&sh(script));
    assert!(wait_for_text(&probe.session, "GOT:[1;1R"));
}

#[test]
fn hidden_cursor_should_keep_its_place_for_ime_text() {
    // Like a TUI that hides the cursor, draws its own, and leaves the real one at its input.
    let probe = start(&sh(
        "printf '\\033[?25l> abc\\033[3;6H'; exec /bin/sleep 600",
    ));
    assert!(wait_for_text(&probe.session, "> abc"));
    let mut frame = Frame::default();
    assert!(wait_until(Duration::from_secs(5), || {
        probe.session.snapshot_into(&mut frame, &Palette::default());
        frame.cursor.is_some_and(|cursor| cursor.row == 2)
    }));
    assert_eq!(
        frame.cursor,
        Some(CursorSnapshot {
            row: 2,
            col: 5,
            shape: CursorShape::Hidden,
        })
    );
}

#[test]
fn resize_should_send_exactly_one_sigwinch_with_new_size() {
    let probe = start(&sh(
        "trap 'echo WINCH $(stty size)' WINCH; echo READY; while :; do /bin/sleep 0.05; done",
    ));
    assert!(wait_for_text(&probe.session, "READY"));
    probe.session.resize(GridSize {
        rows: 30,
        cols: 100,
        cell_width: 8,
        cell_height: 16,
    });
    assert!(wait_for_text(&probe.session, "WINCH 30 100"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(screen(&probe.session).matches("WINCH").count(), 1);
}

#[test]
fn clear_history_should_drop_scrollback() {
    let probe = start(&sh("seq 1 300; exec /bin/sleep 600"));
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .memory()
        .history_lines
        > 250));
    probe.session.clear_history();
    assert_eq!(probe.session.memory().history_lines, 0);
}

#[test]
fn memory_should_follow_cells_formula() {
    let probe = start(&sh("seq 1 300; exec /bin/sleep 600"));
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .memory()
        .history_lines
        > 250));
    let usage = probe.session.memory();
    assert_eq!(usage.grid_bytes, (usage.history_lines + 24) * 80 * 24);
}

#[test]
fn selection_text_should_return_selected_word() {
    let probe = start(&sh("printf 'alpha beta gamma'; exec /bin/sleep 600"));
    assert!(wait_for_text(&probe.session, "alpha beta gamma"));
    let cell = kil_term::ViewportCell {
        row: 0,
        col: 7,
        right_half: false,
    };
    probe
        .session
        .start_selection(kil_term::SelectionKind::Words, cell);
    assert_eq!(probe.session.selection_text().as_deref(), Some("beta"));
}

#[test]
fn snapshot_should_use_colors_the_program_redefined() {
    let probe = start(&sh(
        "printf '\\033]4;1;rgb:12/34/56\\007\\033[31mR\\033[0m'; exec /bin/sleep 600",
    ));
    assert!(wait_for_text(&probe.session, "R"));
    let fg = || {
        let mut frame = Frame::default();
        probe.session.snapshot_into(&mut frame, &Palette::default());
        frame.row(0)[0].fg
    };
    assert!(wait_until(Duration::from_secs(5), || fg() == Rgba::hex(0x12_34_56)));
}

#[test]
fn take_dirty_should_rearm_wakeups_for_a_session_nobody_draws() {
    let probe = start(&sh(
        "printf first; read _; printf second; exec /bin/sleep 600",
    ));
    let wakeup = |probe: &common::Probe| {
        std::iter::from_fn(|| probe.events.recv_timeout(Duration::from_secs(2)).ok())
            .any(|event| matches!(event, SessionEvent::Wakeup))
    };
    assert!(wakeup(&probe));
    assert!(probe.session.take_dirty());
    probe.session.write(b"\n".as_slice());
    assert!(wakeup(&probe));
}

#[test]
fn replay_should_paint_before_the_program_output() {
    let mut config = sh("printf NEW; exec /bin/sleep 600");
    config.replay = b"\x1b[31mOLD\x1b[0m\r\n".to_vec();
    let probe = start(&config);
    assert!(wait_for_text(&probe.session, "NEW"));
    let text = screen(&probe.session);
    assert!(text.find("OLD") < text.find("NEW"), "{text}");
}

#[test]
fn transcript_should_replay_into_a_new_session_as_it_was() {
    let probe = start(&sh(
        "printf 'one \\033[1;32mgreen\\033[0m\\n中文 two\\n'; exec /bin/sleep 600",
    ));
    assert!(wait_for_text(&probe.session, "中文 two"));
    let mut config = sh("exec /bin/sleep 600");
    config.replay = probe.session.transcript().render().unwrap();
    let copy = start(&config);
    assert!(wait_for_text(&copy.session, "中文 two"));
    let text = screen(&copy.session);
    assert!(text.starts_with("one green\n中文 two"), "{text}");
}
