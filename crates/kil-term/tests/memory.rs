//! What a session costs the machine (DESIGN.md §5.2): its grid, and its program.

mod common;

use std::time::Duration;

use common::{sh, start, wait_until};
use kil_term::{Client, Session, SessionConfig, system};

#[test]
fn process_memory_should_count_what_the_program_holds() {
    // The shell keeps a 32 MB string while it waits, so the tree is far bigger than the
    // shell alone.
    let probe = start(&sh(
        "x=$(head -c 33554432 /dev/zero | tr '\\0' 'a'); sleep 600",
    ));
    assert!(
        wait_until(Duration::from_secs(20), || probe.session.process_memory()
            > 20 * 1024 * 1024),
        "the program's memory is not counted: {} bytes",
        probe.session.process_memory()
    );
    // One reading: two in a row differ while the shell is still taking the string in.
    let process_bytes = probe.session.memory().process_bytes;
    assert!(
        process_bytes > 20 * 1024 * 1024,
        "memory() leaves the program out: {process_bytes} bytes"
    );
}

#[test]
fn an_attached_session_should_report_its_program() {
    // The app's path: the program runs in the server, the app holds a mirror of it.
    let dir = std::env::temp_dir().join(format!("kil-mem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    kil_term::server::serve_in_thread(&dir).unwrap();
    let client = Client::connect(&dir).unwrap();
    let mut config = SessionConfig::new(common::SIZE);
    config.program = kil_term::Program::Command {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "x=$(head -c 33554432 /dev/zero | tr '\\0' 'a'); sleep 600".into(),
        ],
    };
    let session: Session = client.spawn("mem-1", &config, |_| {}).unwrap();
    assert!(
        wait_until(Duration::from_secs(20), || session.process_memory()
            > 20 * 1024 * 1024),
        "the program's memory is not counted: {} bytes",
        session.process_memory()
    );
    client.stop_all().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn system_should_report_its_own_memory_state() {
    let memory = system::memory();
    assert!(memory.swap_total >= memory.swap_used);
    assert!(matches!(
        memory.pressure,
        system::Pressure::Normal | system::Pressure::Warning | system::Pressure::Critical
    ));
}
