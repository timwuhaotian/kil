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
