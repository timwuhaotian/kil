//! Events from the PTY thread to the UI, coalesced (DESIGN.md §6.1).

use std::{
    fmt,
    process::ExitStatus,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use alacritty_terminal::{
    event::{Event, EventListener},
    event_loop::{EventLoopSender, Msg},
    vte::ansi::Rgb,
};

use crate::session::GridSize;

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
    /// The program asked for a color; answer with [`crate::Session::answer_color`].
    ColorRequest(ColorQuery),
    /// The program toggled cursor blinking.
    CursorBlinkingChange,
    /// The child exited with this status.
    ChildExited(ExitStatus),
    /// The session is over: the child is gone or handed to the reaper. Always the last event.
    Ended,
}

/// A pending OSC 4/10/11 color query.
pub struct ColorQuery {
    pub(crate) index: usize,
    pub(crate) reply: Arc<dyn Fn(Rgb) -> String + Send + Sync>,
}

impl fmt::Debug for ColorQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ColorQuery")
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
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
    io: OnceLock<EventLoopSender>,
    size: Mutex<GridSize>,
}

impl EventProxy {
    pub(crate) fn new(sink: Sink, size: GridSize) -> Self {
        Self(Arc::new(Shared {
            sink,
            dirty: AtomicBool::new(false),
            io: OnceLock::new(),
            size: Mutex::new(size),
        }))
    }

    /// Connects replies to the PTY; must happen before the event loop starts.
    pub(crate) fn attach(&self, io: EventLoopSender) {
        let _ = self.0.io.set(io);
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

    /// Answers the program directly from the PTY thread: no UI round trip, so programs that
    /// query the terminal at startup (vim, fzf) never wait for a frame.
    fn reply(&self, text: String) {
        if let Some(io) = self.0.io.get() {
            let _ = io.send(Msg::Input(text.into_bytes().into()));
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
            Event::PtyWrite(text) => self.reply(text),
            Event::TextAreaSizeRequest(format) => {
                let size = *self.0.size.lock().unwrap_or_else(PoisonError::into_inner);
                self.reply(format(size.into()));
            }
            Event::Title(title) => self.emit(SessionEvent::Title(Some(title))),
            Event::ResetTitle => self.emit(SessionEvent::Title(None)),
            Event::Bell => self.emit(SessionEvent::Bell),
            Event::ClipboardStore(_, text) => self.emit(SessionEvent::ClipboardStore(text)),
            Event::ColorRequest(index, reply) => {
                self.emit(SessionEvent::ColorRequest(ColorQuery { index, reply }));
            }
            Event::CursorBlinkingChange => self.emit(SessionEvent::CursorBlinkingChange),
            Event::ChildExit(status) => self.emit(SessionEvent::ChildExited(status)),
            // `Ended` comes from the PTY's drop, which also covers loops that stop on I/O
            // errors without alacritty's `Exit`. Clipboard reads are denied (DESIGN.md §5.2).
            Event::Exit | Event::ClipboardLoad(..) | Event::MouseCursorDirty => {}
        }
    }
}
