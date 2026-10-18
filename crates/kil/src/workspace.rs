//! Window root: vertical tab sidebar, the active tab's split tree, and every window-level
//! action (DESIGN.md §4.6, §4.7, §7, §8).

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, AnyWindowHandle, App, ClickEvent, Context,
    CursorStyle, Div, DragMoveEvent, ElementId, Entity, EntityId, FocusHandle, Focusable,
    FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, PromptLevel,
    SharedString, Stateful, StyleRefinement, Subscription, Task, WeakEntity, Window, actions,
    anchored, deferred, div, prelude::*, px, relative,
};

use crate::{
    config,
    motion::{self, Ease, Tween},
    pane_tree::{Axis, Close, Direction, Node, PaneTree, Split, SplitId},
    settings::Settings,
    terminal_element::cells,
    terminal_pane::{BLINK_INTERVAL, PaneEvent, TerminalPane},
    text_field::{FieldEvent, TextField},
    theme::Theme,
};

actions!(
    workspace,
    [
        NewTab,
        CloseTab,
        ClosePane,
        SplitRight,
        SplitDown,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        NextTab,
        PrevTab,
        SelectTab1,
        SelectTab2,
        SelectTab3,
        SelectTab4,
        SelectTab5,
        SelectTab6,
        SelectTab7,
        SelectTab8,
        SelectTab9,
        FontBigger,
        FontSmaller,
        FontReset,
        Quit,
    ]
);

const SIDEBAR_WIDTH: f32 = 220.;
const SIDEBAR_TOP: f32 = 52.;
const ROW_HEIGHT: f32 = 44.;
/// The row's status mark; the cwd line indents past it to align with the title.
const STATUS_SLOT: f32 = 10.;
const TITLE_INSET: f32 = STATUS_SLOT + 6.;
/// Natural exit before the reaper escalates on quit (DESIGN.md §4.6).
const QUIT_GRACE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TabId(u64);

struct Tab {
    id: TabId,
    tree: PaneTree<Entity<TerminalPane>>,
    focused: Entity<TerminalPane>,
    /// Set by the user (Rename Tab); otherwise the row follows the focused pane.
    name: Option<SharedString>,
    /// A job ended here while the tab was not in view; cleared when it is.
    finished: bool,
    animating: bool,
}

/// What a sidebar row's status mark shows, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabStatus {
    /// A pane's program ended badly and its banner is up.
    Failed,
    /// A pane has a foreground job.
    Running,
    /// A job ended while the tab was not in view.
    Done,
    Idle,
}

impl Tab {
    fn title(&self, cx: &App) -> SharedString {
        self.name
            .clone()
            .unwrap_or_else(|| self.focused.read(cx).info().title.clone())
    }

    fn status(&self, cx: &App) -> TabStatus {
        let panes = self.tree.leaves();
        if panes.iter().any(|pane| pane.read(cx).ended_badly()) {
            TabStatus::Failed
        } else if panes.iter().any(|pane| pane.read(cx).info().busy) {
            TabStatus::Running
        } else if self.finished {
            TabStatus::Done
        } else {
            TabStatus::Idle
        }
    }
}

/// A closed tab's row while it animates out.
struct Ghost {
    id: TabId,
    after: Option<TabId>,
    title: SharedString,
}

/// The right-click menu of a sidebar row, at the click.
#[derive(Clone, Copy)]
struct TabMenu {
    tab: TabId,
    position: Point<Pixels>,
}

/// A row whose title is being edited in place.
struct Rename {
    tab: TabId,
    field: Entity<TextField>,
    _subscription: Subscription,
}

#[derive(Clone, Copy)]
struct DividerDrag {
    tab: TabId,
    split: SplitId,
}

struct EmptyDrag;

impl Render for EmptyDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Theme colors copied out so `cx` stays free for listeners while rendering.
#[derive(Clone, Copy)]
struct Chrome {
    terminal_bg: Hsla,
    sidebar_bg: Hsla,
    text: Hsla,
    muted: Hsla,
    hover: Hsla,
    selected: Hsla,
    accent: Hsla,
    failure: Hsla,
    divider: Hsla,
    dim: Hsla,
    banner: Hsla,
}

impl Chrome {
    fn of(theme: &Theme) -> Self {
        Self {
            terminal_bg: theme.terminal_bg,
            sidebar_bg: theme.sidebar_bg,
            text: theme.sidebar_text,
            muted: theme.sidebar_muted,
            hover: theme.sidebar_hover,
            selected: theme.sidebar_selected,
            accent: theme.accent,
            failure: theme.failure,
            divider: theme.divider,
            dim: theme.inactive_dim,
            banner: theme.banner_bg,
        }
    }
}

pub struct Workspace {
    focus_handle: FocusHandle,
    tabs: Vec<Tab>,
    active: usize,
    next_tab: u64,
    /// Offset of the selection pill from the active row, in rows; eases to 0 so the pill
    /// slides from the previous row.
    pill: Tween,
    ghosts: Vec<Ghost>,
    menu: Option<TabMenu>,
    /// Held by the open menu so Escape reaches it instead of the terminal.
    menu_focus: FocusHandle,
    rename: Option<Rename>,
    notices: Vec<SharedString>,
    config: config::Stamp,
    pane_subscriptions: HashMap<EntityId, Subscription>,
    close_confirmed: bool,
    window_title: SharedString,
    /// Font size from the config file, for `font_reset`.
    base_font_size: f32,
    /// The window's one cursor-blink timer; see [`Self::sync_blink`].
    blink: Option<Task<()>>,
    /// The pane it last blinked, so a pane that loses focus is left with its cursor drawn.
    blinking: Option<WeakEntity<TerminalPane>>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(notices: Vec<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.reload_config_if_changed(window, cx);
                if let Some(tab) = this.tabs.get_mut(this.active) {
                    tab.finished = false;
                    cx.notify();
                }
            }
            this.sync_blink(window, cx);
        });
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.should_close_window(window, cx))
                .unwrap_or(true)
        });
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active: 0,
            next_tab: 0,
            pill: Tween::settled(0.),
            ghosts: Vec::new(),
            menu: None,
            menu_focus: cx.focus_handle(),
            rename: None,
            notices,
            config: config::stamp(),
            pane_subscriptions: HashMap::new(),
            close_confirmed: false,
            window_title: SharedString::default(),
            base_font_size: f32::from(cx.global::<Settings>().font_size),
            blink: None,
            blinking: None,
            _subscriptions: vec![activation],
        };
        this.open_tab(None, window, cx);
        this.sync_blink(window, cx);
        this
    }

    // ── tabs ──────────────────────────────────────────────────────────────────────────

    fn new_pane(
        &mut self,
        cwd: Option<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let pane = cx.new(|cx| TerminalPane::new(cwd, cx));
        let subscription = cx.subscribe_in(&pane, window, Self::on_pane_event);
        self.pane_subscriptions
            .insert(pane.entity_id(), subscription);
        pane
    }

    fn open_tab(
        &mut self,
        cwd: Option<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.new_pane(cwd, window, cx);
        let id = TabId(self.next_tab);
        self.next_tab += 1;
        self.tabs.push(Tab {
            id,
            tree: PaneTree::new(pane.clone()),
            focused: pane,
            name: None,
            finished: false,
            animating: false,
        });
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        tab.finished = false;
        if index != self.active {
            let now = Instant::now();
            let offset = self.pill.value(now) + cells(self.active) - cells(index);
            self.pill.set(offset);
            self.pill.retarget(0., motion::PILL, Ease::OutQuint, now);
        }
        self.active = index;
        window.focus(&tab.focused.focus_handle(cx));
        cx.notify();
    }

    /// `cmd-1`…`cmd-8` pick that tab if it exists; `cmd-9` (`usize::MAX`) picks the last.
    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.tabs.len().saturating_sub(1);
        if index == usize::MAX {
            self.activate(last, window, cx);
        } else if index <= last {
            self.activate(index, window, cx);
        }
    }

    fn activate_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
            self.activate(index, window, cx);
        }
    }

    fn close_tab_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return;
        };
        let panes: Vec<_> = tab.tree.leaves().into_iter().cloned().collect();
        self.confirm_close(
            &panes,
            "Close this tab?",
            window,
            cx,
            move |this, window, cx| {
                if let Some(index) = this.tabs.iter().position(|tab| tab.id == id) {
                    this.remove_tab(index, window, cx);
                }
            },
        );
    }

    fn remove_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tabs.remove(index);
        for pane in tab.tree.leaves() {
            pane.update(cx, |pane, _| pane.shutdown());
            self.pane_subscriptions.remove(&pane.entity_id());
        }
        self.menu = self.menu.filter(|menu| menu.tab != tab.id);
        if self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.tab == tab.id)
        {
            self.rename = None;
        }
        let after = index
            .checked_sub(1)
            .and_then(|i| self.tabs.get(i))
            .map(|tab| tab.id);
        self.ghosts.push(Ghost {
            id: tab.id,
            after,
            title: tab.title(cx),
        });
        let ghost = tab.id;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(motion::ROW_OUT).await;
            let _ = this.update(cx, |this, cx| {
                this.ghosts.retain(|g| g.id != ghost);
                cx.notify();
            });
        })
        .detach();

        if self.tabs.is_empty() {
            self.shutdown_all(cx);
            cx.quit();
            return;
        }
        if index < self.active || (index == self.active && index == self.tabs.len()) {
            // The active row moved up one, or the last tab closed and its predecessor takes over.
            let removed_active = index == self.active;
            self.active -= 1;
            if removed_active {
                self.pill.set(1.);
                self.pill
                    .retarget(0., motion::ROW_OUT, Ease::InOut, Instant::now());
            }
        } else if index == self.active {
            // The next row slides up into place while the ghost shrinks above it.
            self.pill.set(-1.);
            self.pill
                .retarget(0., motion::ROW_OUT, Ease::InOut, Instant::now());
        }
        let next = &mut self.tabs[self.active];
        next.finished = false;
        let focused = next.focused.focus_handle(cx);
        window.focus(&focused);
        cx.notify();
    }

    // ── row menu, rename ──────────────────────────────────────────────────────────────

    fn open_menu(
        &mut self,
        tab: TabId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some(TabMenu { tab, position });
        window.focus(&self.menu_focus);
        cx.notify();
    }

    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            window.focus(&tab.focused.focus_handle(cx));
        }
    }

    fn start_rename(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|t| t.title(cx))
        else {
            return;
        };
        let field = cx.new(|cx| TextField::new(&title, window, cx));
        let subscription = cx.subscribe_in(&field, window, Self::end_rename);
        window.focus(&field.focus_handle(cx));
        self.rename = Some(Rename {
            tab: id,
            field,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// An empty name gives the row back to the pane's title.
    fn end_rename(
        &mut self,
        field: &Entity<TextField>,
        event: &FieldEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rename) = self.rename.take_if(|rename| &rename.field == field) else {
            return;
        };
        if let FieldEvent::Confirmed(text) = event {
            let name = text.trim();
            self.set_name(
                rename.tab,
                (!name.is_empty()).then(|| name.to_owned().into()),
            );
        }
        // Enter or Escape: back to the terminal. A click elsewhere already moved focus.
        if field.focus_handle(cx).is_focused(window) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn set_name(&mut self, id: TabId, name: Option<SharedString>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.name = name;
        }
    }

    // ── panes ─────────────────────────────────────────────────────────────────────────

    fn on_pane_event(
        &mut self,
        pane: &Entity<TerminalPane>,
        event: &PaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PaneEvent::InfoChanged => cx.notify(),
            PaneEvent::JobFinished => {
                let watching = window.is_window_active();
                let active = self.active;
                if let Some((index, tab)) = self
                    .tabs
                    .iter_mut()
                    .enumerate()
                    .find(|(_, tab)| tab.tree.contains(pane))
                    && !(watching && index == active)
                {
                    tab.finished = true;
                    cx.notify();
                }
            }
            PaneEvent::Focused => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.tree.contains(pane)) {
                    tab.focused = pane.clone();
                    cx.notify();
                }
            }
            PaneEvent::Closed => self.remove_pane(pane, window, cx),
        }
    }

    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.tabs.get(self.active).map(|tab| tab.focused.clone()) else {
            return;
        };
        let cwd = target.read(cx).current_dir();
        let pane = self.new_pane(cwd, window, cx);
        let tab = &mut self.tabs[self.active];
        if tab.tree.split(&target, axis, pane.clone(), Instant::now()) {
            tab.focused = pane.clone();
            window.focus(&pane.focus_handle(cx));
        }
        cx.notify();
    }

    fn remove_pane(
        &mut self,
        pane: &Entity<TerminalPane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.tree.contains(pane)) else {
            return;
        };
        pane.update(cx, |pane, _| pane.shutdown());
        match self.tabs[index].tree.close(pane, Instant::now()) {
            Close::LastLeaf => self.remove_tab(index, window, cx),
            Close::Animating => {
                let tab = &mut self.tabs[index];
                if &tab.focused == pane
                    && let Some(next) = tab
                        .tree
                        .leaves()
                        .into_iter()
                        .find(|leaf| *leaf != pane)
                        .cloned()
                {
                    tab.focused = next;
                    if index == self.active {
                        window.focus(&tab.focused.focus_handle(cx));
                    }
                }
                cx.notify();
            }
            Close::NotFound => {}
        }
    }

    fn focus_toward(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if let Some(next) = tab.tree.neighbor(&tab.focused, direction).cloned() {
            window.focus(&next.focus_handle(cx));
            tab.focused = next;
            cx.notify();
        }
    }

    fn set_ratio(&mut self, tab: TabId, split: SplitId, ratio: f32, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab) {
            tab.tree.set_ratio(split, ratio);
            cx.notify();
        }
    }

    // ── closing, quitting ─────────────────────────────────────────────────────────────

    /// Runs `then` now if no pane in `panes` has a foreground job; otherwise asks first
    /// (DESIGN.md §4.7).
    fn confirm_close(
        &mut self,
        panes: &[Entity<TerminalPane>],
        message: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let jobs: Vec<SharedString> = panes
            .iter()
            .filter_map(|pane| pane.read(cx).busy_job())
            .collect();
        if jobs.is_empty() {
            then(self, window, cx);
            return;
        }
        let verb = if jobs.len() == 1 { "is" } else { "are" };
        let detail = format!(
            "{} {verb} still running and will be stopped.",
            jobs.join(", ")
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            message,
            Some(&detail),
            &["Close", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| then(this, window, cx));
            }
        })
        .detach();
    }

    fn all_panes(&self) -> Vec<Entity<TerminalPane>> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.tree.leaves().into_iter().cloned())
            .collect()
    }

    /// Hangs up every session, gives shells a moment to exit (and save history), then lets
    /// the reaper SIGKILL stragglers. Blocks the UI for at most ~1 s, only on quit.
    fn shutdown_all(&mut self, cx: &mut Context<Self>) {
        for pane in self.all_panes() {
            pane.update(cx, |pane, _| pane.shutdown());
        }
        if !kil_term::wait_for_children(QUIT_GRACE) {
            kil_term::expedite_reaping();
            if !kil_term::wait_for_children(QUIT_GRACE) {
                log::warn!(
                    "{} session children still running at quit",
                    kil_term::live_children()
                );
            }
        }
    }

    fn should_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.close_confirmed {
            return true;
        }
        let jobs_running = self
            .all_panes()
            .iter()
            .any(|pane| pane.read(cx).busy_job().is_some());
        if !jobs_running {
            self.shutdown_all(cx);
            return true;
        }
        self.confirm_close(
            &self.all_panes(),
            "Close the window?",
            window,
            cx,
            |this, window, cx| {
                this.close_confirmed = true;
                this.shutdown_all(cx);
                window.remove_window();
            },
        );
        false
    }

    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_close(&self.all_panes(), "Quit Kil?", window, cx, |this, _, cx| {
            this.close_confirmed = true;
            this.shutdown_all(cx);
            cx.quit();
        });
    }

    // ── settings ──────────────────────────────────────────────────────────────────────

    /// Also called by the Settings window right after it writes `config.toml`.
    fn reload_config_if_changed(&mut self, window: &Window, cx: &mut Context<Self>) {
        let stamp = config::stamp();
        if stamp != self.config {
            self.config = stamp;
            self.notices = config::apply(cx);
            self.base_font_size = f32::from(cx.global::<Settings>().font_size);
            self.sync_blink(window, cx);
            self.refresh_panes(cx);
        }
    }

    /// Runs one cursor timer, only while blinking is on and the window is active: a timer per
    /// pane woke the app twice a second for every pane, focused or not, and an inactive window
    /// needs none (DESIGN.md A12).
    fn sync_blink(&mut self, window: &Window, cx: &mut Context<Self>) {
        let wanted = cx.global::<Settings>().cursor_blink && window.is_window_active();
        if wanted == self.blink.is_some() {
            return;
        }
        if wanted {
            self.blink = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(BLINK_INTERVAL).await;
                    if this
                        .update_in(cx, |this, window, cx| this.blink_cursor(window, cx))
                        .is_err()
                    {
                        return;
                    }
                }
            }));
        } else {
            self.blink = None;
            if let Some(pane) = self.blinking.take().and_then(|pane| pane.upgrade()) {
                pane.update(cx, TerminalPane::show_cursor);
            }
        }
    }

    fn blink_cursor(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(pane) = self.tabs.get(self.active).map(|tab| tab.focused.clone()) else {
            return;
        };
        if let Some(previous) = self
            .blinking
            .replace(pane.downgrade())
            .and_then(|previous| previous.upgrade())
            && previous != pane
        {
            previous.update(cx, TerminalPane::show_cursor);
        }
        pane.update(cx, |pane, cx| pane.blink(window, cx));
    }

    fn set_font_size(&mut self, change: impl FnOnce(f32) -> f32, cx: &mut Context<Self>) {
        let settings = cx.global::<Settings>();
        let size = change(f32::from(settings.font_size)).clamp(6., 72.);
        let updated = Settings {
            font_size: px(size),
            ..settings.clone()
        };
        cx.set_global(updated);
        self.refresh_panes(cx);
    }

    fn refresh_panes(&mut self, cx: &mut Context<Self>) {
        for pane in self.all_panes() {
            pane.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    // ── rendering ─────────────────────────────────────────────────────────────────────

    /// Collapses finished closes and freezes grids of tabs whose layout is animating.
    fn settle(&mut self, now: Instant, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            for gone in tab.tree.collapse_finished(now) {
                self.pane_subscriptions.remove(&gone.entity_id());
            }
            let animating = tab.tree.is_animating(now);
            if animating != tab.animating {
                tab.animating = animating;
                for pane in tab.tree.leaves() {
                    pane.update(cx, |pane, cx| pane.set_frozen(animating, cx));
                }
            }
        }
    }

    fn render_sidebar(
        &self,
        now: Instant,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = self
            .ghosts
            .iter()
            .filter(|ghost| ghost.after.is_none())
            .map(|ghost| render_ghost(ghost, chrome))
            .collect();
        for (index, tab) in self.tabs.iter().enumerate() {
            rows.push(self.render_row(index, tab, now, chrome, cx));
            rows.extend(
                self.ghosts
                    .iter()
                    .filter(|ghost| ghost.after == Some(tab.id))
                    .map(|ghost| render_ghost(ghost, chrome)),
            );
        }
        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(chrome.sidebar_bg)
            .border_r_1()
            .border_color(chrome.divider)
            .child(div().h(px(SIDEBAR_TOP)).flex_none())
            .child(
                div()
                    .id("tabs")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_2()
                    .flex()
                    .flex_col()
                    // Rows stop their clicks, so this is a double-click on the empty space.
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            this.new_tab(&NewTab, window, cx);
                        }
                    }))
                    .children(rows),
            )
            .child(
                div()
                    .id("new-tab")
                    .mx_2()
                    .mb_2()
                    .h(px(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_lg()
                    .text_size(px(13.))
                    .text_color(chrome.muted)
                    .cursor_pointer()
                    .hover(|style| style.bg(chrome.hover).text_color(chrome.text))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.new_tab(&NewTab, window, cx);
                    }))
                    .child("+")
                    .child("New Tab"),
            )
    }

    fn render_row(
        &self,
        index: usize,
        tab: &Tab,
        now: Instant,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = tab.id;
        let info = tab.focused.read(cx).info().clone();
        let title = tab.title(cx);
        let editing = self
            .rename
            .as_ref()
            .filter(|rename| rename.tab == id)
            .map(|rename| rename.field.clone());
        let status = tab.status(cx);
        let active = index == self.active;
        let pill = active.then(|| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(ROW_HEIGHT * self.pill.value(now)))
                .h(px(ROW_HEIGHT))
                .rounded_lg()
                .bg(chrome.selected)
        });
        div()
            .id(ElementId::NamedInteger("tab".into(), id.0))
            .group("tab-row")
            .relative()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px_3()
            .flex()
            .flex_col()
            .justify_center()
            .rounded_lg()
            .cursor_pointer()
            .when(!active, |row| row.hover(|style| style.bg(chrome.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.activate_id(id, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(id, event.position, window, cx);
                }),
            )
            .children(pill)
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(TITLE_INSET - STATUS_SLOT))
                    .child(status_mark(status, chrome))
                    .child(row_title(title, editing, active, chrome))
                    .child(
                        div()
                            .id(ElementId::NamedInteger("tab-close".into(), id.0))
                            .flex_none()
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_size(px(13.))
                            .text_color(chrome.muted)
                            .opacity(0.)
                            .group_hover("tab-row", |style| style.opacity(1.))
                            .hover(|style| style.bg(chrome.hover).text_color(chrome.text))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.close_tab_id(id, window, cx);
                            }))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .relative()
                    .pl(px(TITLE_INSET))
                    .truncate()
                    .text_size(px(11.))
                    .text_color(chrome.muted)
                    .child(info.cwd),
            )
            .with_animation(
                ElementId::NamedInteger("tab-in".into(), id.0),
                Animation::new(motion::ROW_IN).with_easing(|t| Ease::OutQuint.apply(t)),
                |row, t| row.h(px(ROW_HEIGHT * t)).opacity(t),
            )
            .into_any_element()
    }
}

impl Workspace {
    fn render_menu(
        &self,
        menu: TabMenu,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = menu.tab;
        let named = self
            .tabs
            .iter()
            .any(|tab| tab.id == id && tab.name.is_some());
        let item = |key: &'static str,
                    label: &'static str,
                    run: fn(&mut Self, TabId, &mut Window, &mut Context<Self>),
                    cx: &mut Context<Self>| {
            div()
                .id(key)
                .h(px(26.))
                .px_2()
                .flex()
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|style| style.bg(chrome.accent))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.close_menu(window, cx);
                    run(this, id, window, cx);
                }))
                .child(label)
        };
        let panel = div()
            .id("tab-menu")
            .track_focus(&self.menu_focus)
            .occlude()
            .w(px(180.))
            .p_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(chrome.divider)
            .bg(chrome.banner)
            .shadow_lg()
            .text_size(px(13.))
            .text_color(chrome.text)
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.close_menu(window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }))
            .child(item("menu-rename", "Rename Tab…", Self::start_rename, cx))
            .when(named, |panel| {
                panel.child(item(
                    "menu-reset-name",
                    "Reset Name",
                    |this, id, _, cx| {
                        this.set_name(id, None);
                        cx.notify();
                    },
                    cx,
                ))
            })
            .child(div().my_1().mx_2().h(px(1.)).bg(chrome.divider))
            .child(item("menu-close", "Close Tab", Self::close_tab_id, cx));
        deferred(
            anchored()
                .position(menu.position)
                .snap_to_window_with_margin(px(8.))
                .child(panel),
        )
        .with_priority(1)
    }
}

/// Runs `Quit` in the terminal window, for when another window (Settings) has focus.
pub fn quit_app(cx: &mut App) {
    match cx
        .windows()
        .iter()
        .find_map(AnyWindowHandle::downcast::<Workspace>)
    {
        Some(workspace) => {
            let _ = workspace.update(cx, |this, window, cx| {
                window.activate_window();
                this.quit(&Quit, window, cx);
            });
        }
        None => cx.quit(),
    }
}

/// Re-applies the config files in the terminal window after the Settings window wrote one.
pub fn reload_config(cx: &mut App) {
    for workspace in cx
        .windows()
        .iter()
        .filter_map(AnyWindowHandle::downcast::<Workspace>)
    {
        let _ = workspace.update(cx, |this, window, cx| {
            this.reload_config_if_changed(window, cx);
        });
    }
}

fn render_node(
    node: &Node<Entity<TerminalPane>>,
    tab: &Tab,
    now: Instant,
    chrome: Chrome,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match node {
        Node::Leaf(pane) => render_leaf(pane, tab, chrome),
        Node::Split(split) => render_split(split, tab, now, chrome, cx),
    }
}

fn render_leaf(pane: &Entity<TerminalPane>, tab: &Tab, chrome: Chrome) -> AnyElement {
    let focused = *pane == tab.focused;
    let several = matches!(tab.tree.root(), Node::Split(_));
    let dim = several.then(|| {
        let name = if focused { "focus-in" } else { "focus-out" };
        div().absolute().inset_0().bg(chrome.dim).with_animation(
            ElementId::NamedInteger(name.into(), pane.entity_id().as_u64()),
            Animation::new(motion::FOCUS_DIM),
            move |overlay, t| overlay.opacity(if focused { 1. - t } else { t }),
        )
    });
    div()
        .relative()
        .size_full()
        .overflow_hidden()
        // Cached: output in one pane doesn't re-render its siblings.
        .child(AnyView::from(pane.clone()).cached(StyleRefinement::default().size_full()))
        .children(dim)
        .into_any_element()
}

fn render_split(
    split: &Split<Entity<TerminalPane>>,
    tab: &Tab,
    now: Instant,
    chrome: Chrome,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let ratio = split.ratio.value(now).clamp(0., 1.);
    let horizontal = split.axis == Axis::Horizontal;
    let drag = DividerDrag {
        tab: tab.id,
        split: split.id,
    };
    let first = div()
        .relative()
        .flex_none()
        .overflow_hidden()
        .map(|el| {
            if horizontal {
                el.h_full().w(relative(ratio))
            } else {
                el.w_full().h(relative(ratio))
            }
        })
        .child(render_node(&split.first, tab, now, chrome, cx));
    let second = div()
        .relative()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .child(render_node(&split.second, tab, now, chrome, cx));
    div()
        .relative()
        .flex()
        .size_full()
        .map(|el| {
            if horizontal {
                el.flex_row()
            } else {
                el.flex_col()
            }
        })
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<DividerDrag>, _, cx| {
                let dragged = *event.drag(cx);
                if dragged.tab != drag.tab || dragged.split != drag.split {
                    return;
                }
                let (bounds, position) = (event.bounds, event.event.position);
                let ratio = if horizontal {
                    (position.x - bounds.origin.x) / bounds.size.width
                } else {
                    (position.y - bounds.origin.y) / bounds.size.height
                };
                this.set_ratio(drag.tab, drag.split, ratio, cx);
            }),
        )
        .child(first)
        .child(second)
        .child(divider(drag, horizontal, ratio, chrome))
        .into_any_element()
}

/// The 1px line plus a 7px grab area. Drawn last so the grab area sits above both panes.
fn divider(drag: DividerDrag, horizontal: bool, ratio: f32, chrome: Chrome) -> impl IntoElement {
    let handle = div()
        .id(ElementId::NamedInteger(
            "divider".into(),
            split_id_bits(drag.tab, drag.split),
        ))
        .absolute()
        .map(|el| {
            if horizontal {
                el.top_0()
                    .bottom_0()
                    .left(px(-3.))
                    .w(px(7.))
                    .cursor(CursorStyle::ResizeLeftRight)
            } else {
                el.left_0()
                    .right_0()
                    .top(px(-3.))
                    .h(px(7.))
                    .cursor(CursorStyle::ResizeUpDown)
            }
        })
        .on_drag(drag, |_, _, _, cx| cx.new(|_| EmptyDrag));
    div()
        .absolute()
        .bg(chrome.divider)
        .map(|el| {
            if horizontal {
                el.top_0().bottom_0().left(relative(ratio)).w(px(1.))
            } else {
                el.left_0().right_0().top(relative(ratio)).h(px(1.))
            }
        })
        .child(handle)
}

impl Workspace {
    // ── action handlers ───────────────────────────────────────────────────────────────

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self
            .tabs
            .get(self.active)
            .and_then(|tab| tab.focused.read(cx).current_dir());
        self.open_tab(cwd, window, cx);
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.get(self.active).map(|tab| tab.id) {
            self.close_tab_id(id, window, cx);
        }
    }

    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.tabs.get(self.active).map(|tab| tab.focused.clone()) else {
            return;
        };
        let target = pane.clone();
        self.confirm_close(
            std::slice::from_ref(&pane),
            "Close this pane?",
            window,
            cx,
            move |this, window, cx| this.remove_pane(&target, window, cx),
        );
    }

    fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        if count > 1 {
            let next = if forward {
                (self.active + 1) % count
            } else {
                (self.active + count - 1) % count
            };
            self.activate(next, window, cx);
        }
    }
}

fn split_id_bits(tab: TabId, split: SplitId) -> u64 {
    (tab.0 << 32) ^ split.bits()
}

/// Static on purpose: a spinner would redraw the window every frame for as long as a job
/// (say, a dev server) runs, and idle CPU must stay at zero (DESIGN.md A12).
fn status_mark(status: TabStatus, chrome: Chrome) -> Div {
    let slot = div()
        .flex_none()
        .size(px(STATUS_SLOT))
        .flex()
        .items_center()
        .justify_center();
    match status {
        TabStatus::Idle => slot,
        // A ring: still going.
        TabStatus::Running => slot.child(
            div()
                .size(px(STATUS_SLOT))
                .rounded_full()
                .border_2()
                .border_color(chrome.accent),
        ),
        // A solid dot: something new to look at.
        TabStatus::Done => slot.child(div().size(px(7.)).rounded_full().bg(chrome.accent)),
        TabStatus::Failed => slot
            .text_size(px(12.))
            .font_weight(FontWeight::BOLD)
            .text_color(chrome.failure)
            .child("!"),
    }
}

/// The row's title, or the field editing it.
fn row_title(
    title: SharedString,
    editing: Option<Entity<TextField>>,
    active: bool,
    chrome: Chrome,
) -> Div {
    let title_box = div().flex_1().min_w_0().text_size(px(13.));
    match editing {
        Some(field) => title_box.text_color(chrome.text).child(field),
        None => title_box
            .truncate()
            .text_color(if active { chrome.text } else { chrome.muted })
            .child(title),
    }
}

fn render_ghost(ghost: &Ghost, chrome: Chrome) -> AnyElement {
    div()
        .flex_none()
        .h(px(ROW_HEIGHT))
        .px_3()
        .flex()
        .flex_col()
        .justify_center()
        .overflow_hidden()
        .child(
            div()
                .pl(px(TITLE_INSET))
                .truncate()
                .text_size(px(13.))
                .text_color(chrome.muted)
                .child(ghost.title.clone()),
        )
        .with_animation(
            ElementId::NamedInteger("tab-out".into(), ghost.id.0),
            Animation::new(motion::ROW_OUT).with_easing(|t| Ease::InOut.apply(t)),
            |row, t| row.h(px(ROW_HEIGHT * (1. - t))).opacity(1. - t),
        )
        .into_any_element()
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn bind_actions(root: Stateful<Div>, cx: &mut Context<Workspace>) -> Stateful<Div> {
    root.on_action(cx.listener(Workspace::new_tab))
        .on_action(cx.listener(Workspace::close_tab))
        .on_action(cx.listener(Workspace::close_pane))
        .on_action(cx.listener(Workspace::quit))
        .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
            this.split(Axis::Horizontal, window, cx);
        }))
        .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
            this.split(Axis::Vertical, window, cx);
        }))
        .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
            this.focus_toward(Direction::Left, window, cx);
        }))
        .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
            this.focus_toward(Direction::Right, window, cx);
        }))
        .on_action(cx.listener(|this, _: &FocusUp, window, cx| {
            this.focus_toward(Direction::Up, window, cx);
        }))
        .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
            this.focus_toward(Direction::Down, window, cx);
        }))
        .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle_tab(true, window, cx)))
        .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle_tab(false, window, cx)))
        .on_action(
            cx.listener(|this, _: &FontBigger, _, cx| this.set_font_size(|size| size + 1., cx)),
        )
        .on_action(
            cx.listener(|this, _: &FontSmaller, _, cx| this.set_font_size(|size| size - 1., cx)),
        )
        .on_action(cx.listener(|this, _: &FontReset, _, cx| {
            let base = this.base_font_size;
            this.set_font_size(|_| base, cx);
        }))
}

macro_rules! select_tab_handlers {
    ($($action:ident => $index:expr),* $(,)?) => {
        fn select_tab_listeners(div: Stateful<Div>, cx: &mut Context<Workspace>) -> Stateful<Div> {
            div $(.on_action(cx.listener(|this, _: &$action, window, cx| this.select_tab($index, window, cx))))*
        }
    };
}

select_tab_handlers!(
    SelectTab1 => 0, SelectTab2 => 1, SelectTab3 => 2, SelectTab4 => 3, SelectTab5 => 4,
    SelectTab6 => 5, SelectTab7 => 6, SelectTab8 => 7, SelectTab9 => usize::MAX,
);

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        self.settle(now, cx);
        let active_animating = self
            .tabs
            .get(self.active)
            .is_some_and(|tab| tab.tree.is_animating(now));
        if active_animating || self.pill.is_animating(now) {
            window.request_animation_frame();
        }
        if let Some(tab) = self.tabs.get(self.active) {
            let title = tab.title(cx);
            if title != self.window_title {
                window.set_window_title(&title);
                self.window_title = title;
            }
        }

        let chrome = Chrome::of(cx.global::<Theme>());
        let content = self.tabs.get(self.active).map(|tab| {
            div()
                .relative()
                .size_full()
                .child(render_node(tab.tree.root(), tab, now, chrome, cx))
                .with_animation(
                    ElementId::NamedInteger("tab-show".into(), tab.id.0),
                    Animation::new(motion::TAB_SWITCH).with_easing(|t| Ease::OutQuint.apply(t)),
                    |el, t| el.opacity(t).top(px(6. * (1. - t))),
                )
        });
        let notices = (!self.notices.is_empty()).then(|| {
            div()
                .id("notices")
                .absolute()
                .top_3()
                .right_3()
                .max_w(px(460.))
                .flex()
                .flex_col()
                .gap_2()
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.notices.clear();
                    cx.notify();
                }))
                .children(self.notices.iter().map(|notice| {
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(chrome.banner)
                        .text_size(px(12.))
                        .text_color(chrome.text)
                        .child(notice.clone())
                }))
        });

        let root = div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle);
        let root = select_tab_listeners(bind_actions(root, cx), cx);
        root.flex()
            .size_full()
            .child(self.render_sidebar(now, chrome, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .bg(chrome.terminal_bg)
                    .children(content)
                    .children(notices),
            )
            .children(self.menu.map(|menu| self.render_menu(menu, chrome, cx)))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, MouseUpEvent, TestAppContext, VisualTestContext, point};

    use super::*;
    use crate::text_field;

    const ROW: Point<Pixels> = point(px(100.), px(SIDEBAR_TOP + ROW_HEIGHT / 2.));
    const EMPTY_SIDEBAR: Point<Pixels> = point(px(100.), px(400.));

    fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            cx.set_global(Settings::default());
            cx.bind_keys(text_field::bindings());
        });
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(Vec::new(), window, cx));
        // Rows grow in over `ROW_IN` of wall time; click them at full height.
        std::thread::sleep(motion::ROW_IN + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (workspace, cx)
    }

    fn click(
        cx: &mut VisualTestContext,
        position: Point<Pixels>,
        button: MouseButton,
        count: usize,
    ) {
        for click_count in 1..=count {
            cx.simulate_event(MouseDownEvent {
                position,
                button,
                modifiers: Modifiers::none(),
                click_count,
                first_mouse: false,
            });
            cx.simulate_event(MouseUpEvent {
                position,
                button,
                modifiers: Modifiers::none(),
                click_count,
            });
        }
    }

    fn tab_count(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> usize {
        workspace.read_with(cx, |this, _| this.tabs.len())
    }

    fn first_name(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<SharedString> {
        workspace.read_with(cx, |this, _| this.tabs[0].name.clone())
    }

    /// Right-click the first row, then pick "Rename Tab…" (the menu's first item).
    fn start_rename(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        click(cx, ROW, MouseButton::Right, 1);
        assert!(workspace.read_with(cx, |this, _| this.menu.is_some()));
        click(cx, ROW + point(px(20.), px(18.)), MouseButton::Left, 1);
        assert!(workspace.read_with(cx, |this, _| this.menu.is_none() && this.rename.is_some()));
    }

    #[gpui::test]
    fn double_click_on_empty_sidebar_should_open_a_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        click(cx, EMPTY_SIDEBAR, MouseButton::Left, 2);
        assert_eq!(tab_count(&workspace, cx), 2);
    }

    #[gpui::test]
    fn double_click_on_a_row_should_not_open_a_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        click(cx, ROW, MouseButton::Left, 2);
        assert_eq!(tab_count(&workspace, cx), 1);
    }

    #[gpui::test]
    fn rename_from_the_row_menu_should_name_the_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        start_rename(&workspace, cx);
        cx.simulate_input("构建");
        cx.simulate_keystrokes("enter");
        assert_eq!(first_name(&workspace, cx), Some("构建".into()));
        let title = workspace.read_with(cx, |this, cx| this.tabs[0].title(cx));
        assert_eq!(title, SharedString::from("构建"));
    }

    #[gpui::test]
    fn escape_should_cancel_a_rename(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        start_rename(&workspace, cx);
        cx.simulate_input("x");
        cx.simulate_keystrokes("escape");
        assert_eq!(first_name(&workspace, cx), None);
        assert!(workspace.read_with(cx, |this, _| this.rename.is_none()));
    }

    #[gpui::test]
    fn clicking_a_pane_should_make_it_the_undimmed_one(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(SplitRight);
        std::thread::sleep(motion::SPLIT + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let focused_is_first = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |this, _| {
                let tab = &this.tabs[0];
                tab.focused == *tab.tree.leaves()[0]
            })
        };
        assert!(!focused_is_first(cx), "the new right pane takes focus");
        click(
            cx,
            point(px(SIDEBAR_WIDTH + 100.), px(300.)),
            MouseButton::Left,
            1,
        );
        assert!(focused_is_first(cx));
    }

    fn first_status(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> TabStatus {
        workspace.read_with(cx, |this, cx| this.tabs[0].status(cx))
    }

    fn finish_job_in_first_tab(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        pane.update(cx, |_, cx| cx.emit(PaneEvent::JobFinished));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn job_finishing_in_a_background_tab_should_mark_it_done_until_selected(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(NewTab);
        finish_job_in_first_tab(&workspace, cx);
        assert_eq!(first_status(&workspace, cx), TabStatus::Done);
        cx.dispatch_action(SelectTab1);
        assert_eq!(first_status(&workspace, cx), TabStatus::Idle);
    }

    #[gpui::test]
    fn job_finishing_in_the_tab_in_view_should_not_mark_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        finish_job_in_first_tab(&workspace, cx);
        assert_eq!(first_status(&workspace, cx), TabStatus::Idle);
    }

    #[gpui::test]
    fn blink_timer_should_run_only_while_the_window_is_active(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let blinking =
            |cx: &mut VisualTestContext| workspace.read_with(cx, |this, _| this.blink.is_some());
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(blinking(cx));
        cx.deactivate_window();
        assert!(!blinking(cx));
    }

    #[gpui::test]
    fn escape_should_close_the_row_menu(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        click(cx, ROW, MouseButton::Right, 1);
        cx.simulate_keystrokes("escape");
        assert!(workspace.read_with(cx, |this, _| this.menu.is_none()));
    }
}
