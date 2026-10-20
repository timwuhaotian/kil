//! Open/close churn returns the process to baseline (DESIGN.md A11, F9). Alone in its binary
//! because it measures process-wide counters.

mod common;

use std::time::Duration;

use common::{sh, wait_until};
use kil_term::{Session, diagnostics};

#[derive(Debug, PartialEq, Eq)]
struct Footprint {
    fds: usize,
    threads: Option<u32>,
    children: Vec<i32>,
}

fn footprint() -> Footprint {
    Footprint {
        fds: diagnostics::open_fds().map_or(0, |fds| fds.len()),
        threads: diagnostics::thread_count(),
        children: diagnostics::child_pids(),
    }
}

/// The footprint once it holds still: a session's own thread ends a moment after its child
/// is reaped, and a baseline taken in between counted it (4 threads, then 3 after the churn).
fn settled_footprint() -> Footprint {
    let mut last = footprint();
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        let now = footprint();
        if now == last {
            break;
        }
        last = now;
    }
    last
}

#[expect(
    clippy::expect_used,
    reason = "test helper: a failed spawn fails the test"
)]
fn churn(cycles: usize) {
    let config = sh("exec /bin/sleep 600");
    for _ in 0..cycles {
        drop(Session::spawn(&config, |_| {}).expect("spawn"));
    }
}

#[test]
fn churn_should_return_fds_threads_and_children_to_baseline() {
    churn(1); // starts the reaper thread before measuring
    assert!(kil_term::wait_for_children(Duration::from_secs(5)));
    assert!(wait_until(Duration::from_secs(5), || footprint()
        .children
        .is_empty()));
    let baseline = settled_footprint();

    churn(500);

    assert!(
        kil_term::wait_for_children(Duration::from_secs(10)),
        "live: {}",
        kil_term::live_children()
    );
    let settled = wait_until(Duration::from_secs(5), || footprint() == baseline);
    assert!(settled, "baseline {baseline:?}, now {:?}", footprint());
}
