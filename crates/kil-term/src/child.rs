//! The one owner of a spawned child (DESIGN.md §4.4).

use std::{
    ffi::c_int,
    io,
    process::{Child, ExitStatus},
    sync::{Condvar, Mutex, PoisonError},
    time::{Duration, Instant},
};

use crate::{proc_info::Pid, sys};

/// Children spawned and not yet reaped, process-wide.
static LIVE: Live = Live {
    count: Mutex::new(0),
    changed: Condvar::new(),
};

struct Live {
    count: Mutex<usize>,
    changed: Condvar,
}

impl Live {
    fn add(&self, delta: isize) {
        let mut count = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        *count = count.saturating_add_signed(delta);
        self.changed.notify_all();
    }
}

/// Number of children spawned by sessions and not yet reaped.
#[must_use]
pub fn live_children() -> usize {
    *LIVE.count.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Blocks until every session child has been reaped, or `timeout` passes.
///
/// Returns whether all were reaped. Used on quit and by tests.
#[must_use]
pub fn wait_for_children(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut count = LIVE.count.lock().unwrap_or_else(PoisonError::into_inner);
    while *count > 0 {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        count = LIVE
            .changed
            .wait_timeout(count, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    true
}

/// A spawned, unreaped child process.
///
/// Not `Clone`: owning it is the permission to signal or wait on its PID. An unreaped
/// child's PID cannot be recycled, and reaping consumes the value, so no code path can
/// signal a stranger through it.
#[derive(Debug)]
pub(crate) struct OwnedChild {
    inner: Child,
    pid: Pid,
}

/// Result of a non-blocking reap.
pub(crate) enum Reap {
    Exited(ExitStatus),
    Running(OwnedChild),
}

impl OwnedChild {
    pub(crate) fn new(inner: Child) -> io::Result<Self> {
        let pid = i32::try_from(inner.id())
            .ok()
            .and_then(Pid::new)
            .ok_or_else(|| {
                io::Error::other(format!("spawn returned an invalid pid {}", inner.id()))
            })?;
        LIVE.add(1);
        Ok(Self { inner, pid })
    }

    pub(crate) fn pid(&self) -> Pid {
        self.pid
    }

    pub(crate) fn as_std(&self) -> &Child {
        &self.inner
    }

    /// Sends `signal`. Sound because the child is unreaped, so its PID is still ours.
    pub(crate) fn signal(&self, signal: c_int) -> io::Result<()> {
        sys::kill(self.pid.get(), signal)
    }

    /// Reaps the child if it has exited; otherwise hands it back.
    pub(crate) fn try_reap(mut self) -> Reap {
        match self.inner.try_wait() {
            Ok(Some(status)) => {
                LIVE.add(-1);
                Reap::Exited(status)
            }
            Ok(None) => Reap::Running(self),
            Err(err) => {
                // Only possible if someone else reaped our child, which ownership forbids.
                log::error!("waitpid({}) failed: {err}", self.pid.get());
                Reap::Running(self)
            }
        }
    }
}
