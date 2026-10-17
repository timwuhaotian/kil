//! Our PTY behind alacritty's event loop: its parser and I/O loop, our process lifecycle.

use std::{fs::File, io, os::fd::AsFd, sync::Arc};

use alacritty_terminal::{
    event::{OnResize, WindowSize},
    tty::{ChildEvent, EventedPty, EventedReadWrite},
};
use polling::{Event, PollMode, Poller};

use crate::{
    child::{OwnedChild, Reap},
    events::{EventProxy, SessionEvent},
    reaper::{CLOSE_GRACE, Reaper},
    sys,
};

/// Keys alacritty's event loop dispatches on. They mirror `alacritty_terminal`'s
/// crate-private `PTY_READ_WRITE_TOKEN` / `PTY_CHILD_EVENT_TOKEN` (pinned at `=0.26.0`).
const READ_WRITE_TOKEN: usize = 0;
const CHILD_EVENT_TOKEN: usize = 1;

/// The master side of a session's terminal plus the child it runs.
///
/// Field order is load-bearing: fields drop in declaration order, so `master` closes
/// (hanging up the session) before `child` is signalled and handed to the reaper. Waiting
/// with the master still open can deadlock forever (DESIGN.md §4.1 F2, F3).
pub(crate) struct Pty {
    master: File,
    child: ChildGuard,
}

impl Pty {
    pub(crate) fn new(master: File, child: OwnedChild, events: EventProxy) -> Self {
        Self {
            master,
            child: ChildGuard {
                child: Some(child),
                events,
            },
        }
    }
}

/// On drop: reports an exit the event loop missed, hands a running child to the reaper,
/// then announces [`SessionEvent::Ended`].
struct ChildGuard {
    child: Option<OwnedChild>,
    events: EventProxy,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            match child.try_reap() {
                Reap::Exited(status) => self.events.emit(SessionEvent::ChildExited(status)),
                Reap::Running(child) => {
                    // The master is already closed (see `Pty`), which normally signals the
                    // session leader; this SIGHUP does not rely on the tty driver doing so.
                    let _ = child.signal(libc::SIGHUP);
                    Reaper::get().adopt(child, CLOSE_GRACE);
                }
            }
        }
        self.events.emit(SessionEvent::Ended);
    }
}

impl EventedReadWrite for Pty {
    type Reader = File;
    type Writer = File;

    #[expect(
        unsafe_code,
        reason = "alacritty's trait method is unsafe: sources must outlive registration; \
                  both live in `self` and the event loop deregisters before dropping it"
    )]
    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        interest.key = READ_WRITE_TOKEN;
        // SAFETY: `self.master` outlives the registration; see the method's `expect`.
        unsafe { poll.add_with_mode(&self.master, interest, mode)? };
        if let Some(child) = &self.child.child {
            sys::watch_exit(poll, child.as_std(), CHILD_EVENT_TOKEN)?;
        }
        Ok(())
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        interest.key = READ_WRITE_TOKEN;
        poll.modify_with_mode(&self.master, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        if let Some(child) = &self.child.child {
            sys::unwatch_exit(poll, child.as_std());
        }
        poll.delete(&self.master)
    }

    fn reader(&mut self) -> &mut File {
        &mut self.master
    }

    fn writer(&mut self) -> &mut File {
        &mut self.master
    }
}

impl EventedPty for Pty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        match self.child.child.take()?.try_reap() {
            Reap::Exited(status) => Some(ChildEvent::Exited(Some(status))),
            Reap::Running(child) => {
                self.child.child = Some(child);
                None
            }
        }
    }
}

impl OnResize for Pty {
    fn on_resize(&mut self, size: WindowSize) {
        // ENOTTY after hangup is expected; alacritty's own impl exits the process here.
        if let Err(err) = sys::set_window_size(self.master.as_fd(), winsize(size)) {
            log::debug!("TIOCSWINSZ failed: {err}");
        }
    }
}

pub(crate) fn winsize(size: WindowSize) -> libc::winsize {
    libc::winsize {
        ws_row: size.num_lines,
        ws_col: size.num_cols,
        ws_xpixel: size.num_cols.saturating_mul(size.cell_width),
        ws_ypixel: size.num_lines.saturating_mul(size.cell_height),
    }
}
