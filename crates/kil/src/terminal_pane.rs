//! A terminal pane: one session, its input, its screen (DESIGN.md §6, §8).

use std::{
    borrow::Cow,
    ops::Range,
    path::PathBuf,
    process::ExitStatus,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, AsyncApp, Bounds, ClipboardItem, Context, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, InputHandler, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString, Size,
    Task, UTF16Selection, WeakEntity, Window, actions, div, point, prelude::*,
};
use kil_term::{
    Client, Frame, GridSize, Palette, SelectionKind, Session, SessionConfig, SessionEvent,
    Transcript, ViewportCell,
};

use crate::{
    keys,
    restore::{self, SavedPane},
    settings::{Settings, abbreviate_home},
    terminal_element::{TerminalCursor, TerminalElement, cells},
    theme::Theme,
};

actions!(terminal, [Copy, Paste, ClearScrollback]);

pub const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Process info (title, cwd, busy) refreshes at most this often, trailing the last output.
const INFO_DEBOUNCE: Duration = Duration::from_millis(250);
/// A restored job is typed this long after its shell started, if the shell's line editor
/// never announced itself (see [`TerminalPane::rerun`]).
const RERUN_FALLBACK: Duration = Duration::from_secs(3);

/// What a pane tells its tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneEvent {
    /// [`TerminalPane::info`] or [`TerminalPane::ended_badly`] changed.
    InfoChanged,
    /// A foreground job ended; the shell is back in the foreground.
    JobFinished,
    /// The user clicked into the pane.
    Focused,
    /// The program posted a notification (OSC 9 or 777).
    Notified(kil_term::Notification),
    /// The pane should be removed (clean exit, or the user dismissed the exit banner).
    Closed,
    /// The screen changed since it was last taken for saving ([`TerminalPane::take_screen`]).
    Output,
}

/// How a pane starts: fresh, back on a session the server kept running (DESIGN.md §14), or
/// reopened from what an earlier launch saved (§4.9).
#[derive(Default)]
pub struct Launch {
    /// Names the pane's session in the server and its saved screen; kept across launches.
    pub id: String,
    /// The server still runs the session: show it instead of starting one. If it ended in
    /// the meantime, the pane starts afresh from the fields below.
    pub attach: bool,
    pub cwd: Option<PathBuf>,
    /// A job to type into the shell once it is ready.
    pub rerun: Option<String>,
    /// The saved screen, painted before the shell starts.
    pub replay: Vec<u8>,
}

/// What the sidebar shows for a pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneInfo {
    pub title: SharedString,
    pub cwd: SharedString,
    /// A job other than the shell is in the foreground.
    pub busy: bool,
    /// Name of that job, for close confirmations.
    pub job: Option<SharedString>,
}

enum Status {
    Starting,
    Running,
    /// Ended with a non-zero status, a signal, or unknown; shown in a banner.
    Exited(Option<ExitStatus>),
    Failed(SharedString),
    /// Closed by the user; shown without a banner while it animates out.
    Closing,
}

/// Where the grid was last laid out, for mouse hit-testing and IME placement.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub origin: Point<Pixels>,
    pub cell: Size<Pixels>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent view flags (cursor, drag, layout freeze, frame staleness), not states"
)]
pub struct TerminalPane {
    focus_handle: FocusHandle,
    session: Option<Session>,
    status: Status,
    exit_status: Option<ExitStatus>,
    title: Option<SharedString>,
    pub(crate) frame: Frame,
    /// `frame` is behind the session even though the session reads clean (see
    /// [`Self::refresh_info`]).
    stale: bool,
    grid: Option<GridSize>,
    pub(crate) geometry: Option<Geometry>,
    pub(crate) marked_text: Option<String>,
    /// Drawn over the pane as a view of its own; see [`TerminalCursor`].
    cursor: Entity<TerminalCursor>,
    last_input: Instant,
    scroll_remainder: f32,
    selecting: bool,
    /// A drag belongs to the program (it asked for 1002/1003): its press, drags and release go
    /// to it as reports, and Kil selects nothing.
    pub(crate) forwarding: bool,
    /// The cell the last report named, so a drag the platform re-emits without moving sends
    /// nothing.
    last_cell: Option<(usize, usize)>,
    info: PaneInfo,
    info_refresh: Option<Task<()>>,
    /// While the tab's layout animates, keep the grid: one resize when it settles, not one
    /// per frame (DESIGN.md §6.3).
    frozen: bool,
    id: String,
    /// Where the shell was asked to start.
    start_dir: Option<PathBuf>,
    /// A job from the last launch, typed into the shell once it is ready (DESIGN.md §4.9).
    rerun: Option<String>,
    /// Output arrived since the screen was last taken for saving.
    screen_changed: bool,
    /// How many times the grid was laid out: once per redraw of this pane's view.
    #[cfg(test)]
    pub(crate) layouts: usize,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<PaneEvent> for TerminalPane {}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalPane {
    pub fn new(launch: Launch, client: Arc<Client>, cx: &mut Context<Self>) -> Self {
        let Launch {
            id,
            attach,
            cwd,
            rerun,
            replay,
        } = launch;
        let settings = cx.global::<Settings>();
        let mut config = SessionConfig::new(GridSize {
            rows: 24,
            cols: 80,
            cell_width: 8,
            cell_height: 16,
        });
        config.cwd.clone_from(&cwd);
        config.scrollback_lines = settings.scrollback_lines;
        config.replay = replay;
        config.palette = cx.global::<Theme>().terminal.clone();

        let (tx, rx) = mpsc::unbounded();
        let sink = move |event| {
            let _ = tx.unbounded_send(event);
        };
        let session_id = id.clone();
        // Socket round trips, and a fork+exec in the server: keep them off the UI thread.
        let spawn = cx.background_spawn(async move {
            if attach {
                match client.attach(&session_id, config.scrollback_lines, sink.clone()) {
                    Ok(Some(session)) => return Ok(session),
                    Ok(None) => {
                        log::info!("session {session_id} ended meanwhile; starting it again");
                    }
                    Err(err) => return Err(kil_term::SpawnError::Server(err)),
                }
            }
            client.spawn(&session_id, &config, sink)
        });
        let tasks = vec![
            cx.spawn(async move |this, cx| {
                let result = spawn.await;
                let _ = this.update(cx, |pane, cx| pane.attach(result, cx));
            }),
            cx.spawn(async move |this, cx| pump(this, rx, cx).await),
        ];
        let pane = cx.entity();
        let cursor = cx.new(|cx| TerminalCursor::new(&pane, cx));
        // Named for its folder from the start, not "shell" until the shell is up. Where a
        // fresh shell starts (DESIGN.md §4.2): the asked-for directory, else `$HOME`. A session
        // the server kept may be anywhere, so it waits for the first refresh.
        let start = if attach {
            None
        } else {
            cwd.clone()
                .filter(|dir| dir.is_dir())
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        };

        Self {
            focus_handle: cx.focus_handle(),
            session: None,
            status: Status::Starting,
            exit_status: None,
            title: None,
            frame: Frame::default(),
            stale: true,
            grid: None,
            geometry: None,
            marked_text: None,
            cursor,
            last_input: Instant::now(),
            scroll_remainder: 0.,
            selecting: false,
            forwarding: false,
            last_cell: None,
            info: PaneInfo {
                title: start
                    .as_deref()
                    .map_or_else(|| "shell".into(), |dir| dir_name(dir).into()),
                cwd: start
                    .as_deref()
                    .map(abbreviate_home)
                    .unwrap_or_default()
                    .into(),
                ..PaneInfo::default()
            },
            info_refresh: None,
            frozen: false,
            id,
            start_dir: cwd,
            rerun,
            screen_changed: false,
            #[cfg(test)]
            layouts: 0,
            _tasks: tasks,
        }
    }

    pub fn info(&self) -> &PaneInfo {
        &self.info
    }

    /// What this pane costs the machine: its grid, and the program running in it.
    ///
    /// The program is the larger part: an agent with its own subprocesses dwarfs the
    /// terminal's own scrollback (DESIGN.md §5.2).
    pub fn memory(&self) -> u64 {
        self.session.as_ref().map_or(0, |session| {
            let usage = session.memory();
            u64::try_from(usage.grid_bytes).unwrap_or(u64::MAX) + usage.process_bytes
        })
    }

    /// The program ended on its own with a failure (or never started): the banner is up.
    pub fn ended_badly(&self) -> bool {
        matches!(self.status, Status::Exited(_) | Status::Failed(_))
    }

    /// Working directory of the foreground job (else the shell), queried fresh: new tabs and
    /// splits open here. Until the shell is up, where it was asked to start: a split made
    /// right after another opened in `$HOME`.
    pub fn current_dir(&self) -> Option<PathBuf> {
        self.session
            .as_ref()
            .and_then(Session::foreground)
            .and_then(|foreground| foreground.cwd)
            .or_else(|| self.start_dir.clone())
    }

    /// Name of the foreground job if it is not the shell, queried fresh for confirmations.
    pub fn busy_job(&self) -> Option<SharedString> {
        let foreground = self.session.as_ref()?.foreground()?;
        (!foreground.is_shell).then(|| {
            if foreground.name.is_empty() {
                "a job".into()
            } else {
                foreground.name.into()
            }
        })
    }

    pub fn set_frozen(&mut self, frozen: bool, cx: &mut Context<Self>) {
        if self.frozen != frozen {
            self.frozen = frozen;
            if !frozen {
                cx.notify();
            }
        }
    }

    /// What to reopen here at the next launch: the shell's directory and its foreground job,
    /// or the job still waiting to be re-run.
    pub fn saved(&self) -> SavedPane {
        let session = self.session.as_ref();
        let cwd = session
            .and_then(Session::shell_cwd)
            .or_else(|| self.start_dir.clone());
        let command = self.rerun.clone().or_else(|| {
            let job = session.and_then(Session::job)?;
            job.command_line
                .or_else(|| restore::command_line(&job.args))
        });
        SavedPane {
            id: self.id.clone(),
            cwd: cwd.and_then(|dir| dir.into_os_string().into_string().ok()),
            command,
        }
    }

    #[cfg(test)]
    pub fn start_dir(&self) -> Option<&PathBuf> {
        self.start_dir.as_ref()
    }

    #[cfg(test)]
    pub fn pid(&self) -> Option<kil_term::Pid> {
        self.session.as_ref().map(Session::pid)
    }

    #[cfg(test)]
    pub fn input_mode(&self) -> Option<kil_term::InputMode> {
        self.session.as_ref().map(Session::input_mode)
    }

    #[cfg(test)]
    pub fn screen_text(&self) -> String {
        let session = self.session.as_ref().expect("session");
        let mut frame = Frame::default();
        session.snapshot_into(&mut frame, &Palette::default());
        (0..frame.rows)
            .map(|row| frame.row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The pane's id: its session's name in the server, kept across launches.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether [`Self::take_screen`] has a screen to give.
    pub fn screen_changed(&self) -> bool {
        self.screen_changed && self.session.is_some()
    }

    /// The screen to save, if it changed since last taken.
    pub fn take_screen(&mut self) -> Option<(String, Transcript)> {
        let session = self.session.as_ref().filter(|_| self.screen_changed)?;
        self.screen_changed = false;
        Some((self.id.clone(), session.transcript()))
    }

    fn mark_screen_changed(&mut self, cx: &mut Context<Self>) {
        if !self.screen_changed {
            self.screen_changed = true;
            cx.emit(PaneEvent::Output);
        }
    }

    /// Types the restored job into the shell once its line editor waits for input, which
    /// zsh, bash 5.1+ and fish announce by turning on bracketed paste; pasted, the line can't
    /// trigger completion or other widgets. `force` types it anyway, for shells that never do.
    fn rerun(&mut self, force: bool) {
        let Some(session) = self.session.as_ref().filter(|_| self.rerun.is_some()) else {
            return;
        };
        let bracketed = session.input_mode().bracketed_paste();
        if !bracketed && !force {
            return;
        }
        if let Some(command) = self.rerun.take() {
            let mut bytes = keys::paste(&command, bracketed);
            bytes.push(b'\r');
            session.write(bytes);
        }
    }

    /// Applies a new theme or font: redraws from a fresh snapshot (cells are resolved to
    /// colors when copied), and tells the program's color queries about the palette.
    pub fn restyle(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        if let Some(session) = &self.session {
            session.set_palette(&cx.global::<Theme>().terminal);
        }
        cx.notify();
    }

    /// Ends the session now (hangup + reaper), keeping the last frame on screen for the
    /// close animation.
    pub fn shutdown(&mut self) {
        if let Some(session) = self.session.take() {
            session.close();
        }
        self.status = Status::Closing;
    }

    fn schedule_info_refresh(&mut self, cx: &mut Context<Self>) {
        if self.info_refresh.is_some() {
            return;
        }
        self.info_refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(INFO_DEBOUNCE).await;
            let _ = this.update(cx, |pane, cx| {
                pane.info_refresh = None;
                pane.refresh_info(cx);
            });
        }));
    }

    fn refresh_info(&mut self, cx: &mut Context<Self>) {
        // A pane in a background tab is never drawn, so its session would stay dirty and stop
        // sending wakeups: later output, and the end of its job, would go unseen. Re-arm them
        // here (at most once per debounce) and snapshot on the next draw instead.
        if self.session.as_ref().is_some_and(Session::take_dirty) {
            self.stale = true;
        }
        self.rerun(false);
        let foreground = self.session.as_ref().and_then(Session::foreground);
        let cwd = foreground.as_ref().and_then(|fg| fg.cwd.clone());
        let job = foreground
            .filter(|fg| !fg.is_shell)
            .map(|fg| SharedString::from(fg.name));
        let cwd_text = cwd.as_deref().map(abbreviate_home).unwrap_or_default();
        let title = self
            .title
            .clone()
            .or_else(|| job.clone())
            .or_else(|| cwd.as_deref().map(|dir| SharedString::from(dir_name(dir))))
            .unwrap_or_else(|| "shell".into());
        let info = PaneInfo {
            title,
            cwd: cwd_text.into(),
            busy: job.is_some(),
            job,
        };
        if info != self.info {
            let finished = self.info.busy && !info.busy;
            self.info = info;
            cx.emit(PaneEvent::InfoChanged);
            if finished {
                cx.emit(PaneEvent::JobFinished);
            }
        }
    }

    fn attach(&mut self, result: Result<Session, kil_term::SpawnError>, cx: &mut Context<Self>) {
        match result {
            Ok(session) => {
                if let Some(grid) = self.grid {
                    session.resize(grid);
                }
                // An attached session started with whatever theme was on then.
                session.set_palette(&cx.global::<Theme>().terminal);
                self.session = Some(session);
                self.stale = true;
                self.status = Status::Running;
                self.schedule_info_refresh(cx);
                if self.rerun.is_some() {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(RERUN_FALLBACK).await;
                        let _ = this.update(cx, |pane, _| pane.rerun(true));
                    })
                    .detach();
                }
            }
            Err(err) => {
                log::error!("session failed to start: {err:#}");
                let mut message = err.to_string();
                if let Some(source) = std::error::Error::source(&err) {
                    message = format!("{message}: {source}");
                }
                self.status = Status::Failed(message.into());
                cx.emit(PaneEvent::InfoChanged);
            }
        }
        cx.notify();
    }

    fn handle_events(&mut self, batch: &mut Vec<SessionEvent>, cx: &mut Context<Self>) {
        for event in batch.drain(..) {
            match event {
                SessionEvent::Wakeup => {
                    self.mark_screen_changed(cx);
                    self.rerun(false);
                    self.schedule_info_refresh(cx);
                    cx.notify();
                }
                SessionEvent::Title(title) => {
                    self.title = title
                        .filter(|title| !title.trim().is_empty())
                        .map(Into::into);
                    self.schedule_info_refresh(cx);
                }
                SessionEvent::ClipboardStore(text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                SessionEvent::ChildExited(status) => self.exit_status = Some(status),
                SessionEvent::Notify(notification) => cx.emit(PaneEvent::Notified(notification)),
                SessionEvent::Ended => self.ended(cx),
                SessionEvent::Bell | SessionEvent::CursorBlinkingChange => {}
            }
        }
    }

    fn ended(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Closing) {
            return;
        }
        if self.exit_status.is_some_and(|status| status.success()) {
            cx.emit(PaneEvent::Closed);
        } else {
            self.status = Status::Exited(self.exit_status);
            cx.emit(PaneEvent::InfoChanged);
            cx.notify();
        }
    }

    /// Called by the element each prepaint with the laid-out grid.
    pub(crate) fn layout(&mut self, grid: GridSize, geometry: Geometry, palette: &Palette) {
        #[cfg(test)]
        {
            self.layouts += 1;
        }
        self.geometry = Some(geometry);
        // Not even a new pane's first size while frozen: a pane split off grows in from
        // nothing, and its shell starting one row high, then grown, put its blank screen into
        // scrollback and pulled it back above the prompt, which sat at the bottom.
        if self.grid != Some(grid) && !self.frozen {
            self.grid = Some(grid);
            if let Some(session) = &self.session {
                session.resize(grid);
            }
        }
        // Skipped when nothing changed, e.g. for a cursor blink: the copy holds the terminal
        // lock that the PTY thread parses under.
        if let Some(session) = &self.session
            && (self.stale || session.is_dirty())
        {
            session.snapshot_into(&mut self.frame, palette);
            self.stale = false;
        }
    }

    fn send(&mut self, bytes: impl Into<Cow<'static, [u8]>>, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        // Typing before the restored job ran: the user has taken the pane over.
        self.rerun = None;
        session.scroll_to_bottom();
        session.clear_selection();
        session.write(bytes);
        self.last_input = Instant::now();
        self.show_cursor(cx);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Exited(_) | Status::Failed(_)) {
            if !event.keystroke.modifiers.platform {
                cx.emit(PaneEvent::Closed);
                cx.stop_propagation();
            }
            return;
        }
        let Some(session) = &self.session else { return };
        let option_as_meta = cx.global::<Settings>().option_as_meta;
        if let Some(bytes) = keys::encode(&event.keystroke, session.input_mode(), option_as_meta) {
            self.send(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn cell_at(&self, position: Point<Pixels>) -> Option<ViewportCell> {
        let geometry = self.geometry?;
        let x = ((position.x - geometry.origin.x) / geometry.cell.width).max(0.);
        let y = ((position.y - geometry.origin.y) / geometry.cell.height).max(0.);
        Some(ViewportCell {
            row: index(y),
            col: index(x),
            right_half: x.fract() >= 0.5,
        })
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        cx.emit(PaneEvent::Focused);
        let (Some(session), Some(cell)) = (&self.session, self.cell_at(event.position)) else {
            return;
        };
        let mode = session.input_mode();
        if mode.mouse_drag() && !event.modifiers.shift {
            // The program owns the selection (e.g. killi in fullscreen mode): it gets the
            // press, the drags and the release as reports, and Kil selects nothing.
            session.clear_selection();
            let (col, row) = self.clamp_cell(cell);
            if let Some(report) =
                keys::button_report(0, false, true, col, row, event.modifiers, mode)
            {
                session.write(report);
            }
            self.forwarding = true;
            self.last_cell = Some((col, row));
            return;
        }
        self.forwarding = false;
        self.last_cell = None;
        let kind = match event.click_count {
            0 | 1 => SelectionKind::Cells,
            2 => SelectionKind::Words,
            _ => SelectionKind::Lines,
        };
        session.start_selection(kind, cell);
        self.selecting = true;
        cx.notify();
    }

    /// A viewport cell inside the grid: a drag past the pane's edge names its first or last
    /// cell, so the program's edge autoscroll keeps moving until the button is let go.
    fn clamp_cell(&self, cell: ViewportCell) -> (usize, usize) {
        (
            cell.col.min(self.frame.cols.saturating_sub(1)),
            cell.row.min(self.frame.rows.saturating_sub(1)),
        )
    }

    /// A drag the program owns: its cell as a drag report, unless the pointer hasn't left the
    /// last reported cell (the platform re-emits the position while the button is held).
    pub(crate) fn forward_drag(&mut self, position: Point<Pixels>, modifiers: Modifiers) {
        let (Some(session), Some(cell)) = (&self.session, self.cell_at(position)) else {
            return;
        };
        let mode = session.input_mode();
        if !mode.mouse_drag() {
            self.forwarding = false;
            self.last_cell = None;
            return;
        }
        let cell = self.clamp_cell(cell);
        if self.last_cell == Some(cell) {
            return;
        }
        self.last_cell = Some(cell);
        if let Some(report) = keys::button_report(0, true, true, cell.0, cell.1, modifiers, mode) {
            session.write(report);
        }
    }

    /// The release of a drag the program owns, at its (clamped) position.
    pub(crate) fn forward_up(&mut self, position: Point<Pixels>, modifiers: Modifiers) {
        self.forwarding = false;
        self.last_cell = None;
        let (Some(session), Some(cell)) = (&self.session, self.cell_at(position)) else {
            return;
        };
        let (col, row) = self.clamp_cell(cell);
        let mode = session.input_mode();
        if let Some(report) = keys::button_report(0, false, false, col, row, modifiers, mode) {
            session.write(report);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.forwarding {
            if event.dragging() {
                self.forward_drag(event.position, event.modifiers);
            }
            return;
        }
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let (Some(session), Some(cell)) = (&self.session, self.cell_at(event.position)) {
            session.update_selection(cell);
            cx.notify();
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if self.forwarding {
            if event.button == MouseButton::Left {
                self.forward_up(event.position, event.modifiers);
            }
            return;
        }
        self.selecting = false;
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(session), Some(geometry)) = (&self.session, self.geometry) else {
            return;
        };
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => delta.y / geometry.cell.height,
        };
        self.scroll_remainder += lines;
        let whole = self.scroll_remainder.trunc();
        self.scroll_remainder -= whole;
        let Some(steps) = whole_lines(whole) else {
            return;
        };
        let mode = session.input_mode();
        let notches = usize::try_from(steps.unsigned_abs()).unwrap_or(0);
        if mode.mouse_reporting() {
            // The program scrolls itself (e.g. Claude Code): it gets wheel events, as in xterm.
            // Arrow keys would move its cursor or history instead.
            let Some(cell) = self.cell_at(event.position) else {
                return;
            };
            let col = cell.col.min(self.frame.cols.saturating_sub(1));
            let row = cell.row.min(self.frame.rows.saturating_sub(1));
            if let Some(report) = keys::wheel_report(steps > 0, col, row, event.modifiers, mode) {
                session.write(report.repeat(notches));
            }
        } else if mode.alt_screen() && mode.alternate_scroll() {
            let arrow = keys::wheel_arrow(steps > 0, mode);
            session.write(arrow.repeat(notches));
        } else {
            session.scroll(steps);
        }
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.as_ref().and_then(Session::selection_text) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(session) = &self.session else { return };
        let bracketed = session.input_mode().bracketed_paste();
        let Some(bytes) = keys::paste_clipboard(item.text().as_deref(), bracketed) else {
            return;
        };
        self.send(bytes, cx);
    }

    /// Files dropped on the pane are typed as their paths, the way Terminal.app does it.
    fn drop_files(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let bracketed = session.input_mode().bracketed_paste();
        self.send(keys::drop(paths.paths(), bracketed), cx);
    }

    fn clear_scrollback(&mut self, _: &ClearScrollback, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            session.clear_history();
            self.mark_screen_changed(cx);
            cx.notify();
        }
    }

    /// One blink step; the window's cursor timer calls it on the focused pane only.
    pub fn blink(&mut self, window: &Window, cx: &mut Context<Self>) {
        let active = self.focus_handle.is_focused(window) && window.is_window_active();
        let typing = self.last_input.elapsed() < BLINK_INTERVAL;
        self.cursor.update(cx, |cursor, cx| {
            // Keep the cursor solid while typing.
            let visible = !active || typing || !cursor.visible;
            cursor.set_visible(visible, cx);
        });
    }

    /// Leaves the cursor drawn, for when blinking stops or moves to another pane.
    pub fn show_cursor(&mut self, cx: &mut Context<Self>) {
        self.cursor
            .update(cx, |cursor, cx| cursor.set_visible(true, cx));
    }

    /// The view that draws this pane's cursor, over the pane.
    pub fn cursor(&self) -> &Entity<TerminalCursor> {
        &self.cursor
    }

    fn banner(&self) -> Option<SharedString> {
        match &self.status {
            Status::Starting | Status::Running | Status::Closing => None,
            Status::Failed(message) => Some(format!("{message} — press any key to close").into()),
            Status::Exited(status) => {
                let how = match status {
                    Some(status) if status.code().is_some() => {
                        format!("exited with status {}", status.code().unwrap_or_default())
                    }
                    Some(status) => {
                        use std::os::unix::process::ExitStatusExt;
                        format!("killed by signal {}", status.signal().unwrap_or_default())
                    }
                    None => "ended".to_owned(),
                };
                Some(format!("Process {how} — press any key to close").into())
            }
        }
    }
}

impl Render for TerminalPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let banner = self.banner().map(|text| {
            div()
                .absolute()
                .bottom_3()
                .left_3()
                .right_3()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(theme.banner_bg)
                .text_color(theme.sidebar_text)
                .text_sm()
                .child(text)
        });
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.terminal_bg)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear_scrollback))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_drop(cx.listener(Self::drop_files))
            .child(TerminalElement::new(cx.entity()))
            .children(banner)
    }
}

fn dir_name(dir: &std::path::Path) -> String {
    let abbreviated = abbreviate_home(dir);
    if abbreviated == "~" || abbreviated == "/" {
        return abbreviated;
    }
    dir.file_name()
        .map_or(abbreviated, |name| name.to_string_lossy().into_owned())
}

/// Feeds session events to the pane in batches: one entity update per wakeup, not per event.
async fn pump(
    this: WeakEntity<TerminalPane>,
    mut rx: mpsc::UnboundedReceiver<SessionEvent>,
    cx: &mut AsyncApp,
) {
    let mut batch = Vec::new();
    while let Some(first) = rx.next().await {
        batch.push(first);
        while let Ok(event) = rx.try_recv() {
            batch.push(event);
        }
        if this
            .update(cx, |pane, cx| pane.handle_events(&mut batch, cx))
            .is_err()
        {
            return;
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=u16::MAX first"
)]
fn index(cells: f32) -> usize {
    cells.floor().clamp(0., f32::from(u16::MAX)) as usize
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to ±1000 lines first"
)]
fn whole_lines(lines: f32) -> Option<i32> {
    let lines = lines.clamp(-1000., 1000.) as i32;
    (lines != 0).then_some(lines)
}

/// IME and text input for a pane. Wraps the pane so press-and-hold can be turned off:
/// a terminal wants key repeat, not the accent picker.
pub struct TerminalInput {
    pub pane: Entity<TerminalPane>,
}

impl InputHandler for TerminalInput {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let end = self
            .pane
            .read(cx)
            .marked_text
            .as_deref()
            .map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        let text = self.pane.read(cx).marked_text.as_deref()?;
        Some(0..text.encode_utf16().count())
    }

    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let text = self.pane.read(cx).marked_text.as_deref()?;
        let units: Vec<u16> = text.encode_utf16().collect();
        let range = range.start.min(units.len())..range.end.min(units.len());
        *adjusted = Some(range.clone());
        String::from_utf16(&units[range]).ok()
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut App,
    ) {
        self.pane.update(cx, |pane, cx| {
            pane.marked_text = None;
            if !text.is_empty() {
                pane.send(text.as_bytes().to_vec(), cx);
            }
            cx.notify();
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) {
        self.pane.update(cx, |pane, cx| {
            pane.marked_text = (!text.is_empty()).then(|| text.to_owned());
            cx.notify();
        });
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        self.pane.update(cx, |pane, cx| {
            pane.marked_text = None;
            cx.notify();
        });
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let pane = self.pane.read(cx);
        let geometry = pane.geometry?;
        let cursor = pane.frame.cursor?;
        let origin = point(
            geometry.origin.x + geometry.cell.width * cells(cursor.col),
            geometry.origin.y + geometry.cell.height * cells(cursor.row),
        );
        Some(Bounds::new(origin, geometry.cell))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        None
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}
