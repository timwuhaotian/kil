//! The session server keeps jobs running without a client and hands them back exactly
//! (DESIGN.md §14.7). Real processes, with the server on a thread of the test process.

mod common;

use std::{
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};

use common::{SIZE, program, screen, sh, wait_for_text, wait_until};
use kil_term::{Client, Session, SessionConfig, SessionEvent, diagnostics, server};

/// A server of its own, so the tests don't share sessions.
#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn server(name: &str) -> (PathBuf, Client) {
    let dir = std::env::temp_dir().join(format!("kil-srv-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    server::serve_in_thread(&dir).unwrap();
    let client = Client::connect(&dir).unwrap();
    (dir, client)
}

#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn spawn(client: &Client, id: &str, config: &SessionConfig) -> Session {
    client.spawn(id, config, |_| {}).unwrap()
}

#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn attach(client: &Client, id: &str) -> Session {
    client.attach(id, 1_000, |_| {}).unwrap().unwrap()
}

#[test]
fn a_second_attach_should_share_the_session_not_steal_it() {
    // The Kil window and a phone hold the same session at once: output fans
    // out to both, input from either reaches the program, and neither is
    // evicted with an Ended.
    let (_dir, client) = server("fanout");
    drop(spawn(&client, "a-1", &sh("exec /bin/cat")));
    let (tx, rx) = mpsc::channel();
    let first = client
        .attach("a-1", 1_000, move |event| {
            let _ = tx.send(event);
        })
        .unwrap()
        .unwrap();
    let second = attach(&client, "a-1");
    second.write(b"hello\n".as_slice());
    assert!(wait_for_text(&first, "hello"));
    assert!(wait_for_text(&second, "hello"));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        rx.try_iter()
            .all(|event| !matches!(event, SessionEvent::Ended | SessionEvent::ChildExited(_))),
        "the first client must not see the session end"
    );
    client.stop_all().unwrap();
}

#[test]
fn watchers_should_list_sessions_with_another_client_attached() {
    // The app holds one client per pane it shows; a phone attaching to a session is a
    // second one, and the app's presence mark follows this list.
    let (_dir, client) = server("watchers");
    let first = spawn(&client, "a-1", &sh("exec /bin/sleep 600"));
    assert_eq!(client.watchers().unwrap(), Vec::<String>::new());
    let second = attach(&client, "a-1");
    assert!(wait_until(Duration::from_secs(5), || client
        .watchers()
        .is_ok_and(|watched| watched == ["a-1"])));
    drop(second);
    assert!(wait_until(Duration::from_secs(5), || client
        .watchers()
        .is_ok_and(|watched| watched.is_empty())));
    drop(first);
    client.stop_all().unwrap();
}

#[test]
fn remote_input_should_mark_a_session_controlled_until_it_goes_quiet_or_leaves() {
    // A phone attaches as remote (as the bridge declares it), idles, types, then leaves.
    // While idle it only watches; its keystrokes drive the session, and land in the
    // program like any other input; detaching clears both marks.
    let (dir, client) = server("controlling");
    let first = spawn(&client, "a-1", &sh("exec /bin/cat"));
    // The desktop typing never drives: only a phone's input does.
    first.write(b"local\n".as_slice());
    assert!(wait_for_text(&first, "local"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(client.controlling().unwrap(), Vec::<String>::new());

    let mut phone = UnixStream::connect(server::socket_path(&dir)).unwrap();
    phone
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    // Reads what the server sent so far: the phone's output must be taken, or the server
    // drops it as stalled.
    let drain = |phone: &mut UnixStream| {
        let mut buf = [0u8; 8192];
        loop {
            match phone.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    };
    phone
        .write_all(&frame(19, &[]))
        .and_then(|()| phone.write_all(&frame(1, &2_u32.to_le_bytes())))
        .unwrap();
    let id = b"a-1";
    let mut attach = u32::try_from(id.len()).unwrap().to_le_bytes().to_vec();
    attach.extend_from_slice(id);
    phone.write_all(&frame(5, &attach)).unwrap();
    drain(&mut phone);

    // Attached but idle: watched, not controlled.
    assert!(wait_until(Duration::from_secs(5), || client
        .watchers()
        .is_ok_and(|watched| watched == ["a-1"])));
    assert_eq!(client.controlling().unwrap(), Vec::<String>::new());

    // A keystroke drives the session, and reaches the program untouched.
    phone.write_all(&frame(6, b"hi\n")).unwrap();
    drain(&mut phone);
    assert!(wait_until(Duration::from_secs(5), || client
        .controlling()
        .is_ok_and(|controlling| controlling == ["a-1"])));
    assert!(wait_for_text(&first, "hi"));

    // It detaches: both marks clear, even within the input window.
    drop(phone);
    assert!(wait_until(Duration::from_secs(5), || client
        .watchers()
        .is_ok_and(|watched| watched.is_empty())));
    assert!(wait_until(Duration::from_secs(5), || client
        .controlling()
        .is_ok_and(|controlling| controlling.is_empty())));
    drop(first);
    client.stop_all().unwrap();
}

#[test]
fn a_pane_sees_the_clients_term_program_version() {
    // The server stamps its own version, and a server started by an older Kil keeps
    // stamping that; the client stamps over it, so a program gating OSC 9 notifications
    // on the version doesn't fall back to osascript after an upgrade.
    let (_dir, client) = server("version");
    let mut config = sh("printf 'ver=%s\\n' \"$TERM_PROGRAM_VERSION\"; exec /bin/sleep 600");
    config.env = vec![("TERM_PROGRAM_VERSION".into(), "0.0.8".into())];
    let session = spawn(&client, "a-1", &config);
    assert!(wait_for_text(
        &session,
        &format!("ver={}", env!("CARGO_PKG_VERSION"))
    ));
    client.stop_all().unwrap();
}

#[test]
fn detach_should_keep_the_job_running() {
    let (_dir, client) = server("detach");
    let session = spawn(&client, "a-1", &sh("exec /bin/sleep 600"));
    assert!(wait_until(Duration::from_secs(5), || session
        .foreground()
        .is_some_and(|fg| fg.name == "sleep")));
    let pid = session.pid();
    drop(session);
    std::thread::sleep(Duration::from_secs(2));
    assert!(diagnostics::exists(pid));
    assert_eq!(client.sessions().unwrap(), ["a-1"]);
    client.stop_all().unwrap();
    assert!(!diagnostics::exists(pid));
}

#[test]
fn attach_should_rebuild_the_primary_screen_with_output_from_while_detached() {
    let (_dir, client) = server("primary");
    let session = spawn(
        &client,
        "a-1",
        &sh("printf 'before\\n'; /bin/sleep 0.5; printf 'during\\n'; exec /bin/sleep 600"),
    );
    assert!(wait_for_text(&session, "before"));
    drop(session);
    std::thread::sleep(Duration::from_secs(1));
    let session = attach(&client, "a-1");
    assert!(wait_for_text(&session, "during"));
    let text = screen(&session);
    assert!(text.find("before") < text.find("during"), "{text}");
    client.stop_all().unwrap();
}

#[test]
fn attach_should_rebuild_the_alternate_screen_and_modes() {
    let (_dir, client) = server("alternate");
    let first = spawn(
        &client,
        "a-1",
        &sh(
            "printf 'shell\\n\\033[?1049h\\033[5;3Hvim text\\033[?1h\\033[?2004h\\033[?1002h\\033[?1006h'; \
             exec /bin/sleep 600",
        ),
    );
    assert!(wait_for_text(&first, "vim text"));
    let (text, mode) = (screen(&first), first.input_mode());
    assert!(mode.alt_screen() && mode.app_cursor() && mode.bracketed_paste());
    let second = attach(&client, "a-1");
    assert!(wait_for_text(&second, "vim text"));
    assert_eq!((screen(&second), second.input_mode()), (text, mode));
    client.stop_all().unwrap();
}

#[test]
fn detached_output_should_not_block_the_program() {
    let (dir, client) = server("flood");
    let done = dir.join("done");
    let script = format!(
        "/usr/bin/seq 1 200000; /usr/bin/touch '{}'; exec /bin/sleep 600",
        done.display()
    );
    drop(spawn(&client, "a-1", &sh(&script)));
    assert!(wait_until(Duration::from_secs(20), || done.exists()));
    let session = attach(&client, "a-1");
    assert!(wait_for_text(&session, "200000"));
    client.stop_all().unwrap();
}

#[test]
fn queries_should_be_answered_once_by_the_server() {
    let (_dir, client) = server("queries");
    let script = "stty raw -echo min 0 time 5; printf '\\033[6n\\033]11;?\\033\\\\'; /bin/sleep 0.5; \
                  r=$(dd bs=256 count=1 2>/dev/null); stty sane; \
                  printf 'GOT:%s:END' \"$r\" | tr -d '\\033\\\\'; exec /bin/sleep 600";
    let session = spawn(&client, "a-1", &sh(script));
    assert!(wait_for_text(&session, ":END"));
    let text = screen(&session);
    assert_eq!(text.matches('R').count(), 1, "{text}");
    assert!(text.contains("]11;rgb:1111/1313/1616"), "{text}");
    client.stop_all().unwrap();
}

#[test]
fn child_exit_should_reach_the_client_with_its_status() {
    let (_dir, client) = server("exit");
    let (tx, rx) = mpsc::channel();
    let session = client
        .spawn("a-1", &sh("/bin/sleep 0.3; exit 3"), move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
    let status =
        std::iter::from_fn(|| rx.recv_timeout(Duration::from_secs(5)).ok()).find_map(|event| {
            match event {
                SessionEvent::ChildExited(status) => Some(status.code()),
                _ => None,
            }
        });
    assert_eq!(status, Some(Some(3)));
    drop(session);
    assert!(wait_until(Duration::from_secs(2), || client
        .sessions()
        .unwrap()
        .is_empty()));
}

#[test]
fn close_should_end_the_session_in_the_server() {
    let (_dir, client) = server("close");
    let session = spawn(&client, "a-1", &sh("exec /bin/sleep 600"));
    assert!(wait_until(Duration::from_secs(5), || session
        .foreground()
        .is_some_and(|fg| fg.name == "sleep")));
    let pid = session.pid();
    session.close();
    assert!(wait_until(Duration::from_secs(5), || !diagnostics::exists(
        pid
    )));
    assert_eq!(client.sessions().unwrap(), Vec::<String>::new());
}

/// Hand-written frames: the wire module is the crate's own.
#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(payload.len() + 1)
        .unwrap()
        .to_le_bytes()
        .to_vec();
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

#[test]
fn a_client_that_stops_reading_should_be_dropped_not_stall_the_program() {
    let (dir, client) = server("slow");
    let counter = dir.join("count");
    let script = format!(
        "i=0; while :; do /usr/bin/yes | /usr/bin/head -c 65536; i=$((i+1)); echo $i > '{}'; done",
        counter.display()
    );
    drop(spawn(&client, "a-1", &sh(&script)));
    let mut stalled = UnixStream::connect(server::socket_path(&dir)).unwrap();
    stalled.write_all(&frame(1, &1_u32.to_le_bytes())).unwrap();
    let id = b"a-1";
    let mut attach = u32::try_from(id.len()).unwrap().to_le_bytes().to_vec();
    attach.extend_from_slice(id);
    stalled.write_all(&frame(5, &attach)).unwrap();
    let count = || {
        std::fs::read_to_string(&counter)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
            .unwrap_or(0)
    };
    // 4 MiB of backlog is 64 rounds; the program must get well past that.
    assert!(wait_until(Duration::from_secs(20), || count() > 200));
    // The server hung up on the stalled client: draining it reaches the end, where a
    // client still attached would be fed forever.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = tx.send(stalled.read_to_end(&mut sink).is_ok());
    });
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(true));
    client.stop_all().unwrap();
}

#[test]
fn the_socket_should_be_private_and_vanish_when_idle() {
    let dir = std::env::temp_dir().join(format!("kil-srv-{}-idle", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    server::serve_in_thread(&dir).unwrap();
    let socket = server::socket_path(&dir);
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(wait_until(Duration::from_secs(10), || !socket.exists()));
}

#[test]
fn the_server_should_exit_once_its_last_session_and_client_are_gone() {
    let (dir, client) = server("last");
    spawn(&client, "a-1", &sh("exec /bin/sleep 600")).close();
    drop(client);
    // No timer wakes a server that has sessions: their leaving must.
    let socket = server::socket_path(&dir);
    assert!(wait_until(Duration::from_secs(10), || !socket.exists()));
}

#[test]
fn spawn_should_paint_the_replay_before_the_program_output() {
    let (_dir, client) = server("replay");
    let mut config = program("/bin/sh", &["-c", "printf NEW; exec /bin/sleep 600"]);
    config.size = SIZE;
    config.replay = b"OLD\r\n".to_vec();
    let session = spawn(&client, "a-1", &config);
    assert!(wait_for_text(&session, "NEW"));
    let text = screen(&session);
    assert!(text.find("OLD") < text.find("NEW"), "{text}");
    client.stop_all().unwrap();
}

#[test]
fn color_queries_should_follow_the_theme_palette() {
    let (dir, client) = server("palette");
    let flag = dir.join("go");
    let script = format!(
        "stty raw -echo min 0 time 5; ask() {{ printf '\\033]11;?\\033\\\\' > /dev/tty; /bin/sleep 0.3; \
         dd bs=256 count=1 2>/dev/null | tr -d '\\033\\\\'; }}; printf 'ONE:%s:\\r\\n' \"$(ask)\"; \
         while [ ! -e '{}' ]; do /bin/sleep 0.05; done; printf 'TWO:%s:END' \"$(ask)\"; \
         exec /bin/sleep 600",
        flag.display()
    );
    let mut config = sh(&script);
    config.palette.background = kil_term::Rgba::hex(0x00fb_faf7);
    let session = spawn(&client, "a-1", &config);
    assert!(wait_for_text(&session, "ONE:"));
    let mut light = config.palette;
    light.background = kil_term::Rgba::hex(0x0001_0203);
    session.set_palette(&light);
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(&flag, "").unwrap();
    assert!(wait_for_text(&session, ":END"));
    let text = screen(&session);
    assert!(text.contains("ONE:]11;rgb:fbfb/fafa/f7f7"), "{text}");
    assert!(text.contains("TWO:]11;rgb:0101/0202/0303"), "{text}");
    client.stop_all().unwrap();
}

/// A protocol-1 server: hangs up on any other hello, then lists no sessions.
#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn protocol_1_server(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(server::socket_path(dir)).unwrap();
    std::thread::spawn(move || {
        let hello_1 = frame(1, &1_u32.to_le_bytes());
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut hello = [0_u8; 9];
            stream.read_exact(&mut hello).unwrap();
            stream.write_all(&hello_1).unwrap();
            if hello[5..9] != 1_u32.to_le_bytes() {
                continue;
            }
            let mut list = [0_u8; 5];
            if stream.read_exact(&mut list).is_ok() {
                stream.write_all(&frame(9, &0_u32.to_le_bytes())).unwrap();
            }
        }
    });
}

#[test]
fn client_should_speak_down_to_a_protocol_1_server() {
    let dir = std::env::temp_dir().join(format!("kil-srv-{}-v1", std::process::id()));
    protocol_1_server(&dir);
    let client = Client::connect(&dir).unwrap();
    assert_eq!(client.sessions().unwrap(), Vec::<String>::new());
}

/// A raw client, as the bridge relays a phone: says hello, asks for `id`, and reads on a
/// thread of its own so it never stalls. Returns the stream to write to and the frames,
/// as `(kind, payload)`, until the server hangs up.
#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn raw_attach(
    dir: &std::path::Path,
    id: &str,
    remote: bool,
) -> (UnixStream, mpsc::Receiver<(u8, Vec<u8>)>) {
    let mut stream = UnixStream::connect(server::socket_path(dir)).unwrap();
    if remote {
        stream.write_all(&frame(19, &[])).unwrap();
    }
    stream.write_all(&frame(1, &2_u32.to_le_bytes())).unwrap();
    let mut attach = u32::try_from(id.len()).unwrap().to_le_bytes().to_vec();
    attach.extend_from_slice(id.as_bytes());
    stream.write_all(&frame(5, &attach)).unwrap();
    let mut reader = stream.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Some(frame) = read_frame(&mut reader) {
            if tx.send(frame).is_err() {
                break;
            }
        }
    });
    (stream, rx)
}

/// One frame from a blocking stream; `None` once it closed.
fn read_frame(stream: &mut impl Read) -> Option<(u8, Vec<u8>)> {
    let mut header = [0_u8; 5];
    stream.read_exact(&mut header).ok()?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut payload = vec![0; len.checked_sub(1)?];
    stream.read_exact(&mut payload).ok()?;
    Some((header[4], payload))
}

/// Output frames' text, until `needle` shows up (`true`) or the server hangs up.
fn output_reaches(frames: &mpsc::Receiver<(u8, Vec<u8>)>, needle: &str) -> bool {
    let mut seen = String::new();
    while let Ok((kind, payload)) = frames.recv_timeout(Duration::from_secs(30)) {
        if kind == 14 {
            seen.push_str(&String::from_utf8_lossy(&payload));
            if seen.contains(needle) {
                return true;
            }
            // Only the tail can still complete the needle.
            let keep = seen.len().saturating_sub(needle.len());
            seen.drain(..seen.floor_char_boundary(keep));
        }
    }
    false
}

#[test]
fn a_client_that_keeps_up_should_see_a_flood_through_to_the_exit() {
    // One read of the PTY can take in a megabyte before the loop gets back to its clients.
    // That alone must not count as falling behind: a client reading as fast as it can (the
    // Kil window, say) keeps the session through a flood, up to the exit status.
    let (dir, client) = server("flood-reader");
    let go = dir.join("go");
    let script = format!(
        "while [ ! -e '{}' ]; do /bin/sleep 0.05; done; /usr/bin/yes | /usr/bin/head -c 8000000; exit 3",
        go.display()
    );
    drop(spawn(&client, "a-1", &sh(&script)));
    let (_stream, frames) = raw_attach(&dir, "a-1", false);
    std::fs::write(&go, b"").unwrap();
    let exit = std::iter::from_fn(|| frames.recv_timeout(Duration::from_secs(30)).ok())
        .find(|(kind, _)| *kind == 15)
        .map(|(_, payload)| u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]));
    assert_eq!(exit, Some(3 << 8), "the client was dropped before the exit");
}

#[test]
fn an_attach_should_take_a_scrollback_bigger_than_the_high_water_mark() {
    // A wide pane's whole scrollback is megabytes of state. It is what an attach starts
    // with, not a backlog: the client keeps the session and gets the output after it.
    let (dir, client) = server("big-state");
    let (done, go) = (dir.join("done"), dir.join("go"));
    let mut config = sh(&format!(
        "/usr/bin/yes $(/usr/bin/printf '%0511d' 0) | /usr/bin/head -n 12000; \
         /usr/bin/touch '{}'; while [ ! -e '{}' ]; do /bin/sleep 0.05; done; \
         echo AFTER-THE-STATE; exec /bin/sleep 600",
        done.display(),
        go.display()
    ));
    config.size.cols = 512;
    config.scrollback_lines = 10_000;
    drop(spawn(&client, "a-1", &config));
    assert!(wait_until(Duration::from_secs(60), || done.exists()));
    let (_stream, frames) = raw_attach(&dir, "a-1", false);
    std::fs::write(&go, b"").unwrap();
    assert!(output_reaches(&frames, "AFTER-THE-STATE"));
    client.stop_all().unwrap();
}

/// The payload of the next frame of `kind`, skipping the others.
fn next_of_kind(frames: &mpsc::Receiver<(u8, Vec<u8>)>, kind: u8) -> Option<Vec<u8>> {
    std::iter::from_fn(|| frames.recv_timeout(Duration::from_secs(10)).ok())
        .find(|(found, _)| *found == kind)
        .map(|(_, payload)| payload)
}

#[test]
fn a_phone_should_follow_the_windows_theme_and_grid() {
    // A phone mirrors what the window shows, so it needs the window's palette and grid,
    // which only the window sends. The server passes them to remote clients: on attach,
    // and whenever the window changes them. A local client never gets them back.
    let (dir, client) = server("to-phones");
    let mut config = sh("exec /bin/cat");
    config.palette.background = kil_term::Rgba { r: 1, g: 2, b: 3 };
    let window = spawn(&client, "a-1", &config);
    let (_phone, phone) = raw_attach(&dir, "a-1", true);
    let (_desk, desk) = raw_attach(&dir, "a-1", false);

    // Palette: 16 ANSI colors, then foreground, background and cursor, 3 bytes each.
    let palette = next_of_kind(&phone, 16).unwrap();
    assert_eq!(palette.len(), 19 * 3);
    assert_eq!(palette[51..54], [1, 2, 3]);

    let mut theme = config.palette;
    theme.background = kil_term::Rgba { r: 4, g: 5, b: 6 };
    window.set_palette(&theme);
    window.resize(kil_term::GridSize {
        rows: 30,
        cols: 100,
        ..SIZE
    });
    assert_eq!(next_of_kind(&phone, 16).unwrap()[51..54], [4, 5, 6]);
    // Rows, then columns, as u16 little-endian.
    assert_eq!(next_of_kind(&phone, 7).unwrap()[..4], [30, 0, 100, 0]);

    std::thread::sleep(Duration::from_millis(300));
    assert!(
        desk.try_iter().all(|(kind, _)| kind != 16 && kind != 7),
        "a local client must not get the window's own frames"
    );
    client.stop_all().unwrap();
}

/// A server that takes session `a-1` away from its client, as one stalled or behind is
/// dropped, or as Kil before 0.0.31 handed it to a phone: each attach draws a line naming
/// which attach it is, reports the frames its client sends, and hangs up once the client
/// asks for a grid.
#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn dropping_server(dir: &std::path::Path) -> mpsc::Receiver<(usize, u8, Vec<u8>)> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(server::socket_path(dir)).unwrap();
    let (tx, rx) = mpsc::channel();
    let attaches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let (tx, attaches) = (tx.clone(), std::sync::Arc::clone(&attaches));
            std::thread::spawn(move || {
                if !matches!(read_frame(&mut stream), Some((1, _))) {
                    return;
                }
                stream.write_all(&frame(1, &2_u32.to_le_bytes())).unwrap();
                // The control connection never asks for more and waits here.
                if !matches!(read_frame(&mut stream), Some((5, _))) {
                    return;
                }
                let n = attaches.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                // Identity: pid, start time, tty, then a 24×80 grid of 8×16 cells.
                let mut identity = std::process::id().to_le_bytes().to_vec();
                identity.extend_from_slice(&[0; 20]);
                for value in [24_u16, 80, 8, 16] {
                    identity.extend_from_slice(&value.to_le_bytes());
                }
                stream.write_all(&frame(11, &identity)).unwrap();
                let line = if n == 1 { "first\r\n" } else { "second\r\n" };
                stream.write_all(&frame(14, line.as_bytes())).unwrap();
                while let Some((kind, payload)) = read_frame(&mut stream) {
                    let _ = tx.send((n, kind, payload));
                    if kind == 7 {
                        break;
                    }
                }
            });
        }
    });
    rx
}

#[test]
fn a_mirror_should_attach_again_when_the_server_drops_it() {
    // The window's mirror outlives a dropped connection while the session runs on: it
    // attaches again, starts over from the server's state, and asks for its grid again,
    // so the pane neither shows an exit nor keeps a stale size. Once: a second drop right
    // after ends it, rather than fight a phone for the session on an old server.
    let dir = std::env::temp_dir().join(format!("kil-srv-{}-drops", std::process::id()));
    let frames = dropping_server(&dir);
    let client = Client::connect(&dir).unwrap();
    let (tx, rx) = mpsc::channel();
    let session = client
        .attach("a-1", 1_000, move |event| {
            let _ = tx.send(event);
        })
        .unwrap()
        .unwrap();
    assert!(wait_for_text(&session, "first"));
    session.resize(kil_term::GridSize {
        rows: 30,
        cols: 100,
        ..SIZE
    });
    let first = frames.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((first.0, first.1), (1, 7));

    assert!(wait_for_text(&session, "second"));
    assert!(
        !screen(&session).contains("first"),
        "the mirror starts over from the new state"
    );
    let again = frames.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((again.0, again.1), (2, 7));
    assert_eq!(again.2[..4], [30, 0, 100, 0]);

    assert!(
        std::iter::from_fn(|| rx.recv_timeout(Duration::from_secs(10)).ok())
            .any(|event| matches!(event, SessionEvent::Ended))
    );
    assert!(
        frames.recv_timeout(Duration::from_millis(500)).is_err(),
        "no third attach"
    );
}
