//! Process-wide owner of children whose sessions closed (DESIGN.md §4.5).
//!
//! Closing never blocks the caller: the child is adopted here, reaped when kqueue reports
//! its exit, and `SIGKILL`ed if it outlives its grace period.

use std::{
    collections::HashMap,
    io, mem,
    sync::{
        Arc, OnceLock,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use polling::{Events, Poller};

use crate::{
    child::{OwnedChild, Reap},
    sys,
};

/// Time a closed session's child gets to exit after hangup before `SIGKILL`.
pub(crate) const CLOSE_GRACE: Duration = Duration::from_secs(3);
/// Re-check interval after `SIGKILL`; logs if a child still has not exited.
const KILL_WAIT: Duration = Duration::from_secs(2);
/// Fallback sweep in case a kqueue registration failed (e.g. the child exited first).
const SWEEP: Duration = Duration::from_millis(250);

enum Command {
    Adopt { child: OwnedChild, grace: Duration },
    Expedite,
}

pub(crate) struct Reaper {
    tx: Sender<Command>,
    poller: Option<Arc<Poller>>,
}

impl Reaper {
    /// The process-wide reaper, started on first use.
    pub(crate) fn get() -> &'static Self {
        static REAPER: OnceLock<Reaper> = OnceLock::new();
        REAPER.get_or_init(Self::start)
    }

    fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let poller = Poller::new()
            .inspect_err(|err| log::error!("reaper: kqueue unavailable ({err}); sweeping only"))
            .ok()
            .map(Arc::new);
        let worker = poller.clone();
        if let Err(err) = thread::Builder::new()
            .name("kil-reaper".into())
            .spawn(move || run(&rx, worker.as_deref()))
        {
            log::error!("reaper: failed to start thread ({err}); launchd reaps at exit");
        }
        Self { tx, poller }
    }

    /// Takes ownership of `child`, which must already have been hung up.
    pub(crate) fn adopt(&self, child: OwnedChild, grace: Duration) {
        self.send(Command::Adopt { child, grace });
    }

    fn send(&self, command: Command) {
        if self.tx.send(command).is_err() {
            log::error!("reaper thread is gone; launchd reaps at exit");
        }
        if let Some(poller) = &self.poller {
            let _ = poller.notify();
        }
    }
}

/// Ends every pending grace period: remaining closed-session children get `SIGKILL` now.
pub fn expedite_reaping() {
    Reaper::get().send(Command::Expedite);
}

struct Ward {
    child: OwnedChild,
    deadline: Instant,
    killed: bool,
}

fn key(child: &OwnedChild) -> usize {
    usize::try_from(child.pid().get()).unwrap_or_default()
}

fn run(rx: &Receiver<Command>, poller: Option<&Poller>) {
    let mut wards: HashMap<usize, Ward> = HashMap::new();
    let mut events = Events::new();
    loop {
        if !receive(rx, poller, &mut wards) && wards.is_empty() {
            return;
        }
        let now = Instant::now();
        for (key, ward) in mem::take(&mut wards) {
            if let Some(ward) = reap_or_escalate(ward, poller, now) {
                wards.insert(key, ward);
            }
        }
        let timeout = wards
            .values()
            .map(|ward| ward.deadline.saturating_duration_since(now).min(SWEEP))
            .min();
        let Some(poller) = poller else {
            thread::sleep(timeout.unwrap_or(SWEEP));
            continue;
        };
        events.clear();
        if let Err(err) = poller.wait(&mut events, timeout)
            && err.kind() != io::ErrorKind::Interrupted
        {
            log::error!("reaper: kqueue wait failed: {err}");
            thread::sleep(SWEEP);
        }
    }
}

/// Drains commands; returns `false` once every sender is gone.
fn receive(
    rx: &Receiver<Command>,
    poller: Option<&Poller>,
    wards: &mut HashMap<usize, Ward>,
) -> bool {
    loop {
        match rx.try_recv() {
            Ok(Command::Adopt { child, grace }) => {
                if let Some(poller) = poller {
                    // Failure means the child already exited; the sweep reaps it.
                    let _ = sys::watch_exit(poller, child.as_std(), key(&child));
                }
                let deadline = Instant::now() + grace;
                wards.insert(
                    key(&child),
                    Ward {
                        child,
                        deadline,
                        killed: false,
                    },
                );
            }
            Ok(Command::Expedite) => {
                let now = Instant::now();
                for ward in wards.values_mut().filter(|ward| !ward.killed) {
                    ward.deadline = now;
                }
            }
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

fn reap_or_escalate(ward: Ward, poller: Option<&Poller>, now: Instant) -> Option<Ward> {
    let Ward {
        child,
        deadline,
        killed,
    } = ward;
    let child = match child.try_reap() {
        Reap::Exited(status) => {
            log::debug!("reaper: child exited with {status}");
            return None;
        }
        Reap::Running(child) => child,
    };
    if now < deadline {
        return Some(Ward {
            child,
            deadline,
            killed,
        });
    }
    if killed {
        log::error!(
            "reaper: pid {} survived SIGKILL; still waiting",
            child.pid().get()
        );
    } else if let Err(err) = child.signal(libc::SIGKILL) {
        log::error!("reaper: SIGKILL pid {} failed: {err}", child.pid().get());
    }
    if let Some(poller) = poller {
        // Re-arm: a one-shot filter consumed by an earlier wakeup would otherwise leave
        // only the sweep to notice the exit.
        sys::unwatch_exit(poller, child.as_std());
        let _ = sys::watch_exit(poller, child.as_std(), key(&child));
    }
    Some(Ward {
        child,
        deadline: now + KILL_WAIT,
        killed: true,
    })
}
