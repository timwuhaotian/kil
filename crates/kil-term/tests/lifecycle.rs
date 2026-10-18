//! Process lifecycle invariants (DESIGN.md §4.8): teardown, reaping, identity.

mod common;

use std::time::{Duration, Instant};

use common::{sh, start, wait_for_foreground, wait_for_text, wait_until, zsh};
use kil_term::{Pid, SessionEvent, diagnostics};

#[test]
fn drop_should_not_block_when_child_ignores_sighup() {
    let probe = start(&sh("trap '' HUP; exec /bin/sleep 600"));
    assert!(wait_for_foreground(&probe.session, "sleep"));
    let pid = probe.session.pid();
    let started = Instant::now();
    drop(probe.session);
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "drop took {:?}",
        started.elapsed()
    );
    // Outlive the reaper's deadline: exiting first would leave the child running for good.
    // (`expedite_reaping` would also cut short the grace another test measures.)
    assert!(wait_until(Duration::from_secs(6), || !diagnostics::exists(
        pid
    )));
}

#[test]
fn reaper_should_kill_and_reap_child_that_ignores_sighup_after_grace() {
    let probe = start(&sh("trap '' HUP; exec /bin/sleep 600"));
    assert!(wait_for_foreground(&probe.session, "sleep"));
    let pid = probe.session.pid();
    let closed = Instant::now();
    drop(probe.session);
    assert!(wait_until(Duration::from_secs(6), || !diagnostics::exists(
        pid
    )));
    let elapsed = closed.elapsed();
    assert!(
        elapsed >= Duration::from_millis(2_500),
        "reaped before grace: {elapsed:?}"
    );
}

#[test]
fn close_should_not_deadlock_when_child_blocks_on_undrained_output() {
    let probe = start(&sh("exec /usr/bin/yes"));
    assert!(wait_for_foreground(&probe.session, "yes"));
    let pid = probe.session.pid();
    drop(probe.session);
    assert!(wait_until(Duration::from_secs(1), || !diagnostics::exists(
        pid
    )));
}

#[test]
fn close_should_reap_idle_shell_quickly() {
    let probe = start(&zsh());
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .foreground()
        .is_some()));
    let pid = probe.session.pid();
    drop(probe.session);
    assert!(wait_until(Duration::from_secs(1), || !diagnostics::exists(
        pid
    )));
}

#[test]
fn close_should_leave_hup_ignoring_background_job_running_under_launchd() {
    let marker = std::env::temp_dir().join(format!("kil-bg-{}", std::process::id()));
    let script = format!(
        "(trap '' HUP; exec /bin/sleep 600) & echo $! > '{}'; exec /bin/sleep 600",
        marker.display()
    );
    let probe = start(&sh(&script));
    assert!(wait_until(Duration::from_secs(5), || marker.exists()));
    let job = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let job = Pid::new(job).unwrap();
    let shell = probe.session.pid();
    drop(probe.session);
    assert!(wait_until(Duration::from_secs(2), || !diagnostics::exists(
        shell
    )));
    let parent = diagnostics::parent_of(job);
    let _ = std::process::Command::new("/bin/kill")
        .args(["-9", &job.get().to_string()])
        .status();
    let _ = std::fs::remove_file(&marker);
    assert_eq!(parent, Some(1));
}

#[test]
fn natural_exit_should_report_status_then_end() {
    let probe = start(&sh("exit 3"));
    let mut seen = Vec::new();
    while let Ok(event) = probe.events.recv_timeout(Duration::from_secs(5)) {
        match event {
            SessionEvent::ChildExited(status) => seen.push(format!("exit:{:?}", status.code())),
            SessionEvent::Ended => {
                seen.push("ended".to_owned());
                break;
            }
            _ => {}
        }
    }
    assert_eq!(seen, ["exit:Some(3)", "ended"]);
}

#[test]
fn ended_should_be_delivered_after_close() {
    let probe = start(&sh("exec /bin/sleep 600"));
    drop(probe.session);
    let ended = std::iter::from_fn(|| probe.events.recv_timeout(Duration::from_secs(2)).ok())
        .any(|event| matches!(event, SessionEvent::Ended));
    assert!(ended);
}

#[test]
fn foreground_should_report_job_when_interactive_shell_runs_one() {
    let probe = start(&zsh());
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .foreground()
        .is_some_and(|fg| fg.is_shell)));
    probe.session.write(b"/bin/sleep 600\n".as_slice());
    assert!(wait_for_foreground(&probe.session, "sleep"));
    assert!(probe.session.is_busy());
}

#[test]
fn is_busy_should_be_false_when_shell_is_idle() {
    let probe = start(&zsh());
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .foreground()
        .is_some()));
    assert!(!probe.session.is_busy());
}

#[test]
fn foreground_should_report_shell_cwd() {
    let mut config = zsh();
    config.cwd = Some("/private/tmp".into());
    let probe = start(&config);
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .foreground()
        .is_some()));
    let cwd = probe.session.foreground().and_then(|fg| fg.cwd);
    assert_eq!(cwd.as_deref(), Some(std::path::Path::new("/private/tmp")));
}

#[test]
fn sessions_should_not_install_signal_handlers() {
    let probe = start(&sh("exit 0"));
    drop(probe.session);
    assert!(diagnostics::signal_is_default(libc::SIGCHLD));
}

#[test]
fn spawn_should_fail_cleanly_when_program_is_not_executable() {
    let result = kil_term::Session::spawn(&common::program("/etc/hosts", &[]), |_| {});
    assert!(matches!(
        result,
        Err(kil_term::SpawnError::NotExecutable(_))
    ));
}

#[test]
fn output_should_appear_in_snapshot_with_wide_chars_aligned() {
    let probe = start(&sh("printf 'hello 中文 kil'; exec /bin/sleep 600"));
    assert!(wait_for_text(&probe.session, "hello 中文 kil"));
}

#[test]
fn job_args_should_report_the_foreground_command_line() {
    let probe = start(&zsh());
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .foreground()
        .is_some_and(|fg| fg.is_shell)));
    assert_eq!(probe.session.job_args(), None);
    // `; :` keeps sh from exec-ing into sleep, which would replace the arguments.
    probe
        .session
        .write(b"/bin/sh -c '/bin/sleep 600; :' 'a b' ''\n".as_slice());
    // macOS /bin/sh re-execs a real shell, so its name varies; its arguments don't.
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .job_args()
        .is_some_and(|args| args.len() == 5)));
    let args = probe.session.job_args().unwrap();
    assert_eq!(args[1..], ["-c", "/bin/sleep 600; :", "a b", ""]);
}

#[test]
fn shell_cwd_should_stay_with_the_shell_while_a_job_runs_elsewhere() {
    let mut config = zsh();
    config.cwd = Some("/private/tmp".into());
    let probe = start(&config);
    assert!(wait_until(Duration::from_secs(5), || probe
        .session
        .foreground()
        .is_some()));
    probe
        .session
        .write(b"(cd /usr && exec /bin/sleep 600)\n".as_slice());
    assert!(wait_for_foreground(&probe.session, "sleep"));
    let job_cwd = probe.session.foreground().and_then(|fg| fg.cwd);
    assert_eq!(job_cwd.as_deref(), Some(std::path::Path::new("/usr")));
    assert_eq!(
        probe.session.shell_cwd().as_deref(),
        Some(std::path::Path::new("/private/tmp"))
    );
}
