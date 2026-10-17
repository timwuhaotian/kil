//! A terminal session: one PTY, one child, one VT state (DESIGN.md §3).

use std::{borrow::Cow, ffi::OsString, mem::size_of, path::PathBuf, sync::Arc};

use alacritty_terminal::{
    event::WindowSize,
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config as TermConfig, Osc52, Term, TermMode, cell::Cell},
};

use crate::{
    error::SpawnError,
    events::{ColorQuery, EventProxy, SessionEvent},
    proc_info::{self, ForegroundProcess, Pid, ProcIdentity, TtyId},
    pty::Pty,
    snapshot::{Frame, Palette, Rgba},
    spawn::{self, Spawned},
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
    fn clamped(self) -> Self {
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
#[derive(Clone, Debug)]
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
/// Dropping it closes the session without blocking: the PTY thread hangs up the child and
/// the reaper finishes it (DESIGN.md §4.5).
pub struct Session {
    term: Arc<FairMutex<Term<EventProxy>>>,
    io: EventLoopSender,
    events: EventProxy,
    pid: Pid,
    shell: Option<ProcIdentity>,
    tty: TtyId,
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
        let Spawned { master, child, tty } = spawn::spawn(config)?;
        let pid = child.pid();
        let shell = ProcIdentity::capture(pid);
        let events = EventProxy::new(Box::new(sink), size);
        let term_config = TermConfig {
            scrolling_history: config.scrollback_lines,
            osc52: Osc52::OnlyCopy,
            kitty_keyboard: false,
            ..TermConfig::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            term_config,
            &size,
            events.clone(),
        )));
        let pty = Pty::new(master, child, events.clone());
        let event_loop = EventLoop::new(Arc::clone(&term), events.clone(), pty, true, false)
            .map_err(SpawnError::EventLoop)?;
        let io = event_loop.channel();
        events.attach(io.clone());
        // Detached on purpose: the thread ends on Shutdown or child exit and drops the PTY
        // itself, so nothing ever joins (and blocks on) it.
        let _detached = event_loop.spawn();
        Ok(Self {
            term,
            io,
            events,
            pid,
            shell,
            tty,
        })
    }

    /// PID of the child (the shell).
    #[must_use]
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Sends input bytes to the program.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.io.send(Msg::Input(bytes.into()));
    }

    /// Resizes the grid and the PTY; the program receives one `SIGWINCH`.
    pub fn resize(&self, size: GridSize) {
        let size = size.clamped();
        self.events.set_size(size);
        self.term.lock().resize(size);
        let _ = self.io.send(Msg::Resize(size.into()));
    }

    /// Whether output arrived since the last snapshot.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.events.is_dirty()
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
    }

    /// Returns the viewport to the live screen.
    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// Drops all scrollback. The allocator keeps the pages for reuse (DESIGN.md §5.1).
    pub fn clear_history(&self) {
        self.term.lock().grid_mut().clear_history();
    }

    /// Answers a [`SessionEvent::ColorRequest`]: runtime overrides first, then `palette`.
    pub fn answer_color(&self, query: &ColorQuery, palette: &Palette) {
        let color = self.term.lock().colors()[query.index]
            .map_or_else(|| palette.slot(query.index), Rgba::from);
        self.write((query.reply)(color.into()).into_bytes());
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
    }

    /// Extends the current selection to `cell`.
    pub fn update_selection(&self, cell: ViewportCell) {
        let mut term = self.term.lock();
        let (point, side) = grid_point(&term, cell);
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// Removes the selection.
    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
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
        let _ = self.io.send(Msg::Shutdown);
    }
}
