//! Terminal sessions for kil: PTY spawn, process lifecycle, VT state and render snapshots.
//!
//! A session runs in this process, or in the session server (`kil --server`, [`server`]),
//! which keeps it running while no app is attached; the app reaches it through a [`Client`]
//! (DESIGN.md §14).
//!
//! Process invariants (DESIGN.md §4):
//! - A child PID is only signalled through the value that owns the unreaped child.
//! - Teardown closes the PTY master before anything waits, and never blocks the caller.
//! - No signal handlers are installed; child exits arrive through kqueue.
#![deny(missing_docs)]

mod child;
mod error;
mod events;
mod pane_loop;
mod proc_info;
mod pty;
mod reaper;
mod remote;
pub mod server;
mod session;
mod shell;
mod snapshot;
mod spawn;
mod state;
mod sys;
mod transcript;
mod wire;

pub use child::{live_children, wait_for_children};
pub use error::SpawnError;
pub use events::SessionEvent;
pub use proc_info::{ForegroundProcess, Job, Pid, ProcIdentity, TtyId};
pub use reaper::expedite_reaping;
pub use remote::Client;
pub use session::{
    GridSize, InputMode, MemoryUsage, Program, SelectionKind, Session, SessionConfig, ViewportCell,
};
pub use snapshot::{CellFlags, CursorShape, CursorSnapshot, Frame, Palette, RenderCell, Rgba};
pub use transcript::Transcript;

/// Self-inspection of the current process, for tests and the memory HUD.
pub mod diagnostics {
    use std::{ffi::c_int, io, os::fd::RawFd};

    use crate::{proc_info::Pid, sys};

    fn me() -> i32 {
        i32::try_from(std::process::id()).unwrap_or(i32::MAX)
    }

    /// Descriptors open in this process.
    ///
    /// # Errors
    /// If the kernel refuses the query.
    pub fn open_fds() -> io::Result<Vec<RawFd>> {
        sys::open_fds(me())
    }

    /// Descriptors open in `pid` (same user only).
    ///
    /// # Errors
    /// If `pid` is gone or belongs to another user.
    pub fn open_fds_of(pid: Pid) -> io::Result<Vec<RawFd>> {
        sys::open_fds(pid.get())
    }

    /// Threads in this process.
    #[must_use]
    pub fn thread_count() -> Option<u32> {
        sys::thread_count(me())
    }

    /// Children of this process, including unreaped zombies.
    #[must_use]
    pub fn child_pids() -> Vec<i32> {
        sys::child_pids(me())
    }

    /// Whether `pid` exists at all, zombie included (i.e. it has not been reaped).
    #[must_use]
    pub fn exists(pid: Pid) -> bool {
        sys::bsd_info(pid.get()).is_some()
    }

    /// Parent of `pid`.
    #[must_use]
    pub fn parent_of(pid: Pid) -> Option<i32> {
        sys::bsd_info(pid.get()).map(|info| info.ppid)
    }

    /// Whether `signal` still has its default disposition in this process.
    #[must_use]
    pub fn signal_is_default(signal: c_int) -> bool {
        sys::signal_is_default(signal)
    }
}
