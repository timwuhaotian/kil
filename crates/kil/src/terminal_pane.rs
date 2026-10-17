//! A terminal pane: one session, its input, its screen (DESIGN.md §6, §8).

use std::{
    borrow::Cow,
    ops::Range,
    path::PathBuf,
    process::ExitStatus,
    time::{Duration, Instant},
};

use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, AsyncApp, AsyncWindowContext, Bounds, ClipboardItem, Context, Entity, EventEmitter,
    FocusHandle, Focusable, InputHandler, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString, Size,
    Task, UTF16Selection, WeakEntity, Window, actions, div, point, prelude::*,
};
use kil_term::{
    Frame, GridSize, Palette, SelectionKind, Session, SessionConfig, SessionEvent, ViewportCell,
};

use crate::{
    keys,
    settings::Settings,
    terminal_element::{TerminalElement, cells},
    theme::Theme,
};

actions!(terminal, [Copy, Paste, ClearScrollback]);

const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Process info (title, cwd, busy) refreshes at most this often, trailing the last output.
const INFO_DEBOUNCE: Duration = Duration::from_millis(250);

/// What a pane tells its tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneEvent {
    /// [`TerminalPane::info`] changed.
    InfoChanged,
    /// The user clicked into the pane.
    Focused,
    /// The pane should be removed (clean exit, or the user dismissed the exit banner).
    Closed,
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
}

/// Where the grid was last laid out, for mouse hit-testing and IME placement.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub origin: Point<Pixels>,
    pub cell: Size<Pixels>,
}

pub struct TerminalPane {
    focus_handle: FocusHandle,
    session: Option<Session>,
    status: Status,
    exit_status: Option<ExitStatus>,
    title: Option<SharedString>,
    pub(crate) frame: Frame,
    grid: Option<GridSize>,
    pub(crate) geometry: Option<Geometry>,
    pub(crate) marked_text: Option<String>,
    pub(crate) cursor_visible: bool,
    last_input: Instant,
    scroll_remainder: f32,
    selecting: bool,
    info: PaneInfo,
    info_refresh: Option<Task<()>>,
    /// While the tab's layout animates, keep the grid: one resize when it settles, not one
    /// per frame (DESIGN.md §6.3).
    frozen: bool,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<PaneEvent> for TerminalPane {}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalPane {
    pub fn new(cwd: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = cx.global::<Settings>();
        let mut config = SessionConfig::new(GridSize {
            rows: 24,
            cols: 80,
            cell_width: 8,
            cell_height: 16,
        });
        config.cwd = cwd;
        config.scrollback_lines = settings.scrollback_lines;
        let blink = settings.cursor_blink;

        let (tx, rx) = mpsc::unbounded();
        // fork+exec of a large GUI process takes milliseconds: keep it off the UI thread.
        let spawn = cx.background_spawn(async move {
            Session::spawn(&config, move |event| {
                let _ = tx.unbounded_send(event);
            })
        });
        let mut tasks = vec![
            cx.spawn(async move |this, cx| {
                let result = spawn.await;
                let _ = this.update(cx, |pane, cx| pane.attach(result, cx));
            }),
            cx.spawn(async move |this, cx| pump(this, rx, cx).await),
        ];
        if blink {
            tasks.push(cx.spawn_in(window, async move |this, cx| blink_loop(this, cx).await));
        }

        Self {
            focus_handle: cx.focus_handle(),
            session: None,
            status: Status::Starting,
            exit_status: None,
            title: None,
            frame: Frame::default(),
            grid: None,
            geometry: None,
            marked_text: None,
            cursor_visible: true,
            last_input: Instant::now(),
            scroll_remainder: 0.,
            selecting: false,
            info: PaneInfo {
                title: "shell".into(),
                ..PaneInfo::default()
            },
            info_refresh: None,
            frozen: false,
            _tasks: tasks,
        }
    }

    pub fn info(&self) -> &PaneInfo {
        &self.info
    }

    /// Working directory of the foreground job (else the shell), queried fresh: new tabs and
    /// splits open here.
    pub fn current_dir(&self) -> Option<PathBuf> {
        self.session.as_ref()?.foreground()?.cwd
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

    /// Ends the session now (hangup + reaper), keeping the last frame on screen for the
    /// close animation.
    pub fn shutdown(&mut self) {
        self.session = None;
        self.status = Status::Exited(self.exit_status);
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
            self.info = info;
            cx.emit(PaneEvent::InfoChanged);
        }
    }

    fn attach(&mut self, result: Result<Session, kil_term::SpawnError>, cx: &mut Context<Self>) {
        match result {
            Ok(session) => {
                if let Some(grid) = self.grid {
                    session.resize(grid);
                }
                self.session = Some(session);
                self.status = Status::Running;
                self.schedule_info_refresh(cx);
            }
            Err(err) => {
                log::error!("session failed to start: {err:#}");
                let mut message = err.to_string();
                if let Some(source) = std::error::Error::source(&err) {
                    message = format!("{message}: {source}");
                }
                self.status = Status::Failed(message.into());
            }
        }
        cx.notify();
    }

    fn handle_events(&mut self, batch: &mut Vec<SessionEvent>, cx: &mut Context<Self>) {
        for event in batch.drain(..) {
            match event {
                SessionEvent::Wakeup => {
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
                SessionEvent::ColorRequest(query) => {
                    if let Some(session) = &self.session {
                        session.answer_color(&query, &cx.global::<Theme>().terminal);
                    }
                }
                SessionEvent::ChildExited(status) => self.exit_status = Some(status),
                SessionEvent::Ended => self.ended(cx),
                SessionEvent::Bell | SessionEvent::CursorBlinkingChange => {}
            }
        }
    }

    fn ended(&mut self, cx: &mut Context<Self>) {
        if self.exit_status.is_some_and(|status| status.success()) {
            cx.emit(PaneEvent::Closed);
        } else {
            self.status = Status::Exited(self.exit_status);
            cx.notify();
        }
    }

    /// Called by the element each prepaint with the laid-out grid.
    pub(crate) fn layout(&mut self, grid: GridSize, geometry: Geometry, palette: &Palette) {
        self.geometry = Some(geometry);
        if self.grid != Some(grid) && (!self.frozen || self.grid.is_none()) {
            self.grid = Some(grid);
            if let Some(session) = &self.session {
                session.resize(grid);
            }
        }
        if let Some(session) = &self.session {
            session.snapshot_into(&mut self.frame, palette);
        }
    }

    fn send(&mut self, bytes: impl Into<Cow<'static, [u8]>>, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        session.scroll_to_bottom();
        session.clear_selection();
        session.write(bytes);
        self.last_input = Instant::now();
        self.cursor_visible = true;
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
        let kind = match event.click_count {
            0 | 1 => SelectionKind::Cells,
            2 => SelectionKind::Words,
            _ => SelectionKind::Lines,
        };
        session.start_selection(kind, cell);
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let (Some(session), Some(cell)) = (&self.session, self.cell_at(event.position)) {
            session.update_selection(cell);
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
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
        if mode.alt_screen() && mode.alternate_scroll() {
            let arrow = keys::wheel_arrow(steps > 0, mode);
            session.write(arrow.repeat(usize::try_from(steps.unsigned_abs()).unwrap_or(0)));
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
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(session) = &self.session else { return };
        let bytes = keys::paste(&text, session.input_mode().bracketed_paste());
        self.send(bytes, cx);
    }

    fn clear_scrollback(&mut self, _: &ClearScrollback, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            session.clear_history();
            cx.notify();
        }
    }

    fn blink(&mut self, window: &Window, cx: &mut Context<Self>) {
        let active = self.focus_handle.is_focused(window) && window.is_window_active();
        // Keep the cursor solid while typing.
        let visible = !active || self.last_input.elapsed() < BLINK_INTERVAL || !self.cursor_visible;
        if visible != self.cursor_visible {
            self.cursor_visible = visible;
            cx.notify();
        }
    }

    fn banner(&self) -> Option<SharedString> {
        match &self.status {
            Status::Starting | Status::Running => None,
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let focused = self.focus_handle.is_focused(window);
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
            .child(TerminalElement::new(cx.entity(), focused))
            .children(banner)
    }
}

fn abbreviate_home(dir: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|home| dir.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
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

async fn blink_loop(this: WeakEntity<TerminalPane>, cx: &mut AsyncWindowContext) {
    loop {
        cx.background_executor().timer(BLINK_INTERVAL).await;
        if this
            .update_in(cx, |pane, window, cx| pane.blink(window, cx))
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
