//! A crashed kil leaves no live jobs behind: the kernel hangs up every session (DESIGN.md §4.6,
//! F15). The test re-runs its own binary as the crashing owner.
#![expect(
    unsafe_code,
    reason = "the owner blocks signals like a GCD worker thread"
)]

mod common;

use std::{
    mem::MaybeUninit,
    path::{Path, PathBuf},
    process::Command,
    ptr, thread,
    time::Duration,
};

use common::{sh, start, wait_until};
use kil_term::{Pid, diagnostics};

const TEST: &str = "crash_of_the_owner_should_hang_up_its_jobs";
const SERVER_TEST: &str = "crash_of_the_session_server_should_hang_up_its_jobs";
/// Set in the re-run binary: where the owner writes its job's PID before aborting.
const OWNER: &str = "KIL_TEST_CRASH_OWNER";
/// Like `OWNER`, for an owner that runs the job through a session server it hosts.
const SERVER_OWNER: &str = "KIL_TEST_CRASH_SERVER_OWNER";

fn read_pid(path: &Path) -> Option<Pid> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse()
        .ok()
        .and_then(Pid::new)
}

/// Starts a job the way the app does (from a thread that blocks every signal), then aborts.
#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn crash_as_owner(marker: &Path) -> ! {
    let script = format!("echo $$ > '{}'; exec /bin/sleep 600", marker.display());
    // Kept alive until the abort, which runs no destructors: only the kernel cleans up.
    let _probe = thread::spawn(move || {
        let mut all = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: initializes `all`, then blocks every signal on this thread only.
        unsafe {
            libc::sigfillset(all.as_mut_ptr());
            libc::pthread_sigmask(libc::SIG_BLOCK, all.as_ptr(), ptr::null_mut());
        }
        start(&sh(&script))
    })
    .join()
    .unwrap();
    assert!(wait_until(Duration::from_secs(5), || read_pid(marker).is_some()));
    std::process::abort();
}

/// Hosts a session server, starts a job through it as the app would, then aborts.
#[expect(clippy::unwrap_used, reason = "test helper: a failure fails the test")]
fn crash_as_server(marker: &Path) -> ! {
    let dir = marker.with_extension("server");
    let _ = std::fs::remove_dir_all(&dir);
    kil_term::server::serve_in_thread(&dir).unwrap();
    let client = kil_term::Client::connect(&dir).unwrap();
    let script = format!("echo $$ > '{}'; exec /bin/sleep 600", marker.display());
    // Detached at once: only the server holds the job.
    drop(client.spawn("a-1", &sh(&script), |_| {}).unwrap());
    assert!(wait_until(Duration::from_secs(5), || read_pid(marker).is_some()));
    std::process::abort();
}

/// Re-runs this binary as `test`, with `owner` naming where its job's PID goes, and checks
/// that the job died with it.
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helper: a failure fails the test"
)]
fn crash_and_check(test: &str, owner: &str) {
    let marker: PathBuf =
        std::env::temp_dir().join(format!("kil-crash-{}-{}", std::process::id(), owner.len()));
    let _ = std::fs::remove_file(&marker);
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(owner, &marker)
        .status()
        .unwrap();
    let job = read_pid(&marker);
    let _ = std::fs::remove_file(&marker);
    assert!(!status.success(), "the owner was meant to abort");
    let job = job.expect("the owner started its job");
    let gone = wait_until(Duration::from_secs(2), || !diagnostics::exists(job));
    if !gone {
        let _ = Command::new("/bin/kill")
            .args(["-9", &job.get().to_string()])
            .status();
    }
    assert!(gone, "job {} outlived its crashed owner", job.get());
}

#[test]
fn crash_of_the_owner_should_hang_up_its_jobs() {
    if let Some(marker) = std::env::var_os(OWNER) {
        crash_as_owner(Path::new(&marker));
    }
    crash_and_check(TEST, OWNER);
}

#[test]
fn crash_of_the_session_server_should_hang_up_its_jobs() {
    if let Some(marker) = std::env::var_os(SERVER_OWNER) {
        crash_as_server(Path::new(&marker));
    }
    crash_and_check(SERVER_TEST, SERVER_OWNER);
}
