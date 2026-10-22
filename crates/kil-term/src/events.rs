//! Events from the PTY thread to the UI, coalesced (DESIGN.md §6.1).

use std::{
    mem,
    process::ExitStatus,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use alacritty_terminal::{
    event::{Event, EventListener},
    vte::ansi::Rgb,
};

use crate::{pane_loop::LoopSender, session::GridSize};

/// Something a session reports. Delivered on the session's PTY thread, so sinks must only
/// hand it off (e.g. push to a channel).
#[derive(Debug)]
pub enum SessionEvent {
    /// New output to draw. At most one is in flight until the next snapshot.
    Wakeup,
    /// The program set (`Some`) or reset (`None`) the window title.
    Title(Option<String>),
    /// The program rang the bell.
    Bell,
    /// The program asked to copy text (OSC 52). Reading the clipboard is never allowed.
    ClipboardStore(String),
    /// The program toggled cursor blinking.
    CursorBlinkingChange,
    /// The program posted a notification (OSC 9 or OSC 777).
    Notify(crate::notify::Notification),
    /// The child exited with this status.
    ChildExited(ExitStatus),
    /// The session is over: the child is gone or handed to the reaper. Always the last event.
    Ended,
}

/// A pending OSC 4/10/11 color query, answered by the pane loop once the parse that raised
/// it has released the terminal.
pub(crate) struct ColorQuery {
    pub(crate) index: usize,
    pub(crate) reply: Arc<dyn Fn(Rgb) -> String + Send + Sync>,
}

/// Whose terminal this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// Fed by the program's PTY: answers its queries.
    Local,
    /// A copy of a session that runs in the server, which answers instead: replies from
    /// here would reach the program twice (DESIGN.md §14.4).
    Mirror,
}

type Sink = Box<dyn Fn(SessionEvent) + Send + Sync>;

/// alacritty's listener for one session. Cheap to clone.
#[derive(Clone)]
pub(crate) struct EventProxy(Arc<Shared>);

struct Shared {
    sink: Sink,
    /// Set by the PTY thread on output (and by the UI when it scrolls, selects or resizes),
    /// cleared by the UI when it snapshots.
    dirty: AtomicBool,
    io: OnceLock<LoopSender>,
    size: Mutex<GridSize>,
    role: Role,
    colors: Mutex<Vec<ColorQuery>>,
    /// The last title the program set, for a client that attaches later.
    title: Mutex<Option<String>>,
}

impl EventProxy {
    pub(crate) fn new(sink: Sink, size: GridSize, role: Role) -> Self {
        Self(Arc::new(Shared {
            sink,
            dirty: AtomicBool::new(false),
            io: OnceLock::new(),
            size: Mutex::new(size),
            role,
            colors: Mutex::new(Vec::new()),
            title: Mutex::new(None),
        }))
    }

    /// Connects replies to the PTY; must happen before the pane loop starts.
    pub(crate) fn attach(&self, io: LoopSender) {
        let _ = self.0.io.set(io);
    }

    /// Color queries raised since the last call.
    pub(crate) fn take_color_queries(&self) -> Vec<ColorQuery> {
        mem::take(&mut *self.0.colors.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn emit(&self, event: SessionEvent) {
        (self.0.sink)(event);
    }

    pub(crate) fn clear_dirty(&self) {
        self.0.dirty.store(false, Ordering::Release);
    }

    /// The UI changed the viewport itself; its next snapshot must not be skipped.
    pub(crate) fn mark_dirty(&self) {
        self.0.dirty.store(true, Ordering::Release);
    }

    pub(crate) fn take_dirty(&self) -> bool {
        self.0.dirty.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.0.dirty.load(Ordering::Acquire)
    }

    pub(crate) fn set_size(&self, size: GridSize) {
        *self.0.size.lock().unwrap_or_else(PoisonError::into_inner) = size;
    }

    pub(crate) fn size(&self) -> GridSize {
        *self.0.size.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn title(&self) -> Option<String> {
        self.0
            .title
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_title(&self, title: Option<String>) {
        (*self.0.title.lock().unwrap_or_else(PoisonError::into_inner)).clone_from(&title);
        self.emit(SessionEvent::Title(title));
    }

    /// Answers the program directly from the PTY thread: no UI round trip, so programs that
    /// query the terminal at startup (vim, fzf) never wait for a frame.
    fn reply(&self, text: String) {
        if let Some(io) = self.0.io.get() {
            io.input(text.into_bytes().into());
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => {
                if !self.0.dirty.swap(true, Ordering::AcqRel) {
                    self.emit(SessionEvent::Wakeup);
                }
            }
            Event::PtyWrite(_) | Event::TextAreaSizeRequest(_) | Event::ColorRequest(..)
                if self.0.role == Role::Mirror => {}
            Event::PtyWrite(text) => self.reply(text),
            Event::TextAreaSizeRequest(format) => self.reply(format(self.size().into())),
            Event::ColorRequest(index, reply) => self
                .0
                .colors
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(ColorQuery { index, reply }),
            Event::Title(title) => self.set_title(Some(title)),
            Event::ResetTitle => self.set_title(None),
            Event::Bell => self.emit(SessionEvent::Bell),
            Event::ClipboardStore(_, text) => self.emit(SessionEvent::ClipboardStore(text)),
            Event::CursorBlinkingChange => self.emit(SessionEvent::CursorBlinkingChange),
            Event::ChildExit(status) => self.emit(SessionEvent::ChildExited(status)),
            // `Ended` comes from the PTY's drop, which also covers loops that stop on I/O
            // errors without alacritty's `Exit`. Clipboard reads are denied (DESIGN.md §5.2).
            Event::Exit | Event::ClipboardLoad(..) | Event::MouseCursorDirty => {}
        }
    }
}
