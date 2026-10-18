//! A terminal session: one PTY, one child, one VT state (DESIGN.md §3).

use std::{
    borrow::Cow, ffi::OsString, mem::size_of, os::unix::net::UnixStream, path::PathBuf, sync::Arc,
};

use alacritty_terminal::{
    event::WindowSize,
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config as TermConfig, Osc52, Term, TermMode, cell::Cell},
};

use crate::{
    error::SpawnError,
    events::{EventProxy, Role, SessionEvent},
    pane_loop::{self, Channel, Command, LoopSender},
    proc_info::{self, ForegroundProcess, Job, Pid, ProcIdentity, TtyId},
    pty::Pty,
    remote::Remote,
    snapshot::{Frame, Palette},
    spawn::{self, Spawned},
    transcript::{self, Transcript},
    wire::{self, Identity},
};

/// Grid size in cells plus one cell's pixel size (reported to programs via `TIOCSWINSZ`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    /// Visible rows; at least 1.
    pub rows: u16,
    /// Columns; at least 1.
    pub cols: u16,
    /// Cell width in pixels.
    pub cell_width: u16,
    /// Cell height in pixels.
    pub cell_height: u16,
}

impl GridSize {
    pub(crate) fn clamped(self) -> Self {
        Self {
            rows: self.rows.max(1),
            cols: self.cols.max(1),
            ..self
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

impl From<GridSize> for WindowSize {
    fn from(size: GridSize) -> Self {
        Self {
            num_lines: size.rows,
            num_cols: size.cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}

/// What a session runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Program {
    /// The user's login shell (`$SHELL`, else the password database, else `/bin/zsh`).
    #[default]
    LoginShell,
    /// A specific program.
    Command {
        /// Absolute path of the executable.
        program: PathBuf,
        /// Arguments after `argv[0]`.
        args: Vec<OsString>,
    },
}

/// How to start a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    /// What to run.
    pub program: Program,
    /// Working directory; falls back to `$HOME` if missing.
    pub cwd: Option<PathBuf>,
    /// Initial grid size.
    pub size: GridSize,
    /// Scrollback limit; each line costs `cols × 24` bytes (DESIGN.md §5).
    pub scrollback_lines: usize,
    /// Extra environment, applied last.
    pub env: Vec<(OsString, OsString)>,
    /// Painted into the terminal before the program starts, e.g. a [`Transcript`] of an
    /// earlier launch.
    pub replay: Vec<u8>,
    /// Colors reported to programs that ask (OSC 4/10/11), e.g. to tell a light theme.
    pub palette: Palette,
}

impl SessionConfig {
    /// The user's login shell in `$HOME` with 10,000 lines of scrollback.
    #[must_use]
    pub fn new(size: GridSize) -> Self {
        Self {
            program: Program::LoginShell,
            cwd: None,
            size,
            scrollback_lines: 10_000,
            env: Vec::new(),
            replay: Vec::new(),
            palette: Palette::default(),
        }
    }
}

/// Terminal modes that change how input must be encoded. `Default` is a fresh terminal's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputMode(TermMode);

impl InputMode {
    /// DECCKM: arrows send `ESC O x` instead of `ESC [ x`.
    #[must_use]
    pub fn app_cursor(self) -> bool {
        self.0.contains(TermMode::APP_CURSOR)
    }

    /// DECKPAM: keypad sends application sequences.
    #[must_use]
    pub fn app_keypad(self) -> bool {
        self.0.contains(TermMode::APP_KEYPAD)
    }

    /// Pastes must be wrapped in `ESC [200~` … `ESC [201~`.
    #[must_use]
    pub fn bracketed_paste(self) -> bool {
        self.0.contains(TermMode::BRACKETED_PASTE)
    }

    /// The alternate screen is active (full-screen programs).
    #[must_use]
    pub fn alt_screen(self) -> bool {
        self.0.contains(TermMode::ALT_SCREEN)
    }

    /// In the alternate screen, the wheel should send arrow keys.
    #[must_use]
    pub fn alternate_scroll(self) -> bool {
        self.0.contains(TermMode::ALTERNATE_SCROLL)
    }

    /// The program wants mouse events.
    #[must_use]
    pub fn mouse_reporting(self) -> bool {
        self.0.intersects(TermMode::MOUSE_MODE)
    }
}

/// How a mouse selection grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Cell by cell (drag).
    Cells,
    /// Whole words (double-click).
    Words,
    /// Whole lines (triple-click).
    Lines,
}

/// A cell in the viewport, plus which half of it the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportCell {
    /// Viewport row.
    pub row: usize,
    /// Column.
    pub col: usize,
    /// Pointer is over the right half of the cell.
    pub right_half: bool,
}

/// Memory held by a session's grid (DESIGN.md §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryUsage {
    /// Lines of scrollback currently stored.
    pub history_lines: usize,
    /// Grid width.
    pub columns: usize,
    /// Bytes of cell storage: `(history + visible) × columns × size_of::<Cell>()`.
    pub grid_bytes: usize,
}

/// A running terminal: a child on a PTY, parsed by alacritty's VT state machine.
///
/// The PTY is either in this process ([`Session::spawn`]) or in the session server
/// ([`crate::Client`]), with a mirror of its terminal here (DESIGN.md §14.4). Either way the
/// terminal is read the same.
///
/// Dropping a local session closes it without blocking: its thread hangs up the child and
/// the reaper finishes it (DESIGN.md §4.5). Dropping a remote one only detaches from it: the
/// program keeps running in the server. [`Session::close`] ends either.
pub struct Session {
    term: Arc<FairMutex<Term<EventProxy>>>,
    link: Link,
    events: EventProxy,
    pid: Pid,
    shell: Option<ProcIdentity>,
    tty: TtyId,
}

enum Link {
    Local(LoopSender),
    Remote(Remote),
}

pub(crate) fn term_config(scrollback_lines: usize) -> TermConfig {
    TermConfig {
        scrolling_history: scrollback_lines,
        osc52: Osc52::OnlyCopy,
        kitty_keyboard: false,
        ..TermConfig::default()
    }
}

impl Session {
    /// Starts `config.program` on a new PTY. `sink` receives events on the PTY thread.
    ///
    /// # Errors
    /// When the PTY, the child or the event loop cannot be created.
    pub fn spawn(
        config: &SessionConfig,
        sink: impl Fn(SessionEvent) + Send + Sync + 'static,
    ) -> Result<Self, SpawnError> {
        let size = config.size.clamped();
        let channel = Channel::new().map_err(SpawnError::EventLoop)?;
        let Spawned { master, child, tty } = spawn::spawn(config)?;
        let pid = child.pid();
        let shell = ProcIdentity::capture(pid);
        let events = EventProxy::new(Box::new(sink), size, Role::Local);
        events.attach(channel.sender());
        let mut term = Term::new(term_config(config.scrollback_lines), &size, events.clone());
        // Before the PTY thread starts, so the program's first output lands after it.
        transcript::replay(&mut term, &config.replay);
        let term = Arc::new(FairMutex::new(term));
        let pty = Pty::new(master, child, events.clone());
        let io = channel.sender();
        // Detached on purpose: the thread ends on Shutdown or child exit and drops the PTY
        // itself, so nothing ever joins (and blocks on) it.
        pane_loop::spawn(
            channel,
            Arc::clone(&term),
            events.clone(),
            pty,
            config.palette.clone(),
        )
        .map_err(SpawnError::EventLoop)?;
        Ok(Self {
            term,
            link: Link::Local(io),
            events,
            pid,
            shell,
            tty,
        })
    }

    /// A mirror of a session in the server: `remote` feeds `term` from the connection.
    pub(crate) fn mirror(
        identity: Identity,
        term: Arc<FairMutex<Term<EventProxy>>>,
        events: EventProxy,
        remote: Remote,
    ) -> Option<Self> {
        let pid = Pid::new(identity.pid)?;
        Some(Self {
            term,
            link: Link::Remote(remote),
            events,
            pid,
            shell: Some(ProcIdentity::from_parts(
                pid,
                identity.start_sec,
                identity.start_usec,
            )),
            tty: TtyId(identity.tty),
        })
    }

    /// Colors reported to programs that ask from now on, e.g. after the theme changed. A
    /// server older than protocol 2 keeps the palette the session started with.
    pub fn set_palette(&self, palette: &Palette) {
        match &self.link {
            Link::Local(io) => io.send(Command::Palette(palette.clone())),
            Link::Remote(remote) => remote.send_palette(palette),
        }
    }

    /// Ends the session: hangs the program up, wherever it runs.
    pub fn close(self) {
        if let Link::Remote(remote) = &self.link {
            remote.send(wire::Frame::Close);
        }
    }

    /// Where the session's process is, for a client attaching to it.
    pub(crate) fn identity(&self) -> Identity {
        let (sec, usec) = self.shell.map_or((0, 0), ProcIdentity::start_parts);
        Identity {
            pid: self.pid.get(),
            start_sec: sec,
            start_usec: usec,
            tty: self.tty.0,
            size: self.events.size(),
        }
    }

    /// Gives `stream` this session: its whole state, then its output (DESIGN.md §14.5).
    pub(crate) fn attach(&self, stream: UnixStream) {
        if let Link::Local(io) = &self.link {
            io.send(Command::Attach(stream));
        }
    }

    /// PID of the child (the shell).
    #[must_use]
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Sends input bytes to the program.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        match &self.link {
            Link::Local(io) => io.input(bytes.into()),
            Link::Remote(remote) => remote.send(wire::Frame::Input(bytes.into().into_owned())),
        }
    }

    /// Resizes the grid and the PTY; the program receives one `SIGWINCH`.
    pub fn resize(&self, size: GridSize) {
        let size = size.clamped();
        self.events.set_size(size);
        self.term.lock().resize(size);
        self.events.mark_dirty();
        match &self.link {
            Link::Local(io) => io.send(Command::Resize(size.into())),
            Link::Remote(remote) => remote.send(wire::Frame::Resize(size)),
        }
    }

    /// Whether the viewport changed since the last snapshot: new output, or a resize, scroll
    /// or selection made through this session. A clean session's last snapshot is current.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.events.is_dirty()
    }

    /// Clears [`Self::is_dirty`] without a snapshot and returns what it was.
    ///
    /// For a session nobody is drawing (a background tab): a dirty session sends no further
    /// [`SessionEvent::Wakeup`], so without this its later output would go unnoticed.
    #[must_use]
    pub fn take_dirty(&self) -> bool {
        self.events.take_dirty()
    }

    /// Copies the viewport into `frame`, reusing its allocation.
    pub fn snapshot_into(&self, frame: &mut Frame, palette: &Palette) {
        // Clear first: output racing with the copy sets it again and sends a new Wakeup.
        self.events.clear_dirty();
        frame.fill(&self.term.lock(), palette);
    }

    /// Input encoding modes currently set by the program.
    #[must_use]
    pub fn input_mode(&self) -> InputMode {
        InputMode(*self.term.lock().mode())
    }

    /// Scrolls the viewport by `lines` (positive = back in history).
    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
        self.events.mark_dirty();
    }

    /// Returns the viewport to the live screen.
    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
        self.events.mark_dirty();
    }

    /// Drops all scrollback. The allocator keeps the pages for reuse (DESIGN.md §5.1).
    pub fn clear_history(&self) {
        self.term.lock().grid_mut().clear_history();
        self.events.mark_dirty();
    }

    /// Starts a selection at `cell`, replacing any previous one.
    pub fn start_selection(&self, kind: SelectionKind, cell: ViewportCell) {
        let ty = match kind {
            SelectionKind::Cells => SelectionType::Simple,
            SelectionKind::Words => SelectionType::Semantic,
            SelectionKind::Lines => SelectionType::Lines,
        };
        let mut term = self.term.lock();
        let (point, side) = grid_point(&term, cell);
        term.selection = Some(Selection::new(ty, point, side));
        self.events.mark_dirty();
    }

    /// Extends the current selection to `cell`.
    pub fn update_selection(&self, cell: ViewportCell) {
        let mut term = self.term.lock();
        let (point, side) = grid_point(&term, cell);
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
        self.events.mark_dirty();
    }

    /// Removes the selection.
    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
        self.events.mark_dirty();
    }

    /// Selected text, if any.
    #[must_use]
    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    /// Grid memory currently held.
    #[must_use]
    pub fn memory(&self) -> MemoryUsage {
        let term = self.term.lock();
        let (history_lines, columns) = (term.history_size(), term.columns());
        let grid_bytes = (history_lines + term.screen_lines()) * columns * size_of::<Cell>();
        MemoryUsage {
            history_lines,
            columns,
            grid_bytes,
        }
    }

    /// The process in the foreground of this terminal, verified against PID reuse.
    #[must_use]
    pub fn foreground(&self) -> Option<ForegroundProcess> {
        proc_info::foreground(self.shell?, self.tty)
    }

    /// The terminal's content, to save for a later launch.
    #[must_use]
    pub fn transcript(&self) -> Transcript {
        Transcript::new(Arc::clone(&self.term))
    }

    /// Working directory of the shell itself, even while its job runs elsewhere.
    #[must_use]
    pub fn shell_cwd(&self) -> Option<PathBuf> {
        proc_info::shell_cwd(self.shell?)
    }

    /// The foreground job, verified like [`Self::foreground`]. `None` while the shell is idle.
    #[must_use]
    pub fn job(&self) -> Option<Job> {
        proc_info::job(self.shell?, self.tty)
    }

    /// Whether a job other than the shell is in the foreground (closing should confirm).
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.foreground().is_some_and(|process| !process.is_shell)
    }
}

fn grid_point(term: &Term<EventProxy>, cell: ViewportCell) -> (Point, Side) {
    let row = cell.row.min(term.screen_lines().saturating_sub(1));
    let line = i32::try_from(row).unwrap_or(i32::MAX)
        - i32::try_from(term.grid().display_offset()).unwrap_or(i32::MAX);
    let col = cell.col.min(term.columns().saturating_sub(1));
    let side = if cell.right_half {
        Side::Right
    } else {
        Side::Left
    };
    (Point::new(Line(line), Column(col)), side)
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Link::Local(io) = &self.link {
            io.send(Command::Shutdown);
        }
    }
}
