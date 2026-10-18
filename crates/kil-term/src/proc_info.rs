//! Process identity and foreground-job queries (DESIGN.md §4.3).

use std::{ffi::OsString, num::NonZeroI32, path::PathBuf};

use crate::sys;

/// A process ID; never zero or negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pid(NonZeroI32);

impl Pid {
    /// Wraps a raw PID, rejecting zero and negative values (which address groups in `kill`).
    #[must_use]
    pub fn new(raw: i32) -> Option<Self> {
        NonZeroI32::new(raw).filter(|pid| pid.get() > 0).map(Self)
    }

    /// The raw PID.
    #[must_use]
    pub fn get(self) -> i32 {
        self.0.get()
    }
}

/// Device number of a PTY; processes it controls report it as their `e_tdev`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TtyId(pub(crate) u32);

/// When a process was forked. With the PID it names one process for its whole life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct StartTime {
    sec: u64,
    usec: u64,
}

impl StartTime {
    fn of(info: &sys::BsdInfo) -> Self {
        Self {
            sec: info.start_sec,
            usec: info.start_usec,
        }
    }
}

/// A PID pinned to one process: a recycled PID has a different start time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcIdentity {
    pid: Pid,
    start: StartTime,
}

impl ProcIdentity {
    /// Records the identity of the process currently holding `pid`.
    #[must_use]
    pub fn capture(pid: Pid) -> Option<Self> {
        let info = sys::bsd_info(pid.get())?;
        Some(Self {
            pid,
            start: StartTime::of(&info),
        })
    }

    /// The PID.
    #[must_use]
    pub fn pid(self) -> Pid {
        self.pid
    }

    /// An identity captured by another process (the session server).
    pub(crate) fn from_parts(pid: Pid, sec: u64, usec: u64) -> Self {
        Self {
            pid,
            start: StartTime { sec, usec },
        }
    }

    pub(crate) fn start_parts(self) -> (u64, u64) {
        (self.start.sec, self.start.usec)
    }

    /// Whether this exact process is still running (not exited, not recycled).
    #[must_use]
    pub fn is_alive(self) -> bool {
        self.info().is_some_and(|info| !info.zombie)
    }

    /// Current info, if `pid` still names this process.
    fn info(self) -> Option<sys::BsdInfo> {
        sys::bsd_info(self.pid.get()).filter(|info| StartTime::of(info) == self.start)
    }
}

/// The process group leader in the foreground of a session's terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForegroundProcess {
    /// Leader of the foreground process group.
    pub pid: Pid,
    /// Executable name; empty if the leader already exited while its group lives on.
    pub name: String,
    /// Working directory of the leader, when readable.
    pub cwd: Option<PathBuf>,
    /// True when the shell itself is in the foreground, i.e. the session is idle.
    pub is_shell: bool,
}

/// Finds the foreground process of `shell`'s terminal.
///
/// Every PID is verified before use: the shell by start time, the foreground leader by
/// controlling terminal. A recycled PID therefore yields `None`, never a stranger.
pub(crate) fn foreground(shell: ProcIdentity, tty: TtyId) -> Option<ForegroundProcess> {
    let info = shell
        .info()
        .filter(|info| !info.zombie && info.tdev == tty.0)?;
    if info.tpgid <= 0 || info.tpgid == info.pgid {
        return Some(ForegroundProcess {
            pid: shell.pid,
            cwd: sys::cwd(shell.pid.get()),
            name: info.name,
            is_shell: true,
        });
    }
    let pid = Pid::new(info.tpgid)?;
    let Some(leader) = sys::bsd_info(pid.get()) else {
        // A pipeline whose leader exited: the group is still busy, its name unknown.
        return Some(ForegroundProcess {
            pid,
            name: String::new(),
            cwd: None,
            is_shell: false,
        });
    };
    (leader.tdev == tty.0).then(|| ForegroundProcess {
        pid,
        cwd: sys::cwd(pid.get()),
        name: leader.name,
        is_shell: false,
    })
}

/// Arguments of the foreground job, `argv[0]` first. `None` while the shell itself is in the
/// foreground, or when the job's arguments can't be read (another user's process, e.g. `sudo`).
pub(crate) fn job_args(shell: ProcIdentity, tty: TtyId) -> Option<Vec<OsString>> {
    let job = foreground(shell, tty).filter(|job| !job.is_shell)?;
    sys::proc_args(job.pid.get())
}

/// Working directory of the shell itself, wherever its foreground job runs.
pub(crate) fn shell_cwd(shell: ProcIdentity) -> Option<PathBuf> {
    shell.info().filter(|info| !info.zombie)?;
    sys::cwd(shell.pid.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Pid {
        Pid::new(i32::try_from(std::process::id()).unwrap()).unwrap()
    }

    #[test]
    fn pid_new_should_reject_zero_and_negative() {
        assert_eq!((Pid::new(0), Pid::new(-1)), (None, None));
    }

    #[test]
    fn is_alive_should_be_true_for_current_process() {
        assert!(ProcIdentity::capture(me()).unwrap().is_alive());
    }

    #[test]
    fn is_alive_should_be_false_when_pid_was_recycled() {
        let mut identity = ProcIdentity::capture(me()).unwrap();
        identity.start.usec += 1;
        assert!(!identity.is_alive());
    }

    #[test]
    fn is_alive_should_be_false_after_child_is_reaped() {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = Pid::new(i32::try_from(child.id()).unwrap()).unwrap();
        let identity = ProcIdentity::capture(pid);
        child.wait().unwrap();
        assert!(!identity.is_some_and(ProcIdentity::is_alive));
    }
}
