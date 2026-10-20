//! Window root: vertical tab sidebar, the active tab's split tree, and every window-level
//! action (DESIGN.md §4.6, §4.7, §7, §8).

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, AnyWindowHandle, App, ClickEvent, Context,
    CursorStyle, Div, DragMoveEvent, ElementId, Entity, EntityId, FocusHandle, Focusable,
    FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, PromptLevel,
    SharedString, Stateful, StyleRefinement, Subscription, Task, WeakEntity, Window, actions,
    anchored, deferred, div, prelude::*, px, relative,
};
use kil_term::Client;

use crate::{
    config,
    motion::{self, Ease, Tween},
    pane_tree::{Axis, Close, Direction, Node, PaneTree, Split, SplitId},
    restore::{self, PaneIds, Restored, SavedPane, SavedTab, SavedWindow, SavedWorkspace, Store},
    settings::Settings,
    terminal_element::cells,
    terminal_pane::{BLINK_INTERVAL, Launch, PaneEvent, TerminalPane},
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
        QuitAndStopJobs,
    ]
);

const SIDEBAR_WIDTH: f32 = 220.;
const SIDEBAR_TOP: f32 = 52.;
const ROW_HEIGHT: f32 = 44.;
/// The row's status mark; the cwd line indents past it to align with the title.
const STATUS_SLOT: f32 = 10.;
const TITLE_INSET: f32 = STATUS_SLOT + 6.;
/// What closing does to running jobs, for the confirmation.
const STOPPED: &str = "will be stopped";
/// While anything changes, the session is saved at most this often: what a crash can lose.
const SAVE_INTERVAL: Duration = Duration::from_secs(2);
/// While output continues, screens are saved at most this often. A saved screen is for when
/// the session server is lost too (a restart, or the server crashing): a crash of Kil or a
/// quit leaves the jobs and their screens in the server, and quitting saves screens at once.
/// Each save of a long colored scrollback holds that pane's terminal for milliseconds.
const SCREEN_INTERVAL: Duration = Duration::from_secs(30);
/// A crash within this long after reopening is blamed on the re-run jobs (DESIGN.md §4.9).
const RESTORE_SETTLE: Duration = Duration::from_secs(10);
/// How often the panes' memory is read, for the sidebar's figures (DESIGN.md §5.2).
const MEMORY_INTERVAL: Duration = Duration::from_secs(2);
/// A tab's figure is shown from this much on; below it, the row stays quiet.
const SHOWN_MEMORY: u64 = 100 * 1024 * 1024;
/// From this much on, the figure is shown as a warning.
const HEAVY_MEMORY: u64 = 512 * 1024 * 1024;

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
    /// What this tab's panes cost the machine; read every [`MEMORY_INTERVAL`].
    memory: u64,
}

/// What the panes cost, and how the system is doing (DESIGN.md §5.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MemoryState {
    /// Resident memory of every pane's program, plus their grids.
    total: u64,
    /// How hard the system is pressed for memory.
    pressure: kil_term::system::Pressure,
    /// Swap in use, in bytes.
    swap_used: u64,
    /// Swap in all, in bytes.
    swap_total: u64,
}

impl MemoryState {
    /// The footer's swap line, empty while the system is not under pressure.
    fn swap_line(&self) -> String {
        if self.pressure == kil_term::system::Pressure::Normal {
            String::new()
        } else {
            ratio_label(self.swap_used, self.swap_total)
        }
    }
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

/// What leaving the window does to the sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leave {
    /// They keep running in the server, for the next launch to go back on.
    Detach,
    /// They end, and the next launch runs their jobs again (DESIGN.md §4.9).
    Stop,
}

/// One save of the session, taken on the UI thread and writable from any.
struct Snapshot {
    store: Store,
    ticket: u64,
    saved: SavedWorkspace,
    screens: Vec<(String, kil_term::Transcript)>,
}

impl Snapshot {
    fn write(&self) {
        if let Err(err) = self.store.save(self.ticket, &self.saved, &self.screens) {
            log::error!("{err}");
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

/// A sidebar row being dragged to another place; the row itself moves under the pointer.
#[derive(Clone, Copy)]
struct TabDrag(TabId);

struct EmptyDrag;

/// The sidebar, drawn as a view of its own: output in a pane or a cursor blink redraws the
/// window, and this view is then reused instead of laying out and painting every row again.
/// Everything it shows is the workspace's, which is notified when any of it changes.
struct Sidebar {
    workspace: WeakEntity<Workspace>,
    #[cfg(test)]
    renders: usize,
    _workspace_changed: Subscription,
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.render_sidebar(window, cx).into_any_element()
            })
            .unwrap_or_else(|_| div().into_any_element())
    }
}

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
    /// The session server, which runs every pane's program (DESIGN.md §14).
    client: Arc<Client>,
    /// Where the session is saved for the next launch; `None` saves nothing (tests).
    store: Option<Store>,
    /// The window is leaving: what the next launch reopens is saved, or (after the last tab
    /// closed) cleared, and nothing may overwrite it.
    ended: bool,
    /// The next throttled save, and when it runs; see [`Self::save_in`].
    save_task: Option<(Instant, Task<()>)>,
    /// When the screens were last taken for saving; `None` until they first are.
    screens_saved: Option<Instant>,
    pane_ids: PaneIds,
    frame: SavedWindow,
    window_title: SharedString,
    /// Font size from the config file, for `font_reset`.
    base_font_size: f32,
    sidebar: Entity<Sidebar>,
    /// The window's one cursor-blink timer; see [`Self::sync_blink`].
    blink: Option<Task<()>>,
    /// The pane it last blinked, so a pane that loses focus is left with its cursor drawn.
    blinking: Option<WeakEntity<TerminalPane>>,
    /// The timer that reads what the panes cost; see [`Self::refresh_memory`].
    memory_task: Option<Task<()>>,
    /// What the panes cost, and how the system is doing.
    memory: MemoryState,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(
        notices: Vec<SharedString>,
        client: Arc<Client>,
        store: Option<Store>,
        restored: Option<Restored>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        // A system logout or restart terminates the app without our Quit flow.
        let quitting = cx.on_app_quit(|this, cx| {
            this.save_now(cx);
            async {}
        });
        let moved = cx.observe_window_bounds(window, |this, window, cx| {
            this.frame = SavedWindow::of(window.window_bounds());
            this.schedule_save(cx);
        });
        // What a restore would show changes only with the workspace's own state, which
        // notifies it. Not on every redraw: a cursor blink or a pane's output redraws the
        // window, and saving for that kept an idle window saving every 2 s.
        let changed = cx.observe_self(Self::schedule_save);
        let workspace = cx.entity();
        let sidebar = cx.new(|cx| Sidebar {
            workspace: workspace.downgrade(),
            #[cfg(test)]
            renders: 0,
            _workspace_changed: cx.observe(&workspace, |_, _, cx| cx.notify()),
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
            client,
            store: None,
            ended: false,
            save_task: None,
            screens_saved: None,
            pane_ids: PaneIds::new(),
            frame: SavedWindow::of(window.window_bounds()),
            window_title: SharedString::default(),
            base_font_size: f32::from(cx.global::<Settings>().font_size),
            sidebar,
            blink: None,
            blinking: None,
            memory_task: None,
            memory: MemoryState::default(),
            _subscriptions: vec![activation, quitting, moved, changed],
        };
        if let Some(store) = store.clone() {
            // From here on, a crash is not the reopened jobs' doing.
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(RESTORE_SETTLE).await;
                store.settled();
            })
            .detach();
        }
        this.store = store;
        let mut live: HashSet<String> = match this.client.sessions() {
            Ok(ids) => ids.into_iter().collect(),
            Err(err) => {
                this.notices
                    .push(format!("Lost Kil's session server: {err}").into());
                HashSet::new()
            }
        };
        let reopened =
            restored.is_some_and(|restored| this.restore(restored, &mut live, window, cx));
        this.adopt(live, window, cx);
        if !reopened && this.tabs.is_empty() {
            let cwd = cx.global::<Settings>().default_directory.clone();
            this.open_tab(cwd, window, cx);
        } else if !reopened {
            this.activate(0, window, cx);
        }
        this.sync_blink(window, cx);
        this.start_memory_watch(cx);
        this
    }

    /// Opens a tab for each session the server runs that no saved tab claimed, e.g. after
    /// the layout was lost: a running job is never left out of sight (DESIGN.md §14.3).
    fn adopt(&mut self, live: HashSet<String>, window: &mut Window, cx: &mut Context<Self>) {
        let mut ids: Vec<_> = live
            .into_iter()
            .filter(|id| restore::is_valid_id(id))
            .collect();
        ids.sort();
        for id in ids {
            let launch = Launch {
                id,
                attach: true,
                ..Launch::default()
            };
            self.push_tab(launch, window, cx);
        }
    }

    /// Reopens the tabs of the last launch; false if there were none. A pane whose session
    /// the server still runs (in `live`) goes back on it (DESIGN.md §14); any other shows its
    /// old screen and runs its job again (§4.9).
    fn restore(
        &mut self,
        restored: Restored,
        live: &mut HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Restored { saved, rerun } = restored;
        let store = self.store.clone();
        let mut seen = std::collections::HashSet::new();
        let mut held_back = false;
        for tab in saved.tabs {
            let shape = tab.root.map(&mut |pane: SavedPane| {
                let id = if restore::is_valid_id(&pane.id) && seen.insert(pane.id.clone()) {
                    pane.id
                } else {
                    self.pane_ids.fresh()
                };
                let attach = live.remove(&id);
                held_back |= !attach && !rerun && pane.command.is_some();
                let launch = Launch {
                    replay: store
                        .as_ref()
                        .map(|store| store.transcript(&id))
                        .unwrap_or_default(),
                    id,
                    attach,
                    cwd: pane.cwd.map(PathBuf::from),
                    rerun: pane.command.filter(|_| rerun && !attach),
                };
                self.new_pane(launch, window, cx)
            });
            let tree = PaneTree::from_shape(shape);
            let leaves = tree.leaves();
            let Some(focused) = leaves
                .get(tab.focused)
                .or(leaves.first())
                .map(|&pane| pane.clone())
            else {
                continue;
            };
            let id = TabId(self.next_tab);
            self.next_tab += 1;
            self.tabs.push(Tab {
                id,
                tree,
                focused,
                name: tab
                    .name
                    .filter(|name| !name.trim().is_empty())
                    .map(Into::into),
                finished: false,
                animating: false,
                memory: 0,
            });
        }
        if held_back {
            self.notices.push(
                "Kil quit while reopening these tabs last time, so their jobs were not run again."
                    .into(),
            );
        }
        let Some(last) = self.tabs.len().checked_sub(1) else {
            return false;
        };
        // Set first, so the selection pill starts on the row instead of sliding to it.
        self.active = saved.active.min(last);
        self.activate(self.active, window, cx);
        true
    }

    /// Saves soon, at most once per [`SAVE_INTERVAL`] however much changes, so a crash or a
    /// forced quit loses little. Writing happens off the UI thread.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_in(SAVE_INTERVAL, cx);
    }

    /// Saves the screens that output changed, at most once per [`SCREEN_INTERVAL`].
    fn schedule_screens(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let wait = self.screens_saved.map_or(Duration::ZERO, |at| {
            SCREEN_INTERVAL.saturating_sub(now.saturating_duration_since(at))
        });
        self.save_in(wait.max(SAVE_INTERVAL), cx);
    }

    /// Saves in `delay`, or sooner if a save is already due sooner.
    fn save_in(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if self.store.is_none() || self.ended {
            return;
        }
        let at = cx.background_executor().now() + delay;
        if self.save_task.as_ref().is_some_and(|(due, _)| *due <= at) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.save_task = None;
                if let Some(snapshot) = this.snapshot(false, cx) {
                    cx.background_spawn(async move { snapshot.write() })
                        .detach();
                }
            });
        });
        self.save_task = Some((at, task));
    }

    /// Saves on this thread, for when the window closes or the app quits: before the sessions
    /// end, since it is their jobs that get saved.
    fn save_now(&mut self, cx: &mut Context<Self>) {
        self.save_task = None;
        if let Some(snapshot) = self.snapshot(true, cx) {
            snapshot.write();
            snapshot.store.settled();
        }
    }

    /// What the next launch needs, taken now; `None` once the sessions have ended. Screens
    /// that changed are taken if `all_screens`, or once [`SCREEN_INTERVAL`] has passed since
    /// they last were; until then they wait for a later save.
    fn snapshot(&mut self, all_screens: bool, cx: &mut Context<Self>) -> Option<Snapshot> {
        let store = self.store.clone().filter(|_| !self.ended)?;
        let now = cx.background_executor().now();
        let screens_due = all_screens
            || self
                .screens_saved
                .is_none_or(|at| now.saturating_duration_since(at) >= SCREEN_INTERVAL);
        if screens_due {
            self.screens_saved = Some(now);
        }
        let ticket = store.ticket();
        let mut screens = Vec::new();
        let mut waiting = false;
        let mut tabs = Vec::with_capacity(self.tabs.len());
        for tab in &self.tabs {
            let root = tab.tree.shape().map(&mut |pane: &Entity<TerminalPane>| {
                pane.update(cx, |pane, _| {
                    if screens_due {
                        screens.extend(pane.take_screen());
                    } else {
                        waiting |= pane.screen_changed();
                    }
                    pane.saved()
                })
            });
            tabs.push(SavedTab {
                name: tab.name.as_ref().map(ToString::to_string),
                focused: tab
                    .tree
                    .leaves()
                    .iter()
                    .position(|&pane| *pane == tab.focused)
                    .unwrap_or(0),
                root,
            });
        }
        if waiting {
            self.schedule_screens(cx);
        }
        let saved = SavedWorkspace {
            active: self.active,
            window: Some(self.frame),
            tabs,
        };
        Some(Snapshot {
            store,
            ticket,
            saved,
            screens,
        })
    }

    // ── tabs ──────────────────────────────────────────────────────────────────────────

    fn new_pane(
        &mut self,
        launch: Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let client = Arc::clone(&self.client);
        let pane = cx.new(|cx| TerminalPane::new(launch, client, cx));
        let subscription = cx.subscribe_in(&pane, window, Self::on_pane_event);
        self.pane_subscriptions
            .insert(pane.entity_id(), subscription);
        pane
    }

    fn fresh_pane(
        &mut self,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let launch = Launch {
            id: self.pane_ids.fresh(),
            cwd,
            ..Launch::default()
        };
        self.new_pane(launch, window, cx)
    }

    fn open_tab(&mut self, cwd: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let launch = Launch {
            id: self.pane_ids.fresh(),
            cwd,
            ..Launch::default()
        };
        self.push_tab(launch, window, cx);
        self.activate(self.tabs.len() - 1, window, cx);
    }

    /// Adds a tab of one pane after the others, without selecting it.
    fn push_tab(&mut self, launch: Launch, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.new_pane(launch, window, cx);
        let id = TabId(self.next_tab);
        self.next_tab += 1;
        self.tabs.push(Tab {
            id,
            tree: PaneTree::new(pane.clone()),
            focused: pane,
            name: None,
            finished: false,
            animating: false,
            memory: 0,
        });
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
    /// Puts tab `id` where tab `to` is, shifting the rows between; the same tab stays active.
    fn move_tab(&mut self, id: TabId, to: TabId, cx: &mut Context<Self>) {
        let index = |tabs: &[Tab], id| tabs.iter().position(|tab| tab.id == id);
        let (Some(from), Some(to)) = (index(&self.tabs, id), index(&self.tabs, to)) else {
            return;
        };
        let active = self.tabs[self.active].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = index(&self.tabs, active).unwrap_or(0);
        cx.notify();
    }

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
            STOPPED,
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
            // Nothing is left to reopen: the next launch starts fresh. Every pane's session
            // was closed with its tab, so the server exits once it is idle.
            if let Some(store) = &self.store {
                store.clear();
            }
            self.ended = true;
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
            PaneEvent::Output => self.schedule_screens(cx),
        }
    }

    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.tabs.get(self.active).map(|tab| tab.focused.clone()) else {
            return;
        };
        let cwd = target.read(cx).current_dir();
        let pane = self.fresh_pane(cwd, window, cx);
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

    /// A divider was double-clicked: every pane in its tab gets an equal share again.
    fn equalize(&mut self, tab: TabId, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab) {
            tab.tree.equalize(Instant::now());
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

    /// Runs `then` now if no pane in `panes` has a foreground job; otherwise asks first,
    /// saying what happens to the jobs (DESIGN.md §4.7).
    fn confirm_close(
        &mut self,
        panes: &[Entity<TerminalPane>],
        message: &'static str,
        outcome: &'static str,
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
        let detail = format!("{} {verb} still running and {outcome}.", jobs.join(", "));
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

    /// Ends every session in the server; it gives programs a moment to exit (and save
    /// history), then has the reaper SIGKILL stragglers. Blocks the UI for at most ~1 s, only
    /// when the user asked for it.
    fn stop_all(&mut self, cx: &mut Context<Self>) {
        for pane in self.all_panes() {
            pane.update(cx, |pane, _| pane.shutdown());
        }
        if let Err(err) = self.client.stop_all() {
            log::error!("stopping the jobs failed: {err}");
        }
    }

    /// Asks how to leave if jobs are running: by default they keep running in the server and
    /// are back when Kil reopens (DESIGN.md A20). With none running, the sessions are left to
    /// the server without asking.
    fn leave(
        &mut self,
        message: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Leave, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let jobs: Vec<SharedString> = self
            .all_panes()
            .iter()
            .filter_map(|pane| pane.read(cx).busy_job())
            .collect();
        if jobs.is_empty() {
            then(self, Leave::Detach, window, cx);
            return;
        }
        let verb = if jobs.len() == 1 { "keeps" } else { "keep" };
        let detail = format!(
            "{} {verb} running, and will be here when Kil reopens.",
            jobs.join(", ")
        );
        let answer = window.prompt(
            PromptLevel::Info,
            message,
            Some(&detail),
            &["Quit", "Stop Jobs and Quit", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let leave = match answer.await {
                Ok(0) => Leave::Detach,
                Ok(1) => Leave::Stop,
                _ => return,
            };
            let _ = this.update_in(cx, |this, window, cx| then(this, leave, window, cx));
        })
        .detach();
    }

    /// Saves what the next launch reopens, then lets go of the sessions or ends them.
    fn depart(&mut self, leave: Leave, cx: &mut Context<Self>) {
        self.close_confirmed = true;
        self.save_now(cx);
        self.ended = true;
        if leave == Leave::Stop {
            self.stop_all(cx);
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
            self.depart(Leave::Detach, cx);
            return true;
        }
        self.leave("Close Kil?", window, cx, |this, leave, window, cx| {
            this.depart(leave, cx);
            window.remove_window();
        });
        false
    }

    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.leave("Quit Kil?", window, cx, |this, leave, _, cx| {
            this.depart(leave, cx);
            cx.quit();
        });
    }

    fn quit_and_stop(&mut self, _: &QuitAndStopJobs, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_close(
            &self.all_panes(),
            "Quit Kil and stop all jobs?",
            "will be stopped, then run again when Kil reopens",
            window,
            cx,
            |this, _, cx| {
                this.depart(Leave::Stop, cx);
                cx.quit();
            },
        );
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
            pane.update(cx, TerminalPane::restyle);
        }
        cx.notify();
    }

    /// Reads what every pane costs, and how the system is doing (DESIGN.md §5.2).
    ///
    /// A program in a pane can take gigabytes, and the system charges them to the app that
    /// started it: the sidebar's figures are how the user finds the hog before macOS does.
    fn refresh_memory(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut total = 0;
        for tab in &mut self.tabs {
            let mut sum = 0;
            for pane in tab.tree.leaves() {
                sum += pane.read(cx).memory();
            }
            // The bytes change at every reading of any running program; what the row shows
            // seldom does, and a redraw for nothing is what keeps an idle window awake.
            changed |= shown_memory(sum) != shown_memory(tab.memory);
            tab.memory = sum;
            total += sum;
        }
        let system = kil_term::system::memory();
        let state = MemoryState {
            total,
            pressure: system.pressure,
            swap_used: system.swap_used,
            swap_total: system.swap_total,
        };
        changed |= shown_memory(state.total) != shown_memory(self.memory.total)
            || state.swap_line() != self.memory.swap_line();
        self.memory = state;
        // Only redraw when a figure on screen changed: the window sleeps otherwise.
        if changed {
            cx.notify();
        }
    }

    /// Reads what the panes cost, every [`MEMORY_INTERVAL`].
    fn start_memory_watch(&mut self, cx: &mut Context<Self>) {
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(MEMORY_INTERVAL).await;
                if this.update(cx, Workspace::refresh_memory).is_err() {
                    break;
                }
            }
        }));
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

    /// What [`Sidebar`] draws.
    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        if self.pill.is_animating(now) {
            // Asked from the sidebar's render, so it is the sidebar that draws the next frame.
            window.request_animation_frame();
        }
        let chrome = Chrome::of(cx.global::<Theme>());
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
            // The title bar the window doesn't draw: double-clicking it zooms, as the user set
            // in System Settings (fill the screen, then back), or minimizes.
            .child(
                div()
                    .id("titlebar")
                    .h(px(SIDEBAR_TOP))
                    .flex_none()
                    .on_click(|event: &ClickEvent, window, _| {
                        if event.click_count() == 2 {
                            window.titlebar_double_click();
                        }
                    }),
            )
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
            .child(memory_footer(self.memory, chrome))
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
        let row = div()
            .id(ElementId::NamedInteger("tab".into(), id.0))
            .group("tab-row");
        // Not while renaming: a drag in the field selects its text.
        reorderable(row, id, editing.is_none(), cx)
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
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                // Clicks inside the title being edited belong to the field.
                if this.rename.as_ref().is_some_and(|rename| rename.tab == id) {
                    return;
                }
                if event.click_count() == 2 {
                    this.start_rename(id, window, cx);
                } else {
                    this.activate_id(id, window, cx);
                }
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
                    .when_some(shown_memory(tab.memory), |row, shown| {
                        row.child(memory_label(shown, chrome))
                    })
                    .child(tab_close(id, chrome, cx)),
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
    in_terminal_window(cx, |this, window, cx| this.quit(&Quit, window, cx));
}

/// Quits without asking about running jobs: they keep running, and the next launch is back
/// on them (Restart to Update).
pub fn detach_and_quit(cx: &mut App) {
    in_terminal_window(cx, |this, _, cx| {
        this.depart(Leave::Detach, cx);
        cx.quit();
    });
}

/// Runs `QuitAndStopJobs` in the terminal window, for when another window has focus.
pub fn quit_and_stop_app(cx: &mut App) {
    in_terminal_window(cx, |this, window, cx| {
        this.quit_and_stop(&QuitAndStopJobs, window, cx);
    });
}

fn in_terminal_window(
    cx: &mut App,
    run: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>),
) {
    match cx
        .windows()
        .iter()
        .find_map(AnyWindowHandle::downcast::<Workspace>)
    {
        Some(workspace) => {
            let _ = workspace.update(cx, |this, window, cx| {
                window.activate_window();
                run(this, window, cx);
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
        Node::Leaf(pane) => render_leaf(pane, tab, chrome, cx),
        Node::Split(split) => render_split(split, tab, now, chrome, cx),
    }
}

fn render_leaf(pane: &Entity<TerminalPane>, tab: &Tab, chrome: Chrome, cx: &App) -> AnyElement {
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
        // A sibling, not a child, of the pane's view: a blink redraws only the cursor.
        .child(
            AnyView::from(pane.read(cx).cursor().clone())
                .cached(StyleRefinement::default().absolute().inset_0()),
        )
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
        .child(divider(drag, horizontal, ratio, chrome, cx))
        .into_any_element()
}

/// The 1px line plus a 7px grab area. Drawn last so the grab area sits above both panes.
/// Dragging it moves the divider; double-clicking it shares the tab equally again.
fn divider(
    drag: DividerDrag,
    horizontal: bool,
    ratio: f32,
    chrome: Chrome,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
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
        .debug_selector(|| "divider".to_owned())
        .on_drag(drag, |_, _, _, cx| cx.new(|_| EmptyDrag))
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if event.click_count() == 2 {
                this.equalize(drag.tab, cx);
            }
        }));
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

    /// In the default directory if one is set (Settings); otherwise where the focused pane
    /// is. Splits always follow their pane.
    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = cx
            .global::<Settings>()
            .default_directory
            .clone()
            .or_else(|| {
                self.tabs
                    .get(self.active)
                    .and_then(|tab| tab.focused.read(cx).current_dir())
            });
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
            STOPPED,
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

/// Lets `row` be dragged (when `draggable`) and takes the dragged tab's place when the pointer
/// passes over it.
fn reorderable(
    row: Stateful<Div>,
    id: TabId,
    draggable: bool,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    row.when(draggable, |row| {
        row.on_drag(TabDrag(id), |_, _, _, cx| cx.new(|_| EmptyDrag))
    })
    .on_drag_move(
        cx.listener(move |this, event: &DragMoveEvent<TabDrag>, _, cx| {
            let TabDrag(dragged) = *event.drag(cx);
            let (y, row) = (event.event.position.y, event.bounds);
            // Rows are one height, so the dragged row lands under the pointer and stays.
            if dragged != id && row.top() <= y && y < row.bottom() {
                this.move_tab(dragged, id, cx);
            }
        }),
    )
}

/// A memory figure as the sidebar shows it: its label, and whether it warns. `None` below
/// [`SHOWN_MEMORY`], where the row stays quiet.
fn shown_memory(bytes: u64) -> Option<(String, bool)> {
    (bytes >= SHOWN_MEMORY).then(|| (byte_label(bytes), bytes >= HEAVY_MEMORY))
}

/// A tab's memory figure, in the row's title line.
fn memory_label((label, heavy): (String, bool), chrome: Chrome) -> Div {
    div()
        .flex_none()
        .text_size(px(11.))
        .text_color(if heavy { chrome.failure } else { chrome.muted })
        .child(label)
}

/// The × of a tab row, shown while the pointer is over it.
fn tab_close(id: TabId, chrome: Chrome, cx: &mut Context<Workspace>) -> Stateful<Div> {
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
        .child("×")
}

/// A byte count as a short label: "1.2 GB", "340 MB", "8 KB".
fn byte_label(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        let tenths = bytes.saturating_mul(10) / GB;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else if bytes >= MB {
        format!("{} MB", bytes / MB)
    } else {
        format!("{} KB", bytes / KB)
    }
}

/// Two byte counts as "1.8/3.0 GB", the way a gauge reads.
fn ratio_label(used: u64, total: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;
    if total >= GB {
        let (used, total) = (used.saturating_mul(10) / GB, total.saturating_mul(10) / GB);
        format!(
            "{}.{}/{}.{} GB",
            used / 10,
            used % 10,
            total / 10,
            total % 10
        )
    } else {
        format!("{}/{} MB", used / MB, total / MB)
    }
}

/// The sidebar's line about what the panes cost, and how the system is doing.
///
/// Nothing is shown while there is nothing to say: a quiet sidebar is the point.
fn memory_footer(memory: MemoryState, chrome: Chrome) -> Div {
    let mut row = div().mx_2().flex().gap_2().text_size(px(11.));
    let mut shown = false;
    if let Some((label, _)) = shown_memory(memory.total) {
        row = row.child(
            div()
                .text_color(chrome.muted)
                .child(format!("Tabs {label}")),
        );
        shown = true;
    }
    if memory.pressure != kil_term::system::Pressure::Normal {
        row = row.child(div().text_color(chrome.failure).child(format!(
            "⚠ swap {}",
            ratio_label(memory.swap_used, memory.swap_total)
        )));
        shown = true;
    }
    if shown {
        row = row.mb_1();
    }
    row
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
        .on_action(cx.listener(Workspace::quit_and_stop))
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
        if active_animating {
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
            .child(
                AnyView::from(self.sidebar.clone()).cached(
                    StyleRefinement::default()
                        .w(px(SIDEBAR_WIDTH))
                        .h_full()
                        .flex_none(),
                ),
            )
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
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        Modifiers, MouseUpEvent, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
        VisualTestContext, point,
    };

    use super::*;
    use crate::text_field;

    const ROW: Point<Pixels> = point(px(100.), px(SIDEBAR_TOP + ROW_HEIGHT / 2.));
    const EMPTY_SIDEBAR: Point<Pixels> = point(px(100.), px(400.));

    fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        workspace_saving_to(None, cx)
    }

    /// A workspace that reopens what `store` holds and saves there.
    fn workspace_saving_to(
        store: Option<Store>,
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        workspace_on(test_client(), store, cx)
    }

    /// A session server of the test's own, on a thread of the test process.
    fn test_client() -> Arc<Client> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kil-wst-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        kil_term::server::serve_in_thread(&dir).unwrap();
        Arc::new(Client::connect(&dir).unwrap())
    }

    /// A workspace whose panes run in `client`'s server, reopening what `store` holds.
    fn workspace_on(
        client: Arc<Client>,
        store: Option<Store>,
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        let restored = store.as_ref().and_then(|store| store.load().unwrap());
        cx.update(|cx| {
            cx.set_global(Theme::default());
            cx.set_global(Settings::default());
            cx.bind_keys(text_field::bindings());
        });
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            Workspace::new(Vec::new(), client, store, restored, window, cx)
        });
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
    fn a_pane_should_report_what_it_costs(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        // The session starts in the test server; give it a moment to come up.
        let mut memory = 0;
        for _ in 0..50 {
            cx.run_until_parked();
            workspace.update(cx, Workspace::refresh_memory);
            memory = workspace.read_with(cx, |this, _| this.tabs[0].memory);
            if memory > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(memory > 0, "the pane's program is not counted");
    }

    #[gpui::test]
    fn memory_should_redraw_only_when_a_shown_figure_changes(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let notified = Rc::new(Cell::new(0));
        let count = Rc::clone(&notified);
        let _watch =
            cx.update(|_, cx| cx.observe(&workspace, move |_, _| count.set(count.get() + 1)));
        workspace.update(cx, Workspace::refresh_memory);
        cx.run_until_parked();
        notified.set(0);
        // A program's resident memory moved by a few bytes, far below what the sidebar shows.
        workspace.update(cx, |this, cx| {
            this.tabs[0].memory += 4096;
            this.refresh_memory(cx);
        });
        cx.run_until_parked();
        assert_eq!(notified.get(), 0);
        // Over the threshold, the row shows the figure: that is a change.
        workspace.update(cx, |this, cx| {
            this.tabs[0].memory = SHOWN_MEMORY * 2;
            this.refresh_memory(cx);
        });
        cx.run_until_parked();
        assert_eq!(notified.get(), 1);
    }

    #[test]
    fn byte_label_should_scale_to_the_unit() {
        assert_eq!(byte_label(0), "0 KB");
        assert_eq!(byte_label(8 * 1024 * 1024), "8 MB");
        assert_eq!(byte_label(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }

    #[test]
    fn ratio_label_should_read_as_a_gauge() {
        let gb = 1024 * 1024 * 1024;
        assert_eq!(ratio_label(3 * gb / 2, 3 * gb), "1.5/3.0 GB");
        assert_eq!(
            ratio_label(64 * 1024 * 1024, 128 * 1024 * 1024),
            "64/128 MB"
        );
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
    fn double_click_on_a_row_should_rename_it_and_enter_should_save(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        click(cx, ROW, MouseButton::Left, 2);
        assert!(workspace.read_with(cx, |this, _| this.rename.is_some()));
        cx.simulate_input("日志");
        cx.simulate_keystrokes("enter");
        assert_eq!(first_name(&workspace, cx), Some("日志".into()));
    }

    #[gpui::test]
    fn a_rename_should_be_saved_when_its_field_loses_focus(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        // GPUI tells views about focus leaving only in the active window.
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        click(cx, ROW, MouseButton::Left, 2);
        cx.simulate_input("x");
        // Into the terminal: the field loses focus.
        click(
            cx,
            point(px(SIDEBAR_WIDTH + 100.), px(300.)),
            MouseButton::Left,
            1,
        );
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |this, _| this.rename.is_none()));
        assert_ne!(first_name(&workspace, cx), None);
    }

    #[gpui::test]
    fn new_tabs_should_open_in_the_default_directory_and_splits_follow_their_pane(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx);
        let home = std::path::PathBuf::from("/usr");
        cx.update(|_, cx| {
            let settings = Settings {
                default_directory: Some(home.clone()),
                ..cx.global::<Settings>().clone()
            };
            cx.set_global(settings);
        });
        let started_in = |workspace: &Entity<Workspace>, cx: &mut VisualTestContext| {
            workspace.read_with(cx, |this, cx| {
                this.tabs[this.active].focused.read(cx).start_dir().cloned()
            })
        };
        // The first tab's shell is not in the default directory, and its split follows it.
        cx.dispatch_action(SplitRight);
        assert_ne!(started_in(&workspace, cx), Some(home.clone()));
        cx.dispatch_action(NewTab);
        assert_eq!(started_in(&workspace, cx), Some(home));
    }

    #[gpui::test]
    fn dragging_a_row_should_move_its_tab_and_keep_the_active_one(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(NewTab);
        cx.dispatch_action(NewTab);
        workspace.update(cx, |this, _| {
            for (tab, name) in this.tabs.iter_mut().zip(["a", "b", "c"]) {
                tab.name = Some(name.into());
            }
        });
        std::thread::sleep(motion::ROW_IN + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        // Row "a" down past "b" onto "c", in small moves as a hand would.
        cx.simulate_event(MouseDownEvent {
            position: ROW,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        });
        for step in (1..=8u8).map(f32::from) {
            cx.simulate_event(gpui::MouseMoveEvent {
                position: ROW + point(px(0.), px(ROW_HEIGHT * 2. * step / 8.)),
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers::none(),
            });
        }
        cx.simulate_event(MouseUpEvent {
            position: ROW + point(px(0.), px(ROW_HEIGHT * 2.)),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        let (names, active) = workspace.read_with(cx, |this, _| {
            let names: Vec<_> = this
                .tabs
                .iter()
                .filter_map(|tab| tab.name.clone())
                .collect();
            (names, this.tabs[this.active].name.clone())
        });
        assert_eq!(
            (names, active),
            (vec!["b".into(), "c".into(), "a".into()], Some("c".into()))
        );
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
    fn the_default_keys_should_split_and_close_panes(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.update(|_, cx| {
            let (table, errors) = crate::keymap::resolve(None);
            assert_eq!(errors, []);
            cx.bind_keys(crate::keymap::bindings(&table));
        });
        let panes = |cx: &mut VisualTestContext| {
            cx.update(|_, _| {});
            workspace.read_with(cx, |this, _| this.tabs[this.active].tree.leaves().len())
        };
        // cmd-shift-d splits right, cmd-d splits down, cmd-shift-c closes the pane.
        cx.simulate_keystrokes("cmd-shift-d");
        assert_eq!(panes(cx), 2);
        cx.simulate_keystrokes("cmd-d");
        assert_eq!(panes(cx), 3);
        cx.simulate_keystrokes("cmd-shift-c");
        std::thread::sleep(motion::CLOSE_PANE + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert_eq!(panes(cx), 2);
    }

    #[gpui::test]
    fn panes_should_share_a_tab_evenly_again_after_a_pane_closes(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(SplitRight);
        cx.dispatch_action(SplitDown);
        // The user drags a divider, leaving the tab lopsided.
        workspace.update(cx, |this, _| {
            let id = match this.tabs[this.active].tree.root() {
                Node::Split(split) => split.id,
                Node::Leaf(_) => unreachable!(),
            };
            this.tabs[this.active].tree.set_ratio(id, 0.3);
        });
        cx.dispatch_action(ClosePane);
        workspace.read_with(cx, |this, _| match this.tabs[this.active].tree.shape() {
            crate::pane_tree::Shape::Split { ratio, .. } => {
                assert!((ratio - 0.5).abs() < f32::EPSILON, "ratio {ratio}");
            }
            shape @ crate::pane_tree::Shape::Leaf(_) => panic!("{shape:?}"),
        });
    }

    #[gpui::test]
    fn double_clicking_a_divider_should_share_the_tab_equally_again(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(SplitRight);
        std::thread::sleep(motion::SPLIT + Duration::from_millis(20));
        workspace.update(cx, |this, _| {
            let id = match this.tabs[this.active].tree.root() {
                Node::Split(split) => split.id,
                Node::Leaf(_) => unreachable!(),
            };
            this.tabs[this.active].tree.set_ratio(id, 0.3);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let divider = cx.debug_bounds("divider").expect("divider drawn");
        click(cx, divider.center(), MouseButton::Left, 2);
        workspace.read_with(cx, |this, _| match this.tabs[this.active].tree.shape() {
            crate::pane_tree::Shape::Split { ratio, .. } => {
                assert!((ratio - 0.5).abs() < f32::EPSILON, "ratio {ratio}");
            }
            shape @ crate::pane_tree::Shape::Leaf(_) => panic!("{shape:?}"),
        });
    }

    /// Polls `done` while letting the window draw, for up to 5 s of real time.
    fn wait_for(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(cx) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn the_wheel_should_send_mouse_events_to_a_program_that_reports_the_mouse(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        // Turns on mouse reporting as Claude Code does (1003 + SGR 1006), then shows the
        // bytes of one wheel notch: two would be 22, but one is enough.
        cx.simulate_input(
            "printf '\\033[?1003h\\033[?1006h'; stty raw -echo; \
             r=$(dd bs=1 count=11 2>/dev/null); stty sane; echo; echo \"GOT:$r\" | cat -v\n",
        );
        wait_for(cx, |cx| {
            pane.read_with(cx, |pane, _| {
                pane.input_mode()
                    .is_some_and(kil_term::InputMode::mouse_reporting)
            })
        });
        let position = point(px(SIDEBAR_WIDTH + 200.), px(300.));
        cx.simulate_event(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Lines(point(0., 1.)),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        wait_for(cx, |cx| {
            pane.read_with(cx, |pane, _| pane.screen_text().contains("GOT:^[[<64;"))
        });
        let screen = pane.read_with(cx, |pane, _| pane.screen_text());
        assert!(screen.contains("GOT:^[[<64;"), "{screen}");
        assert!(!screen.contains("GOT:^[[A") && !screen.contains("GOT:^[OA"));
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
    fn a_cursor_blink_should_redraw_the_cursor_and_not_the_grid(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        let cursor = pane.read_with(cx, |pane, _| pane.cursor().clone());
        let layouts = |cx: &mut VisualTestContext| pane.read_with(cx, |pane, _| pane.layouts);
        // Everything drawn once, so what follows is the only change.
        cx.update(|window, _| window.refresh());
        let before = layouts(cx);
        cursor.update(cx, |cursor, cx| cursor.set_visible(false, cx));
        assert_eq!(layouts(cx), before);
        // A change of the pane's own does lay its grid out again.
        pane.update(cx, |_, cx| cx.notify());
        assert_eq!(layouts(cx), before + 1);
    }

    #[gpui::test]
    fn output_in_a_pane_should_not_redraw_the_sidebar(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        let sidebar = workspace.read_with(cx, |this, _| this.sidebar.clone());
        let renders =
            |cx: &mut VisualTestContext| sidebar.read_with(cx, |sidebar, _| sidebar.renders);
        cx.update(|window, _| window.refresh());
        let before = renders(cx);
        // What new output does to a pane.
        pane.update(cx, |_, cx| cx.notify());
        assert_eq!(renders(cx), before);
        // The workspace's own changes are the sidebar's.
        workspace.update(cx, |_, cx| cx.notify());
        assert_eq!(renders(cx), before + 1);
    }

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kil-ws-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn shell_in(cwd: &str, command: Option<&str>) -> crate::pane_tree::Shape<SavedPane> {
        crate::pane_tree::Shape::Leaf(SavedPane {
            id: String::new(),
            cwd: Some(cwd.to_owned()),
            command: command.map(str::to_owned),
        })
    }

    fn saved_layout(dir: &std::path::Path) -> SavedWorkspace {
        Store::at(dir.to_owned()).load().unwrap().unwrap().saved
    }

    #[gpui::test]
    fn launch_should_reopen_the_saved_tabs_splits_and_names(cx: &mut TestAppContext) {
        use crate::pane_tree::Shape;
        let saved = SavedWorkspace {
            active: 1,
            window: None,
            tabs: vec![
                SavedTab {
                    name: None,
                    focused: 0,
                    root: shell_in("/", None),
                },
                SavedTab {
                    name: Some("构建".to_owned()),
                    focused: 2,
                    root: Shape::Split {
                        axis: Axis::Horizontal,
                        ratio: 0.3,
                        first: Box::new(shell_in("/tmp", Some("true"))),
                        second: Box::new(Shape::Split {
                            axis: Axis::Vertical,
                            ratio: 0.5,
                            first: Box::new(shell_in("/", None)),
                            second: Box::new(shell_in("/usr", None)),
                        }),
                    },
                },
            ],
        };
        let store = Store::at(scratch_dir("reopen"));
        store.save(store.ticket(), &saved, &[]).unwrap();
        let (workspace, cx) = workspace_saving_to(Some(store), cx);
        workspace.read_with(cx, |this, _| {
            let tab = &this.tabs[1];
            let leaves = tab.tree.leaves();
            assert_eq!((this.tabs.len(), this.active), (2, 1));
            assert_eq!(tab.name, Some("构建".into()));
            assert_eq!(leaves.len(), 3);
            assert_eq!(tab.focused, *leaves[2]);
            assert!(!tab.tree.is_animating(Instant::now()));
        });
    }

    #[gpui::test]
    fn quit_should_save_the_layout_for_the_next_launch(cx: &mut TestAppContext) {
        let dir = scratch_dir("quit");
        let (workspace, cx) = workspace_saving_to(Some(Store::at(dir.clone())), cx);
        cx.dispatch_action(SplitRight);
        workspace.update(cx, |this, _| this.tabs[0].name = Some("dev".into()));
        cx.dispatch_action(Quit);
        cx.run_until_parked();
        let saved = saved_layout(&dir);
        assert_eq!(saved.tabs.len(), 1);
        assert_eq!(saved.tabs[0].name.as_deref(), Some("dev"));
        assert_eq!(saved.tabs[0].focused, 1);
        assert!(matches!(
            saved.tabs[0].root,
            crate::pane_tree::Shape::Split {
                axis: Axis::Horizontal,
                ..
            }
        ));
    }

    #[gpui::test]
    fn closing_the_window_should_save_the_layout(cx: &mut TestAppContext) {
        let dir = scratch_dir("window");
        let (workspace, cx) = workspace_saving_to(Some(Store::at(dir.clone())), cx);
        cx.dispatch_action(NewTab);
        assert!(cx.simulate_close());
        assert!(workspace.read_with(cx, |this, _| this.ended));
        let saved = saved_layout(&dir);
        assert_eq!((saved.tabs.len(), saved.active), (2, 1));
    }

    #[gpui::test]
    fn changes_should_be_saved_while_running_for_a_crash(cx: &mut TestAppContext) {
        let dir = scratch_dir("running");
        let (_workspace, cx) = workspace_saving_to(Some(Store::at(dir.clone())), cx);
        cx.dispatch_action(SplitDown);
        cx.executor().advance_clock(SAVE_INTERVAL);
        cx.run_until_parked();
        assert!(matches!(
            saved_layout(&dir).tabs[0].root,
            crate::pane_tree::Shape::Split {
                axis: Axis::Vertical,
                ..
            }
        ));
    }

    #[gpui::test]
    fn redrawing_should_not_schedule_a_save(cx: &mut TestAppContext) {
        let dir = scratch_dir("redraw");
        let (workspace, cx) = workspace_saving_to(Some(Store::at(dir)), cx);
        workspace.update(cx, |this, _| this.save_task = None);
        // What a cursor blink, or output in a pane, does to the window.
        cx.update(|window, _| window.refresh());
        assert!(workspace.read_with(cx, |this, _| this.save_task.is_none()));
    }

    #[gpui::test]
    fn output_should_save_screens_less_often_than_layout_changes(cx: &mut TestAppContext) {
        let dir = scratch_dir("screens");
        let (workspace, cx) = workspace_saving_to(Some(Store::at(dir)), cx);
        let due = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |this, _| this.save_task.as_ref().map(|(at, _)| *at))
        };
        let now = cx.executor().now();
        workspace.update(cx, |this, cx| {
            this.save_task = None;
            this.screens_saved = Some(now);
            this.schedule_screens(cx);
        });
        assert_eq!(due(cx), Some(now + SCREEN_INTERVAL));
        // A layout change still reaches the disk within SAVE_INTERVAL.
        cx.dispatch_action(SplitDown);
        assert_eq!(due(cx), Some(now + SAVE_INTERVAL));
    }

    #[gpui::test]
    fn closing_the_last_tab_should_leave_nothing_to_restore(cx: &mut TestAppContext) {
        let dir = scratch_dir("closed");
        let (workspace, cx) = workspace_saving_to(Some(Store::at(dir.clone())), cx);
        cx.dispatch_action(SplitRight);
        cx.executor().advance_clock(SAVE_INTERVAL);
        cx.run_until_parked();
        cx.dispatch_action(CloseTab);
        // What the app-quit hook that follows does.
        workspace.update(cx, Workspace::save_now);
        assert_eq!(Store::at(dir).load(), Ok(None));
    }

    fn first_pane(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> (String, i32) {
        workspace.read_with(cx, |this, cx| {
            let pane = this.tabs[0].focused.read(cx);
            (pane.saved().id, pane.pid().map_or(0, kil_term::Pid::get))
        })
    }

    /// Waits for the first pane's session to be up, in the server.
    fn started(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> (String, i32) {
        for _ in 0..200 {
            cx.run_until_parked();
            let pane = first_pane(workspace, cx);
            if pane.1 != 0 {
                return pane;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the pane's session never started");
    }

    #[gpui::test]
    fn quit_should_leave_the_sessions_running_in_the_server(cx: &mut TestAppContext) {
        let client = test_client();
        let (workspace, cx) = workspace_on(Arc::clone(&client), None, cx);
        let (id, pid) = started(&workspace, cx);
        cx.dispatch_action(Quit);
        cx.run_until_parked();
        assert_eq!(client.sessions().unwrap(), [id]);
        assert!(kil_term::diagnostics::exists(
            kil_term::Pid::new(pid).unwrap()
        ));
        client.stop_all().unwrap();
    }

    #[gpui::test]
    fn reopening_should_go_back_on_the_sessions_the_server_kept(cx: &mut TestAppContext) {
        let client = test_client();
        let dir = scratch_dir("reattach");
        let (first, cx) = workspace_on(Arc::clone(&client), Some(Store::at(dir.clone())), cx);
        let before = started(&first, cx);
        cx.dispatch_action(Quit);
        cx.run_until_parked();
        let mut app = cx.cx.clone();
        let (second, cx) = workspace_on(Arc::clone(&client), Some(Store::at(dir)), &mut app);
        assert_eq!(started(&second, cx), before);
        client.stop_all().unwrap();
    }

    #[gpui::test]
    fn launch_should_open_a_tab_for_a_session_no_saved_tab_claims(cx: &mut TestAppContext) {
        let client = test_client();
        let config = kil_term::SessionConfig::new(kil_term::GridSize {
            rows: 24,
            cols: 80,
            cell_width: 8,
            cell_height: 16,
        });
        drop(client.spawn("orphan-1", &config, |_| {}).unwrap());
        let (workspace, cx) = workspace_on(Arc::clone(&client), None, cx);
        assert_eq!(started(&workspace, cx).0, "orphan-1");
        assert_eq!(tab_count(&workspace, cx), 1);
        client.stop_all().unwrap();
    }

    #[gpui::test]
    fn escape_should_close_the_row_menu(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        click(cx, ROW, MouseButton::Right, 1);
        cx.simulate_keystrokes("escape");
        assert!(workspace.read_with(cx, |this, _| this.menu.is_none()));
    }
}
