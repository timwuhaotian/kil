//! Window root: vertical tab sidebar, the active tab's split tree, and every window-level
//! action (DESIGN.md §4.6, §4.7, §7, §8).

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    io,
    ops::Range,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{StreamExt, channel::mpsc};
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, AnyWindowHandle, App, ClickEvent, Context,
    CursorStyle, Div, DragMoveEvent, ElementId, Entity, EntityId, FocusHandle, Focusable,
    FontWeight, Hsla, KeyContext, KeyDownEvent, KeybindingKeystroke, MouseButton, MouseDownEvent,
    Pixels, Point, PromptLevel, ScrollHandle, SharedString, Stateful, StyleRefinement,
    Subscription, Task, WeakEntity, Window, actions, anchored, deferred, div, prelude::*, px,
    relative,
};
use kil_term::{Client, MemoryProbe, TabDesc};

use crate::{
    config, keymap,
    motion::{self, Ease, Motion, Tween},
    notify,
    pane_tree::{Axis, Close, Direction, Node, PaneTree, Split, SplitId},
    restore::{
        self, PaneIds, Restored, SavedGroup, SavedPane, SavedTab, SavedWindow, SavedWorkspace,
        Store,
    },
    settings::Settings,
    terminal_pane::{BLINK_INTERVAL, Launch, PaneEvent, TerminalPane},
    text_field::{FieldEvent, TextField},
    theme::{Theme, on_accent},
};

actions!(
    workspace,
    [
        NewTab,
        CloseTab,
        PinTab,
        UnpinTab,
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
        Restart,
    ]
);

/// The sidebar's width until its edge is dragged (A31), and what a double-click on it restores.
const SIDEBAR_WIDTH: f32 = 220.;
/// How far the sidebar's edge drags.
const SIDEBAR_MIN: f32 = 160.;
const SIDEBAR_MAX: f32 = 400.;
const SIDEBAR_TOP: f32 = 52.;
const ROW_HEIGHT: f32 = 44.;
/// A group's header: one line, lighter than the tab rows under it (A30).
const GROUP_HEIGHT: f32 = 28.;
/// How far a group's rows sit in from the others; its rail runs down the middle of the gap.
const GROUP_INDENT: f32 = 12.;
/// What a new group, or one whose name was emptied, is called.
const GROUP_NAME: &str = "Group";
/// The New Tab button: one line, so it is lighter than the two-line rows above it.
const NEW_TAB_HEIGHT: f32 = 36.;
/// The rows' inset from the sidebar's edges.
const ROWS_INSET: f32 = 8.;
/// The split buttons at the top right of the tab.
const SPLIT_BUTTONS_HEIGHT: f32 = 24.;
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
/// How often the server is asked which sessions a phone watches, for the presence marks.
const WATCHERS_INTERVAL: Duration = Duration::from_secs(2);
/// A tab's figure is shown from this much on; below it, the row stays quiet.
const SHOWN_MEMORY: u64 = 100 * 1024 * 1024;
/// From this much on, the figure is shown as a warning.
const HEAVY_MEMORY: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TabId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GroupId(u64);

/// A named run of unpinned tabs under a header that folds them away (A30). Its tabs are
/// the ones whose `group` names it; they always sit next to each other in `tabs`, and the
/// group is dropped when the last of them leaves.
struct Group {
    id: GroupId,
    name: SharedString,
    collapsed: bool,
    /// Offset of the header from its place, in pixels, as [`Tab::shift`].
    shift: Tween,
}

/// One thing in the tab list: a tab's row, or a group's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Item {
    Tab(TabId),
    Group(GroupId),
}

/// Where an item rests in the tab list, from the list's top, ignoring its shift.
#[derive(Clone, Copy, Debug)]
struct Placed {
    item: Item,
    top: f32,
    height: f32,
}

impl Placed {
    fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

struct Tab {
    id: TabId,
    tree: PaneTree<Entity<TerminalPane>>,
    focused: Entity<TerminalPane>,
    /// Set by the user (Rename Tab); otherwise the row follows the focused pane.
    name: Option<SharedString>,
    /// Pinned tabs form a stable group above the others.
    pinned: bool,
    /// The group the tab is in; never set on a pinned tab.
    group: Option<GroupId>,
    /// A job ended here while the tab was not in view; cleared when it is.
    finished: bool,
    animating: bool,
    /// What this tab's panes cost the machine; read every [`MEMORY_INTERVAL`].
    memory: u64,
    /// Offset of the row from its slot, in pixels; eases to 0 as it makes room for a
    /// dragged row, or settles into place after a drop.
    shift: Tween,
    /// How much of the row shows: 0 while its group is folded away, 1 otherwise.
    reveal: Tween,
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

/// What a sidebar row's status mark shows, most urgent first (and least in `Ord`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    fn new(id: TabId, tree: PaneTree<Entity<TerminalPane>>, focused: Entity<TerminalPane>) -> Self {
        Self {
            id,
            tree,
            focused,
            name: None,
            pinned: false,
            group: None,
            finished: false,
            animating: false,
            memory: 0,
            shift: Tween::settled(0.),
            reveal: Tween::settled(1.),
        }
    }

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

/// The right-click menu of a sidebar row or group header, at the click.
#[derive(Clone, Copy)]
struct TabMenu {
    item: Item,
    position: Point<Pixels>,
}

/// A row's title, or a group's name, being edited in place.
struct Rename {
    item: Item,
    field: Entity<TextField>,
    _subscription: Subscription,
}

#[derive(Clone, Copy)]
struct DividerDrag {
    tab: TabId,
    split: SplitId,
}

/// A sidebar row being dragged to another place; the row itself moves under the pointer.
/// Which one is [`Workspace::dragging`]'s.
#[derive(Clone, Copy)]
struct TabDrag;

/// A group's header being dragged, taking the whole group with it.
#[derive(Clone, Copy)]
struct GroupDrag;

/// The sidebar's edge being dragged to resize it.
#[derive(Clone, Copy)]
struct SidebarDrag;

/// The row or header being dragged: where in it the pointer took it, and where its slot is
/// now, in the window.
#[derive(Clone, Copy)]
struct RowDrag {
    item: Item,
    grab: Point<Pixels>,
    slot_top: Pixels,
    /// The folded group whose header a dragged tab is over: let go, it joins that group.
    into: Option<GroupId>,
}

/// The dragged row, lifted off the list: it follows the pointer up and down while the slot
/// it will drop into stays open below.
#[derive(Clone)]
struct RowPreview {
    title: SharedString,
    cwd: SharedString,
    status: TabStatus,
    pinned: bool,
    chrome: Chrome,
    /// Where across the row it was taken, to keep it in the sidebar's column.
    grab_x: Pixels,
    /// The row's width, the sidebar's less its insets.
    width: f32,
}

impl Render for RowPreview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let chrome = self.chrome;
        // Drawn at the pointer, less where the row was taken; held to the column, so the
        // row moves only up and down, as the list does.
        let left = px(ROWS_INSET) - (window.mouse_position().x - self.grab_x);
        div().child(
            div()
                .absolute()
                .left(left)
                .top_0()
                .w(px(self.width))
                .h(px(ROW_HEIGHT))
                .rounded_lg()
                // The sidebar is translucent; a lifted row is not.
                .bg(Hsla {
                    a: 1.,
                    ..chrome.sidebar_bg
                })
                .shadow_lg()
                .child(
                    div()
                        .size_full()
                        .px_3()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .rounded_lg()
                        .border_1()
                        .border_color(chrome.divider)
                        .bg(chrome.selected)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(TITLE_INSET - STATUS_SLOT))
                                .child(status_mark(self.status, chrome))
                                .child(row_title(self.title.clone(), None, true, chrome))
                                .when(self.pinned, |row| row.child(pin_mark(chrome))),
                        )
                        .child(row_cwd(self.cwd.clone(), chrome)),
                ),
        )
    }
}

/// A dragged group's header, lifted off the list folded: the group moves as one line.
#[derive(Clone)]
struct GroupPreview {
    name: SharedString,
    count: usize,
    status: TabStatus,
    chrome: Chrome,
    grab_x: Pixels,
    width: f32,
}

impl Render for GroupPreview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let chrome = self.chrome;
        let left = px(ROWS_INSET) - (window.mouse_position().x - self.grab_x);
        div().child(
            div()
                .absolute()
                .left(left)
                .top_0()
                .w(px(self.width))
                .h(px(GROUP_HEIGHT))
                .rounded_lg()
                .bg(Hsla {
                    a: 1.,
                    ..chrome.sidebar_bg
                })
                .shadow_lg()
                .child(
                    group_line(true, chrome)
                        .size_full()
                        .rounded_lg()
                        .border_1()
                        .border_color(chrome.divider)
                        .bg(chrome.selected)
                        .child(group_name(self.name.clone(), None, chrome))
                        .child(group_summary(self.count, self.status, chrome)),
                ),
        )
    }
}

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
    /// The tab groups, in no particular order: their tabs' places order them.
    groups: Vec<Group>,
    next_group: u64,
    /// The sidebar's width, as its edge was last dragged (A31).
    sidebar_width: f32,
    /// Offset of the selection pill from the active row, in pixels; eases to 0 so the pill
    /// slides from the previous row.
    pill: Tween,
    ghosts: Vec<Ghost>,
    menu: Option<TabMenu>,
    /// Held by the open menu so Escape reaches it instead of the terminal.
    menu_focus: FocusHandle,
    rename: Option<Rename>,
    dragging: Option<RowDrag>,
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
    /// Scroll state of the tab list, so the active row can be brought into view.
    tabs_scroll: ScrollHandle,
    /// The active row the list last scrolled to; `Cell` because the scroll is requested
    /// while rendering the sidebar.
    scrolled_active: Cell<usize>,
    /// The window's one cursor-blink timer; see [`Self::sync_blink`].
    blink: Option<Task<()>>,
    /// The pane it last blinked, so a pane that loses focus is left with its cursor drawn.
    blinking: Option<WeakEntity<TerminalPane>>,
    /// The timer that reads what the panes cost; see [`Self::refresh_memory`].
    memory_task: Option<Task<()>>,
    /// The timer that asks which sessions a phone watches; see [`Self::apply_watchers`].
    watchers_task: Option<Task<()>>,
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
        let tabs_scroll = ScrollHandle::new();
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active: 0,
            next_tab: 0,
            groups: Vec::new(),
            next_group: 0,
            sidebar_width: SIDEBAR_WIDTH,
            pill: Tween::settled(0.),
            ghosts: Vec::new(),
            menu: None,
            menu_focus: cx.focus_handle(),
            rename: None,
            dragging: None,
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
            tabs_scroll,
            scrolled_active: Cell::new(0),
            blink: None,
            blinking: None,
            memory_task: None,
            watchers_task: None,
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
        this.start_watchers_watch(cx);
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
        self.sidebar_width = saved
            .sidebar_width
            .filter(|width| width.is_finite())
            .map_or(SIDEBAR_WIDTH, |width| width.clamp(SIDEBAR_MIN, SIDEBAR_MAX));
        let groups = self.restore_groups(&saved.groups);
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
                name: tab
                    .name
                    .filter(|name| !name.trim().is_empty())
                    .map(Into::into),
                pinned: tab.pinned,
                group: tab
                    .group
                    .and_then(|group| groups.get(group).copied())
                    .filter(|_| !tab.pinned),
                ..Tab::new(id, tree, focused)
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
        self.active = saved.active.min(last);
        self.gather();
        // Folded groups reopen folded, without folding as the window opens.
        for index in 0..self.tabs.len() {
            let shown = if self.is_shown(index) { 1. } else { 0. };
            self.tabs[index].reveal.set(shown);
        }
        // Set first, so the selection pill starts on the row instead of sliding to it.
        self.activate(self.active, window, cx);
        true
    }

    /// The saved groups, made again; a tab names its group by its index here.
    fn restore_groups(&mut self, saved: &[SavedGroup]) -> Vec<GroupId> {
        saved
            .iter()
            .map(|group| {
                let id = GroupId(self.next_group);
                self.next_group += 1;
                let name = group.name.trim();
                self.groups.push(Group {
                    id,
                    name: if name.is_empty() { GROUP_NAME } else { name }
                        .to_owned()
                        .into(),
                    collapsed: group.collapsed,
                    shift: Tween::settled(0.),
                });
                id
            })
            .collect()
    }

    /// Puts reopened tabs in a shape the sidebar can show: a hand-edited old save may mix
    /// the sections or split a group. Pinned rows go to the top, and a group's tabs gather
    /// where its first one is. The active tab stays active.
    fn gather(&mut self) {
        let active = self.tabs.get(self.active).map(|tab| tab.id);
        let mut first = HashMap::new();
        let keys: HashMap<TabId, (bool, usize)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let at = tab
                    .group
                    .map_or(index, |group| *first.entry(group).or_insert(index));
                (tab.id, (!tab.pinned, at))
            })
            .collect();
        self.tabs.sort_by_key(|tab| keys[&tab.id]);
        self.prune_groups();
        self.active = self
            .tabs
            .iter()
            .position(|tab| Some(tab.id) == active)
            .unwrap_or(0);
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
        // In the order the tabs place them.
        let mut saved_groups: Vec<GroupId> = Vec::new();
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
            let group = tab.group.and_then(|group| {
                let index = saved_groups.iter().position(|&id| id == group);
                index.or_else(|| {
                    saved_groups.push(group);
                    Some(saved_groups.len() - 1)
                })
            });
            tabs.push(SavedTab {
                name: tab.name.as_ref().map(ToString::to_string),
                pinned: tab.pinned,
                group,
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
            sidebar_width: (self.sidebar_width != SIDEBAR_WIDTH).then_some(self.sidebar_width),
            window: Some(self.frame),
            groups: saved_groups
                .iter()
                .filter_map(|&id| self.group(id))
                .map(|group| SavedGroup {
                    name: group.name.to_string(),
                    collapsed: group.collapsed,
                })
                .collect(),
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
        self.tabs
            .push(Tab::new(id, PaneTree::new(pane.clone()), pane));
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        if index != self.active {
            let now = Instant::now();
            let tops = self.tops(now);
            let top = |tab: Option<&Tab>| tab.and_then(|tab| tops.get(&Item::Tab(tab.id)).copied());
            if let (Some(from), Some(to)) =
                (top(self.tabs.get(self.active)), top(self.tabs.get(index)))
            {
                self.pill.set(self.pill.value(now) + from - to);
                self.pill.retarget(0., motion::PILL, Ease::OutQuint, now);
            }
        }
        self.active = index;
        let tab = &mut self.tabs[index];
        tab.finished = false;
        window.focus(&tab.focused.focus_handle(cx));
        cx.notify();
    }

    fn toggle_pin(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(pinned) = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| !tab.pinned)
        else {
            return;
        };
        self.set_pinned(id, pinned, cx);
    }

    /// Pinning leaves the tab's group: pinned tabs are in none.
    fn set_pinned(&mut self, id: TabId, pinned: bool, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        if self.tabs[from].pinned == pinned {
            return;
        }
        self.rearrange(cx, |this| {
            this.tabs[from].pinned = pinned;
            let pinned_count = this.tabs.iter().filter(|tab| tab.pinned).count();
            // Pin after the existing pinned rows; unpin before the ordinary rows.
            let to = if pinned {
                pinned_count - 1
            } else {
                pinned_count
            };
            this.place_tab(from, to, None);
        });
    }

    /// Moves tab `from` to `to` (an index once it is out of the list) and into `group`.
    fn place_tab(&mut self, from: usize, to: usize, group: Option<GroupId>) {
        let mut tab = self.tabs.remove(from);
        tab.group = group;
        self.tabs.insert(to.min(self.tabs.len()), tab);
    }

    /// Changes the tabs' order or groups, keeping the active tab active. Every row and
    /// header that moved slides over from where it was (`ROW_SHIFT`) instead of jumping;
    /// one being dragged stays under the pointer.
    fn rearrange(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Self)) {
        let now = Instant::now();
        let before = self.tops(now);
        let active = self.tabs.get(self.active).map(|tab| tab.id);
        change(self);
        self.prune_groups();
        if let Some(active) = active {
            self.active = self
                .tabs
                .iter()
                .position(|tab| tab.id == active)
                .unwrap_or(0);
        }
        let lifted = self.dragging.map(|drag| drag.item);
        for placed in self.layout(now) {
            let Some(&was) = before.get(&placed.item) else {
                continue;
            };
            if Some(placed.item) == lifted || (was - placed.top).abs() < 0.5 {
                continue;
            }
            if let Some(shift) = self.shift_mut(placed.item) {
                shift.set(shift.value(now) + was - placed.top);
                shift.retarget(0., motion::ROW_SHIFT, Ease::OutQuint, now);
            }
        }
        cx.notify();
    }

    fn shift_mut(&mut self, item: Item) -> Option<&mut Tween> {
        match item {
            Item::Tab(id) => self
                .tabs
                .iter_mut()
                .find(|tab| tab.id == id)
                .map(|tab| &mut tab.shift),
            Item::Group(id) => self.group_mut(id).map(|group| &mut group.shift),
        }
    }

    // ── groups ────────────────────────────────────────────────────────────────────────

    fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.iter().find(|group| group.id == id)
    }

    fn group_mut(&mut self, id: GroupId) -> Option<&mut Group> {
        self.groups.iter_mut().find(|group| group.id == id)
    }

    /// Where a group's tabs are in `tabs`: they always sit together.
    fn group_range(&self, id: GroupId) -> Option<Range<usize>> {
        let start = self.tabs.iter().position(|tab| tab.group == Some(id))?;
        let len = self.tabs[start..]
            .iter()
            .take_while(|tab| tab.group == Some(id))
            .count();
        Some(start..start + len)
    }

    /// The group whose header is being dragged.
    fn dragged_group(&self) -> Option<GroupId> {
        match self.dragging?.item {
            Item::Group(id) => Some(id),
            Item::Tab(_) => None,
        }
    }

    /// Whether tab `index`'s row would show if it were not the active one: a folded group
    /// keeps its tabs out of sight, and so does a group being dragged.
    fn shown_unless_active(&self, index: usize) -> bool {
        match self.tabs[index].group {
            None => true,
            Some(id) => {
                self.dragged_group() != Some(id)
                    && self.group(id).is_none_or(|group| !group.collapsed)
            }
        }
    }

    /// Whether tab `index`'s row shows. The active tab stays under its folded group's
    /// header, and a dragged tab keeps its slot.
    fn is_shown(&self, index: usize) -> bool {
        let tab = &self.tabs[index];
        self.shown_unless_active(index)
            || self
                .dragging
                .is_some_and(|drag| drag.item == Item::Tab(tab.id))
            || (index == self.active && tab.group.is_none_or(|id| self.dragged_group() != Some(id)))
    }

    /// The tabs whose rows show, in order: what `cmd-1`…`cmd-9` and Next and Previous Tab
    /// go through, so a folded group stays out of the way.
    fn shown_tabs(&self) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&index| self.is_shown(index))
            .collect()
    }

    /// Eases each row toward showing or folded away, as its group and the selection say.
    fn sync_reveal(&mut self, now: Instant) {
        for index in 0..self.tabs.len() {
            let to = if self.is_shown(index) { 1. } else { 0. };
            let reveal = &mut self.tabs[index].reveal;
            if (reveal.target() - to).abs() > f32::EPSILON {
                let (duration, ease) = if to > 0. {
                    (motion::ROW_IN, Ease::OutQuint)
                } else {
                    (motion::ROW_OUT, Ease::InOut)
                };
                reveal.retarget(to, duration, ease, now);
            }
        }
    }

    /// Drops the groups left without tabs, and the menu or rename that was on one.
    fn prune_groups(&mut self) {
        let tabs = &self.tabs;
        self.groups
            .retain(|group| tabs.iter().any(|tab| tab.group == Some(group.id)));
        let gone = |item: Item, groups: &[Group]| match item {
            Item::Tab(_) => false,
            Item::Group(id) => groups.iter().all(|group| group.id != id),
        };
        if self.menu.is_some_and(|menu| gone(menu.item, &self.groups)) {
            self.menu = None;
        }
        if self
            .rename
            .as_ref()
            .is_some_and(|rename| gone(rename.item, &self.groups))
        {
            self.rename = None;
        }
    }

    /// New Group from Tab: the tab goes into a group of its own, whose name is then edited.
    /// A tab taken from another group goes after it, so that group stays in one piece.
    fn new_group(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        if self.tabs[from].pinned {
            return;
        }
        let group = GroupId(self.next_group);
        self.next_group += 1;
        self.groups.push(Group {
            id: group,
            name: GROUP_NAME.into(),
            collapsed: false,
            shift: Tween::settled(0.),
        });
        let to = self.tabs[from]
            .group
            .and_then(|old| self.group_range(old))
            .map_or(from, |range| range.end - 1);
        self.rearrange(cx, |this| this.place_tab(from, to, Some(group)));
        self.start_rename(Item::Group(group), window, cx);
    }

    /// Remove from Group: the tab goes just after its group.
    fn leave_group(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let Some(range) = self.tabs[from]
            .group
            .and_then(|group| self.group_range(group))
        else {
            return;
        };
        self.rearrange(cx, |this| this.place_tab(from, range.end - 1, None));
    }

    /// Ungroup: the group goes, and its tabs stay where they are.
    fn ungroup(&mut self, id: GroupId, cx: &mut Context<Self>) {
        self.rearrange(cx, |this| {
            for tab in &mut this.tabs {
                if tab.group == Some(id) {
                    tab.group = None;
                }
            }
        });
    }

    fn toggle_group(&mut self, id: GroupId, cx: &mut Context<Self>) {
        if let Some(group) = self.group_mut(id) {
            group.collapsed = !group.collapsed;
            cx.notify();
        }
    }

    // ── the list's layout, and dragging in it ─────────────────────────────────────────

    /// Where each row and header is in the tab list, from its top, without shifts: a row
    /// folding in or out counts for what shows of it now. Mirrors
    /// [`Self::render_sidebar`], less the rows of closed tabs animating out.
    fn layout(&self, now: Instant) -> Vec<Placed> {
        self.layout_by(|_, tab| ROW_HEIGHT * tab.reveal.value(now).clamp(0., 1.))
    }

    /// The layout once the rows have folded in or out as they are about to: where a drag
    /// lands is worked out on this, so it doesn't depend on how far an animation got.
    fn resting_layout(&self) -> Vec<Placed> {
        self.layout_by(
            |index, _| {
                if self.is_shown(index) { ROW_HEIGHT } else { 0. }
            },
        )
    }

    fn layout_by(&self, row_height: impl Fn(usize, &Tab) -> f32) -> Vec<Placed> {
        let mut placed = Vec::with_capacity(self.tabs.len() + self.groups.len());
        let mut top = 0.;
        let mut run = None;
        for (index, tab) in self.tabs.iter().enumerate() {
            if tab.group != run {
                run = tab.group;
                if let Some(group) = run {
                    placed.push(Placed {
                        item: Item::Group(group),
                        top,
                        height: GROUP_HEIGHT,
                    });
                    top += GROUP_HEIGHT;
                }
            }
            let height = row_height(index, tab);
            placed.push(Placed {
                item: Item::Tab(tab.id),
                top,
                height,
            });
            top += height;
        }
        placed
    }

    fn tops(&self, now: Instant) -> HashMap<Item, f32> {
        self.layout(now)
            .into_iter()
            .map(|placed| (placed.item, placed.top))
            .collect()
    }

    /// The group an item belongs to: a header's own, or a tab's.
    fn group_of(&self, item: Item) -> Option<GroupId> {
        match item {
            Item::Group(id) => Some(id),
            Item::Tab(id) => self.tabs.iter().find(|tab| tab.id == id)?.group,
        }
    }

    /// The pointer dragging a row or header is at `y` in the tab list, whose top is at
    /// `list_top` in the window.
    fn drag_over(&mut self, y: f32, list_top: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.dragging else {
            return;
        };
        let layout = self.resting_layout();
        let Some(at) = layout.iter().position(|placed| placed.item == drag.item) else {
            return;
        };
        let over = layout
            .iter()
            .position(|placed| placed.height > 0. && placed.top <= y && y < placed.bottom());
        let into = match drag.item {
            Item::Tab(id) => self.drag_tab_over(id, &layout, at, over, y, cx),
            Item::Group(id) => {
                self.drag_group_over(id, &layout, at, over, y, cx);
                None
            }
        };
        let slot = self
            .resting_layout()
            .into_iter()
            .find(|placed| placed.item == drag.item)
            .map_or(0., |placed| placed.top);
        if let Some(drag) = &mut self.dragging {
            drag.slot_top = list_top + px(slot);
            if drag.into != into {
                drag.into = into;
                cx.notify();
            }
        }
    }

    /// A dragged tab takes the place of the row it passes, and that row's group with it
    /// (A30). Past an open group's header it goes in at the top, or back out above it;
    /// below the last row it goes to the end, in no group. Over a folded group's header it
    /// stays put, and that group is returned: let go there, it goes in at the end.
    fn drag_tab_over(
        &mut self,
        id: TabId,
        layout: &[Placed],
        at: usize,
        over: Option<usize>,
        y: f32,
        cx: &mut Context<Self>,
    ) -> Option<GroupId> {
        let from = self.tabs.iter().position(|tab| tab.id == id)?;
        let pinned = self.tabs[from].pinned;
        let Some(over) = over else {
            let end = layout.last().map_or(0., Placed::bottom);
            let last = self.tabs.len() - 1;
            if !pinned && y >= end && (from != last || self.tabs[from].group.is_some()) {
                self.rearrange(cx, |this| this.place_tab(from, last, None));
            }
            return None;
        };
        if over == at {
            return None;
        }
        let target = layout[over];
        // Only once the row would land under the pointer, so it doesn't trade places back
        // with one taller or shorter than itself.
        let down = over > at;
        let height = layout[at].height;
        let lands = if down {
            y >= target.bottom() - height
        } else {
            y < target.top + height
        };
        match target.item {
            Item::Tab(other) => {
                let to = self.tabs.iter().position(|tab| tab.id == other)?;
                if self.tabs[to].pinned == pinned && lands {
                    let group = self.tabs[to].group;
                    self.rearrange(cx, |this| this.place_tab(from, to, group));
                }
                None
            }
            Item::Group(group) => {
                if pinned {
                    return None;
                }
                if self.group(group).is_some_and(|group| group.collapsed) {
                    return Some(group);
                }
                if lands {
                    self.rearrange(cx, |this| {
                        let tab = this.tabs.remove(from);
                        let first = this
                            .tabs
                            .iter()
                            .position(|tab| tab.group == Some(group))
                            .unwrap_or(from);
                        this.tabs.insert(first, tab);
                        this.tabs[first].group = down.then_some(group);
                    });
                }
                None
            }
        }
    }

    /// A dragged group moves as one: past a tab out of any group, or past a whole other
    /// group, never into one, and never among the pinned tabs.
    fn drag_group_over(
        &mut self,
        id: GroupId,
        layout: &[Placed],
        at: usize,
        over: Option<usize>,
        y: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(range) = self.group_range(id) else {
            return;
        };
        let Some(over) = over else {
            let end = layout.last().map_or(0., Placed::bottom);
            if y >= end && range.end != self.tabs.len() {
                self.rearrange(cx, |this| {
                    let moved: Vec<Tab> = this.tabs.drain(range).collect();
                    this.tabs.extend(moved);
                });
            }
            return;
        };
        let target = self.group_of(layout[over].item);
        if target == Some(id) {
            return;
        }
        // The block the pointer is over, in the layout and in `tabs`.
        let block = if let Some(other) = target {
            let first = layout
                .iter()
                .position(|placed| placed.item == Item::Group(other));
            let last = layout
                .iter()
                .rposition(|placed| self.group_of(placed.item) == Some(other));
            first.zip(last).zip(self.group_range(other))
        } else {
            let index = match layout[over].item {
                Item::Tab(other) => self.tabs.iter().position(|tab| tab.id == other),
                Item::Group(_) => None,
            };
            index
                .filter(|&index| !self.tabs[index].pinned)
                .map(|index| ((over, over), index..index + 1))
        };
        let Some(((first, last), tabs)) = block else {
            return;
        };
        let down = first > at;
        let height = layout[at].height;
        let lands = if down {
            y >= layout[last].bottom() - height
        } else {
            y < layout[first].top + height
        };
        if !lands {
            return;
        }
        self.rearrange(cx, |this| {
            let moved: Vec<Tab> = this.tabs.drain(range.clone()).collect();
            let to = if down {
                tabs.end - moved.len()
            } else {
                tabs.start
            };
            this.tabs.splice(to..to, moved);
        });
    }

    /// The dragged row or header was let go: it settles from under the pointer into its
    /// slot. A tab let go on a folded group's header goes in at the group's end instead.
    fn drop_row(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(drag) = self.dragging.take() else {
            return;
        };
        if let (Item::Tab(id), Some(group)) = (drag.item, drag.into) {
            let from = self.tabs.iter().position(|tab| tab.id == id);
            if let (Some(from), Some(range)) = (from, self.group_range(group)) {
                let to = if from < range.end {
                    range.end - 1
                } else {
                    range.end
                };
                self.rearrange(cx, |this| this.place_tab(from, to, Some(group)));
            }
            cx.notify();
            return;
        }
        let top = window.mouse_position().y - drag.grab.y;
        if let Some(shift) = self.shift_mut(drag.item) {
            shift.set((top - drag.slot_top) / px(1.));
            shift.retarget(0., motion::ROW_DROP, Ease::OutQuint, Instant::now());
        }
        cx.notify();
    }

    /// `cmd-1`…`cmd-8` pick that shown tab if it exists; `cmd-9` (`usize::MAX`) picks the
    /// last. Tabs folded away in a group are skipped.
    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown_tabs();
        let pick = if index == usize::MAX {
            shown.last()
        } else {
            shown.get(index)
        };
        if let Some(&pick) = pick {
            self.activate(pick, window, cx);
        }
    }

    /// Goes to the pane named `id` (a click on its notification), bringing Kil forward. A pane
    /// closed since stays unfound, and Kil is still brought forward.
    fn go_to_pane(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.activate(true);
        let found = self.tabs.iter().enumerate().find_map(|(index, tab)| {
            let pane = tab
                .tree
                .leaves()
                .into_iter()
                .find(|pane| pane.read(cx).id() == id)?;
            Some((index, pane.clone()))
        });
        if let Some((index, pane)) = found {
            self.tabs[index].focused = pane;
            self.activate(index, window, cx);
        }
    }

    /// Goes to each pane whose notification is clicked, as `clicks` names them.
    pub fn go_to_clicked_notifications(
        mut clicks: mpsc::UnboundedReceiver<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(pane) = clicks.next().await {
                if this
                    .update_in(cx, |this, window, cx| this.go_to_pane(&pane, window, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
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
        let now = Instant::now();
        let was_at = self
            .tabs
            .get(self.active)
            .and_then(|tab| self.tops(now).get(&Item::Tab(tab.id)).copied());
        let tab = self.tabs.remove(index);
        for pane in tab.tree.leaves() {
            pane.update(cx, |pane, _| pane.shutdown());
            self.pane_subscriptions.remove(&pane.entity_id());
        }
        self.menu = self.menu.filter(|menu| menu.item != Item::Tab(tab.id));
        if self
            .rename
            .as_ref()
            .is_some_and(|rename| rename.item == Item::Tab(tab.id))
        {
            self.rename = None;
        }
        self.prune_groups();
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
        if index < self.active {
            // The active row moved up one.
            self.active -= 1;
        } else if index == self.active {
            // The next row takes over, or the previous one after the last tab. A row folded
            // away in a group is passed over for the nearest one that shows.
            let mut next = index.min(self.tabs.len() - 1);
            if !self.shown_unless_active(next) {
                let shown = (next..self.tabs.len())
                    .chain((0..next).rev())
                    .find(|&other| self.shown_unless_active(other));
                next = shown.unwrap_or(next);
            }
            self.active = next;
            // The pill slides from the closed row, which shrinks above a row below it.
            let below = if next >= index { ROW_HEIGHT } else { 0. };
            let now_at = self.tops(now).get(&Item::Tab(self.tabs[next].id)).copied();
            if let (Some(was), Some(now_at)) = (was_at, now_at) {
                self.pill.set(was - now_at - below);
                self.pill.retarget(0., motion::ROW_OUT, Ease::InOut, now);
            }
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
        item: Item,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some(TabMenu { item, position });
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

    fn start_rename(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        let title = match item {
            Item::Tab(id) => self
                .tabs
                .iter()
                .find(|tab| tab.id == id)
                .map(|t| t.title(cx)),
            Item::Group(id) => self.group(id).map(|group| group.name.clone()),
        };
        let Some(title) = title else {
            return;
        };
        let field = cx.new(|cx| TextField::new(&title, window, cx));
        let subscription = cx.subscribe_in(&field, window, Self::end_rename);
        window.focus(&field.focus_handle(cx));
        self.rename = Some(Rename {
            item,
            field,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// An empty name gives the row back to the pane's title, and a group its default name.
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
            let name = (!name.is_empty()).then(|| SharedString::from(name.to_owned()));
            match rename.item {
                Item::Tab(id) => self.set_name(id, name),
                Item::Group(id) => {
                    if let Some(group) = self.group_mut(id) {
                        group.name = name.unwrap_or_else(|| GROUP_NAME.into());
                    }
                }
            }
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
            PaneEvent::Notified(notification) => {
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
                    let info = pane.read(cx).info().clone();
                    let (title, subtitle, body) = notify::describe(
                        &notification.title,
                        &notification.body,
                        info.job.as_deref().map(|job| &**job),
                        &tab.title(cx),
                        &info.cwd,
                    );
                    notify::post(pane.read(cx).id(), &title, &subtitle, &body);
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

    /// Restart: stops every job, quits, and opens Kil again.
    fn restart(&mut self, _: &Restart, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_close(
            &self.all_panes(),
            "Restart Kil and stop all jobs?",
            "will be stopped, then run again when Kil restarts",
            window,
            cx,
            |this, _, cx| {
                // A second confirmed prompt (Restart again from another window) must not
                // arm a second relaunch, and a relaunch that can't be armed must not cost
                // the jobs: only stop and quit once the new Kil is on its way.
                if this.ended {
                    return;
                }
                if !crate::restart_kil() {
                    this.notices.push(
                        "Kil couldn't open itself again, so it stayed open and the jobs kept running."
                            .into(),
                    );
                    cx.notify();
                    return;
                }
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

    /// What every pane costs, as far as the UI thread can tell: the grids are measured here,
    /// and the processes are left to [`MemoryProbe::read`], which asks the kernel about every
    /// process below each shell and belongs on a background thread.
    fn memory_probes(&self, cx: &App) -> Vec<(TabId, Vec<MemoryProbe>)> {
        self.tabs
            .iter()
            .map(|tab| {
                let probes = tab
                    .tree
                    .leaves()
                    .iter()
                    .filter_map(|pane| pane.read(cx).memory_probe())
                    .collect();
                (tab.id, probes)
            })
            .collect()
    }

    /// Takes in what the panes cost (`readings`, per tab) and how the system is doing
    /// (DESIGN.md §5.2).
    ///
    /// A program in a pane can take gigabytes, and the system charges them to the app that
    /// started it: the sidebar's figures are how the user finds the hog before macOS does.
    fn apply_memory(
        &mut self,
        readings: &[(TabId, u64)],
        system: kil_term::system::Memory,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        let mut total = 0;
        for tab in &mut self.tabs {
            // A tab opened since the probes were taken keeps what it had.
            if let Some(&(_, sum)) = readings.iter().find(|(id, _)| *id == tab.id) {
                // The bytes change at every reading of any running program; what the row
                // shows seldom does, and a redraw for nothing is what keeps an idle window
                // awake.
                changed |= shown_memory(sum) != shown_memory(tab.memory);
                tab.memory = sum;
            }
            total += tab.memory;
        }
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

    /// [`Self::memory_probes`] read on this thread, for tests.
    #[cfg(test)]
    fn refresh_memory(&mut self, cx: &mut Context<Self>) {
        let readings: Vec<_> = self
            .memory_probes(cx)
            .into_iter()
            .map(|(id, probes)| (id, probes.into_iter().map(MemoryProbe::read).sum()))
            .collect();
        self.apply_memory(&readings, kil_term::system::memory(), cx);
    }

    /// Reads what the panes cost, every [`MEMORY_INTERVAL`]. The reading walks every pane's
    /// process tree through the kernel, so it runs on a background thread.
    fn start_memory_watch(&mut self, cx: &mut Context<Self>) {
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(MEMORY_INTERVAL).await;
                let Ok(probes) = this.update(cx, |this, cx| this.memory_probes(cx)) else {
                    break;
                };
                let (readings, system) = cx
                    .background_executor()
                    .spawn(async move {
                        let readings: Vec<_> = probes
                            .into_iter()
                            .map(|(id, probes)| {
                                (id, probes.into_iter().map(MemoryProbe::read).sum())
                            })
                            .collect();
                        (readings, kil_term::system::memory())
                    })
                    .await;
                if this
                    .update(cx, |this, cx| this.apply_memory(&readings, system, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Marks the panes a phone is attached to, and the ones it recently typed in; a failed
    /// query leaves the marks standing.
    fn apply_watchers(
        &mut self,
        watched: io::Result<Vec<String>>,
        controlling: io::Result<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        let (Ok(watched), Ok(controlling)) = (watched, controlling) else {
            return;
        };
        let watched: HashSet<&str> = watched.iter().map(String::as_str).collect();
        let controlling: HashSet<&str> = controlling.iter().map(String::as_str).collect();
        let mut changed = false;
        for pane in self.all_panes() {
            changed |= pane.update(cx, |pane, _| {
                pane.set_presence(watched.contains(pane.id()), controlling.contains(pane.id()))
            });
        }
        if changed {
            cx.notify();
        }
    }

    /// This window's tabs as the sidebar shows them, for the server to hand a phone:
    /// sidebar order, the user's name or the focused pane's title, its folder, every
    /// split pane's session, pinned, active, and the row's status. Taken here, on the
    /// UI thread; only the socket round trip leaves it.
    fn tab_descriptions(&self, cx: &App) -> Vec<TabDesc> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let focused = tab.focused.read(cx);
                TabDesc {
                    title: tab.title(cx).to_string(),
                    cwd: focused.info().cwd.to_string(),
                    session: focused.id().to_owned(),
                    panes: tab
                        .tree
                        .leaves()
                        .iter()
                        .map(|pane| pane.read(cx).id().to_owned())
                        .collect(),
                    pinned: tab.pinned,
                    active: index == self.active,
                    status: match tab.status(cx) {
                        TabStatus::Idle => 0,
                        TabStatus::Running => 1,
                        TabStatus::Done => 2,
                        TabStatus::Failed => 3,
                    },
                }
            })
            .collect()
    }

    /// Asks the server which sessions a phone watches, every [`WATCHERS_INTERVAL`],
    /// publishing this window's tabs in the same round trip so a phone's roster mirrors
    /// the sidebar. The first round runs at once, not after an interval.
    fn start_watchers_watch(&mut self, cx: &mut Context<Self>) {
        let client = Arc::clone(&self.client);
        self.watchers_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(tabs) = this.update(cx, |this, cx| this.tab_descriptions(cx)) else {
                    break;
                };
                // A socket round trip: keep it off the UI thread, like panes spawning.
                // A failed publish (an older server hangs up on it) is ignored: presence
                // polling carries on either way.
                let (watched, controlling) = cx
                    .background_executor()
                    .spawn({
                        let client = Arc::clone(&client);
                        async move {
                            let _ = client.publish_tabs(&tabs);
                            (client.watchers(), client.controlling())
                        }
                    })
                    .await;
                if this
                    .update(cx, |this, cx| this.apply_watchers(watched, controlling, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(WATCHERS_INTERVAL).await;
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
            // While animating too, for a pane split off mid-animation.
            if animating || animating != tab.animating {
                tab.animating = animating;
                for pane in tab.tree.leaves() {
                    pane.update(cx, |pane, cx| pane.set_frozen(animating, cx));
                }
            }
        }
    }

    /// The active row's child index in the tab list: past the leading ghosts, and the
    /// headers and rows (with their ghosts) above it. Mirrors [`Self::render_sidebar`].
    fn active_child_index(&self) -> usize {
        let mut index = self
            .ghosts
            .iter()
            .filter(|ghost| ghost.after.is_none())
            .count();
        let mut run = None;
        for (position, tab) in self.tabs.iter().enumerate() {
            if tab.group != run {
                run = tab.group;
                index += usize::from(run.is_some_and(|id| self.group(id).is_some()));
            }
            if position == self.active {
                break;
            }
            index += 1;
            index += self
                .ghosts
                .iter()
                .filter(|ghost| ghost.after == Some(tab.id))
                .count();
        }
        index
    }

    /// The pointer dragging a row or header moved; `bounds` are the tab list's.
    fn drag_moved(
        &mut self,
        bounds: gpui::Bounds<Pixels>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let top = bounds.top() + self.tabs_scroll.offset().y;
        self.drag_over((position.y - top) / px(1.), top, cx);
    }

    fn set_sidebar_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        if (width - self.sidebar_width).abs() >= 0.5 {
            self.sidebar_width = width;
            cx.notify();
        }
    }

    /// What [`Sidebar`] draws.
    fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        self.sync_reveal(now);
        if self.pill.is_animating(now)
            || self
                .tabs
                .iter()
                .any(|tab| tab.shift.is_animating(now) || tab.reveal.is_animating(now))
            || self
                .groups
                .iter()
                .any(|group| group.shift.is_animating(now))
        {
            // Asked from the sidebar's render, so it is the sidebar that draws the next frame.
            window.request_animation_frame();
        }
        let chrome = Chrome::of(cx.global::<Theme>());
        // The active row stays in view: with 15+ tabs, ⌘1-9 selects below the fold.
        // Minimal reveal in this frame; unknown layout stays pending for the next.
        if self.scrolled_active.get() != self.active && !self.tabs.is_empty() {
            self.scrolled_active.set(self.active);
            self.tabs_scroll.scroll_to_item(self.active_child_index());
        }
        let mut rows: Vec<AnyElement> = self
            .ghosts
            .iter()
            .filter(|ghost| ghost.after.is_none())
            .map(|ghost| render_ghost(ghost, chrome, cx))
            .collect();
        let mut run = None;
        for (index, tab) in self.tabs.iter().enumerate() {
            if tab.group != run {
                run = tab.group;
                if let Some(group) = run.and_then(|id| self.group(id)) {
                    rows.push(self.render_group(group, now, chrome, cx));
                }
            }
            rows.push(self.render_row(index, tab, now, chrome, cx));
            rows.extend(
                self.ghosts
                    .iter()
                    .filter(|ghost| ghost.after == Some(tab.id))
                    .map(|ghost| render_ghost(ghost, chrome, cx)),
            );
        }
        div()
            .w(px(self.sidebar_width))
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
                    .track_scroll(&self.tabs_scroll)
                    .px(px(ROWS_INSET))
                    .flex()
                    .flex_col()
                    // Rows stop their clicks, so this is a double-click on the empty space.
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            this.new_tab(&NewTab, window, cx);
                        }
                    }))
                    // Where a dragged row or header goes is worked out from the list's
                    // layout as a whole: a header and a row differ in height.
                    .on_drag_move(cx.listener(|this, event: &DragMoveEvent<TabDrag>, _, cx| {
                        this.drag_moved(event.bounds, event.event.position, cx);
                    }))
                    .on_drag_move(
                        cx.listener(|this, event: &DragMoveEvent<GroupDrag>, _, cx| {
                            this.drag_moved(event.bounds, event.event.position, cx);
                        }),
                    )
                    .children(rows),
            )
            .child(memory_footer(self.memory, chrome))
            .child(new_tab_button(new_tab_keys(window), chrome, cx))
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
            .filter(|rename| rename.item == Item::Tab(id))
            .map(|rename| rename.field.clone());
        let status = tab.status(cx);
        let presence = Presence::of(tab, cx);
        let active = index == self.active;
        let lifted =
            cx.has_active_drag() && self.dragging.is_some_and(|drag| drag.item == Item::Tab(id));
        let preview = RowPreview {
            title: title.clone(),
            cwd: info.cwd.clone(),
            status,
            pinned: tab.pinned,
            chrome,
            grab_x: px(0.),
            width: self.sidebar_width - 2. * ROWS_INSET,
        };
        let pill = (active && !lifted).then(|| selection_pill(self.pill.value(now), chrome));
        // What shows of the row while its group folds or unfolds.
        let reveal = tab.reveal.value(now).clamp(0., 1.);
        let row = div()
            .id(ElementId::NamedInteger("tab".into(), id.0))
            .group("tab-row");
        // Not while renaming: a drag in the field selects its text.
        let row = draggable(row, id, editing.is_none().then_some(preview), cx)
            .relative()
            .top(px(tab.shift.value(now)))
            .flex_none()
            .h(px(ROW_HEIGHT * reveal))
            .when(reveal < 1., |row| row.overflow_hidden().opacity(reveal))
            .when(tab.group.is_some(), |row| {
                row.ml(px(GROUP_INDENT)).child(group_rail(chrome))
            })
            .rounded_lg();
        if lifted {
            return row.child(open_slot(chrome)).into_any_element();
        }
        let row = row
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
                if this
                    .rename
                    .as_ref()
                    .is_some_and(|rename| rename.item == Item::Tab(id))
                {
                    return;
                }
                if event.click_count() == 2 {
                    this.start_rename(Item::Tab(id), window, cx);
                } else {
                    this.activate_id(id, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(Item::Tab(id), event.position, window, cx);
                }),
            )
            .children(pill)
            .when_some(presence, |row, presence| {
                row.child(presence_ring(presence, chrome))
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(TITLE_INSET - STATUS_SLOT))
                    .child(status_mark(status, chrome))
                    .child(row_title(title, editing, active, chrome))
                    .when(tab.pinned, |row| row.child(pin_mark(chrome)))
                    .when_some(shown_memory(tab.memory), |row, shown| {
                        row.child(memory_label(shown, chrome))
                    })
                    .when_some(presence, |row, presence| {
                        row.child(presence_badge(presence, chrome))
                    })
                    .child(tab_close(id, chrome, cx)),
            )
            .child(row_cwd(info.cwd, chrome).relative());
        animate_row_in(row, id, reveal)
    }

    /// A group's header: click folds or unfolds it, a double-click names it, and a drag
    /// moves the whole group. Folded, it shows how many tabs it holds and their most
    /// urgent mark.
    fn render_group(
        &self,
        group: &Group,
        now: Instant,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = group.id;
        let editing = self
            .rename
            .as_ref()
            .filter(|rename| rename.item == Item::Group(id))
            .map(|rename| rename.field.clone());
        let dragging = self.dragging.filter(|_| cx.has_active_drag());
        let lifted = dragging.is_some_and(|drag| drag.item == Item::Group(id));
        let into = dragging.is_some_and(|drag| drag.into == Some(id));
        let (count, status) = self
            .tabs
            .iter()
            .filter(|tab| tab.group == Some(id))
            .fold((0, TabStatus::Idle), |(count, status), tab| {
                (count + 1, status.min(tab.status(cx)))
            });
        let header = div()
            .id(ElementId::NamedInteger("group".into(), id.0))
            .debug_selector(move || format!("group-{}", id.0))
            .relative()
            .top(px(group.shift.value(now)))
            .flex_none()
            .h(px(GROUP_HEIGHT))
            .rounded_lg();
        if lifted {
            return header.child(open_slot(chrome)).into_any_element();
        }
        let preview = GroupPreview {
            name: group.name.clone(),
            count,
            status,
            chrome,
            grab_x: px(0.),
            width: self.sidebar_width - 2. * ROWS_INSET,
        };
        let workspace = cx.entity().downgrade();
        header
            .when(editing.is_none(), |header| {
                header.on_drag(GroupDrag, move |_, grab, _, cx| {
                    let _ = workspace.update(cx, |this, cx| {
                        this.dragging = Some(RowDrag {
                            item: Item::Group(id),
                            grab,
                            slot_top: px(0.),
                            into: None,
                        });
                        cx.notify();
                    });
                    let preview = GroupPreview {
                        grab_x: grab.x,
                        ..preview.clone()
                    };
                    cx.new(|_| preview)
                })
            })
            .cursor_pointer()
            .hover(|style| style.bg(chrome.hover))
            // A dragged tab over it goes in when let go.
            .when(into, |header| {
                header
                    .bg(chrome.hover)
                    .border_1()
                    .border_color(chrome.accent)
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                if this
                    .rename
                    .as_ref()
                    .is_some_and(|rename| rename.item == Item::Group(id))
                {
                    return;
                }
                match event.click_count() {
                    1 => this.toggle_group(id, cx),
                    // The first click folded it; put that back, and name it.
                    2 => {
                        this.toggle_group(id, cx);
                        this.start_rename(Item::Group(id), window, cx);
                    }
                    _ => {}
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(Item::Group(id), event.position, window, cx);
                }),
            )
            .child(
                group_line(group.collapsed, chrome)
                    .size_full()
                    .child(group_name(group.name.clone(), editing, chrome))
                    .when(group.collapsed, |line| {
                        line.child(group_summary(count, status, chrome))
                    }),
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
                // Like a native menu: the dismissing click goes no further.
                cx.stop_propagation();
                this.close_menu(window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }));
        let panel = match menu.item {
            Item::Tab(id) => self.tab_menu(panel, id, chrome, cx),
            Item::Group(id) => {
                let item = |key, label, run: MenuAction<GroupId>, cx: &mut Context<Self>| {
                    menu_item(key, label, id, run, chrome, cx)
                };
                panel
                    .child(item(
                        "menu-rename-group",
                        "Rename Group…",
                        |this, id, window, cx| this.start_rename(Item::Group(id), window, cx),
                        cx,
                    ))
                    .child(item(
                        "menu-ungroup",
                        "Ungroup",
                        |this, id, _, cx| this.ungroup(id, cx),
                        cx,
                    ))
            }
        };
        deferred(
            anchored()
                .position(menu.position)
                .snap_to_window_with_margin(px(8.))
                .child(panel),
        )
        .with_priority(1)
    }

    /// A row's menu: Pin or Unpin, Rename, Reset Name, the group entries, Close.
    fn tab_menu(
        &self,
        panel: Stateful<Div>,
        id: TabId,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let tab = self.tabs.iter().find(|tab| tab.id == id);
        let named = tab.is_some_and(|tab| tab.name.is_some());
        let pinned = tab.is_some_and(|tab| tab.pinned);
        let grouped = tab.is_some_and(|tab| tab.group.is_some());
        let item = |key, label, run: MenuAction<TabId>, cx: &mut Context<Self>| {
            menu_item(key, label, id, run, chrome, cx)
        };
        let separator = || div().my_1().mx_2().h(px(1.)).bg(chrome.divider);
        panel
            .child(item(
                "menu-pin",
                if pinned { "Unpin Tab" } else { "Pin Tab" },
                |this, id, _, cx| this.toggle_pin(id, cx),
                cx,
            ))
            .child(item(
                "menu-rename",
                "Rename Tab…",
                |this, id, window, cx| this.start_rename(Item::Tab(id), window, cx),
                cx,
            ))
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
            // Pinned tabs are in no group.
            .when(!pinned, |panel| {
                panel
                    .child(separator())
                    .child(item(
                        "menu-new-group",
                        "New Group from Tab",
                        Self::new_group,
                        cx,
                    ))
                    .when(grouped, |panel| {
                        panel.child(item(
                            "menu-leave-group",
                            "Remove from Group",
                            |this, id, _, cx| this.leave_group(id, cx),
                            cx,
                        ))
                    })
            })
            .child(separator())
            .child(item("menu-close", "Close Tab", Self::close_tab_id, cx))
    }
}

/// What a menu entry does to the row or group it was opened on.
type MenuAction<T> = fn(&mut Workspace, T, &mut Window, &mut Context<Workspace>);

fn menu_item<T: Copy + 'static>(
    key: &'static str,
    label: &'static str,
    target: T,
    run: MenuAction<T>,
    chrome: Chrome,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    div()
        .id(key)
        .debug_selector(move || key.to_owned())
        .h(px(26.))
        .px_2()
        .flex()
        .items_center()
        .rounded_md()
        .cursor_pointer()
        .hover(|style| style.bg(chrome.accent).text_color(on_accent()))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.close_menu(window, cx);
            run(this, target, window, cx);
        }))
        .child(label)
}

/// Runs `Quit` in the terminal window, for when another window (Settings) has focus. With the
/// terminal window in front, its own handler takes the action, and this does nothing.
pub fn quit_app(cx: &mut App) {
    if !terminal_window_is_active(cx) {
        in_terminal_window(cx, |this, window, cx| this.quit(&Quit, window, cx));
    }
}

/// Quits without asking about running jobs: they keep running, and the next launch is back
/// on them (Restart to Update).
pub fn detach_and_quit(cx: &mut App) {
    in_terminal_window(cx, |this, _, cx| {
        this.depart(Leave::Detach, cx);
        cx.quit();
    });
}

/// Runs `QuitAndStopJobs` in the terminal window, for when another window has focus. With the
/// terminal window in front, its own handler takes the action, and this does nothing.
pub fn quit_and_stop_app(cx: &mut App) {
    if !terminal_window_is_active(cx) {
        in_terminal_window(cx, |this, window, cx| {
            this.quit_and_stop(&QuitAndStopJobs, window, cx);
        });
    }
}

/// Runs `Restart` in the terminal window, for when another window has focus. With the
/// terminal window in front, its own handler takes the action, and this does nothing.
pub fn restart_app(cx: &mut App) {
    if terminal_window_is_active(cx) {
        return;
    }
    if cx
        .windows()
        .iter()
        .any(|w| w.downcast::<Workspace>().is_some())
    {
        in_terminal_window(cx, |this, window, cx| this.restart(&Restart, window, cx));
        return;
    }
    // No terminal window: there is nothing to stop or save, so Kil only opens again.
    cx.defer(|cx| {
        if crate::restart_kil() {
            cx.quit();
        }
    });
}

fn terminal_window_is_active(cx: &App) -> bool {
    cx.active_window()
        .is_some_and(|window| window.downcast::<Workspace>().is_some())
}

/// Runs `run` in the terminal window, once the current update is over: a menu action is
/// dispatched inside the active window's update, and a window can't be updated again from
/// inside its own (GPUI takes it out of the app meanwhile), so done at once from the terminal
/// window's own menu, `run` ran on no window at all.
fn in_terminal_window(
    cx: &mut App,
    run: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) {
    cx.defer(move |cx| {
        match cx
            .windows()
            .iter()
            .find_map(AnyWindowHandle::downcast::<Workspace>)
        {
            Some(workspace) => {
                let updated = workspace.update(cx, |this, window, cx| {
                    window.activate_window();
                    run(this, window, cx);
                });
                if let Err(err) = updated {
                    log::error!("terminal window: {err:#}");
                }
            }
            None => cx.quit(),
        }
    });
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
        let base = div().absolute().inset_0().bg(chrome.dim);
        if !Motion::enabled() {
            return if focused {
                base.opacity(0.).into_any_element()
            } else {
                base.opacity(1.).into_any_element()
            };
        }
        let name = if focused { "focus-in" } else { "focus-out" };
        base.with_animation(
            ElementId::NamedInteger(name.into(), pane.entity_id().as_u64()),
            Animation::new(motion::FOCUS_DIM),
            move |overlay, t| overlay.opacity(if focused { 1. - t } else { t }),
        )
        .into_any_element()
    });
    // Inset the hairline so dividers painted above the panes cannot cover it.
    let ring = (several && focused).then(|| {
        let base = div()
            .absolute()
            .inset(px(1.))
            .border_1()
            .border_color(chrome.accent.opacity(0.75))
            .debug_selector(|| "focus-ring".to_owned());
        if Motion::enabled() {
            base.with_animation(
                ElementId::NamedInteger("focus-ring".into(), pane.entity_id().as_u64()),
                Animation::new(motion::FOCUS_DIM),
                gpui::Styled::opacity,
            )
            .into_any_element()
        } else {
            base.into_any_element()
        }
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
        .children(ring)
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

/// Split Right and Split Down, at the top right of the tab: quiet until the pointer comes.
fn split_buttons(chrome: Chrome, cx: &mut Context<Workspace>) -> impl IntoElement {
    let button = |axis: Axis| {
        let horizontal = axis == Axis::Horizontal;
        // A pane with its divider: across for Split Right, down for Split Down.
        let line = div().absolute().bg(chrome.text).map(|line| {
            if horizontal {
                line.top_0().bottom_0().left(px(6.)).w(px(1.))
            } else {
                line.left_0().right_0().top(px(4.5)).h(px(1.))
            }
        });
        div()
            .id(if horizontal {
                "split-right"
            } else {
                "split-down"
            })
            .size(px(SPLIT_BUTTONS_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(chrome.hover))
            .debug_selector(move || {
                if horizontal {
                    "split-right"
                } else {
                    "split-down"
                }
                .to_owned()
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.split(axis, window, cx);
            }))
            .child(
                div()
                    .relative()
                    .w(px(14.))
                    .h(px(11.))
                    .rounded_sm()
                    .border_1()
                    .border_color(chrome.text)
                    .child(line),
            )
    };
    div()
        .id("split-buttons")
        .absolute()
        .top_2()
        .right_2()
        .p(px(2.))
        .flex()
        .gap(px(2.))
        .rounded_lg()
        .bg(chrome.terminal_bg)
        .opacity(0.45)
        .hover(|style| style.opacity(1.))
        // Clicks here are the buttons', not the pane's below.
        .occlude()
        .child(button(Axis::Horizontal))
        .child(button(Axis::Vertical))
}

/// The 1px line plus a 6px grab area. Drawn last so the grab area sits above both panes.
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
                    .w(px(6.))
                    .cursor(CursorStyle::ResizeLeftRight)
            } else {
                el.left_0()
                    .right_0()
                    .top(px(-3.))
                    .h(px(6.))
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

    fn pin_tab(&mut self, _: &PinTab, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.get(self.active).map(|tab| tab.id) {
            self.set_pinned(id, true, cx);
        }
    }

    fn unpin_tab(&mut self, _: &UnpinTab, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.get(self.active).map(|tab| tab.id) {
            self.set_pinned(id, false, cx);
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

    /// Through the tabs that show: a folded group's tabs are skipped.
    fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown_tabs();
        let count = shown.len();
        if count > 1 {
            let at = shown
                .iter()
                .position(|&index| index == self.active)
                .unwrap_or(0);
            let next = if forward {
                (at + 1) % count
            } else {
                (at + count - 1) % count
            };
            self.activate(shown[next], window, cx);
        }
    }
}

fn split_id_bits(tab: TabId, split: SplitId) -> u64 {
    (tab.0 << 32) ^ split.bits()
}

/// Lets `row` be dragged when it has a `preview`, the row lifted. Where it goes is the
/// tab list's to work out ([`Workspace::drag_over`]).
fn draggable(
    row: Stateful<Div>,
    id: TabId,
    preview: Option<RowPreview>,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    let workspace = cx.entity().downgrade();
    row.when_some(preview, |row, preview| {
        row.on_drag(TabDrag, move |_, grab, _, cx| {
            let _ = workspace.update(cx, |this, cx| {
                this.dragging = Some(RowDrag {
                    item: Item::Tab(id),
                    grab,
                    slot_top: px(0.),
                    into: None,
                });
                cx.notify();
            });
            let preview = RowPreview {
                grab_x: grab.x,
                ..preview.clone()
            };
            cx.new(|_| preview)
        })
    })
}

/// The active row's highlight, `offset` pixels from the row while it slides over.
fn selection_pill(offset: f32, chrome: Chrome) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .top(px(offset))
        .h(px(ROW_HEIGHT))
        .rounded_lg()
        .bg(chrome.selected)
}

/// The open slot a lifted row or header will drop into.
fn open_slot(chrome: Chrome) -> Div {
    div()
        .size_full()
        .rounded_lg()
        .border_1()
        .border_color(chrome.accent.opacity(0.5))
        .bg(chrome.hover)
}

/// The line down the left of a group's rows, under its header's fold mark.
fn group_rail(chrome: Chrome) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(-GROUP_INDENT / 2. - 0.5))
        .w(px(1.))
        .bg(chrome.divider)
}

/// A group header's line: the fold mark, centered over the rail, then what the caller adds.
fn group_line(collapsed: bool, chrome: Chrome) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(TITLE_INSET - STATUS_SLOT))
        .pl(px((GROUP_INDENT - STATUS_SLOT) / 2.))
        .pr_3()
        .child(
            div()
                .flex_none()
                .size(px(STATUS_SLOT))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(10.))
                .text_color(chrome.muted)
                .child(if collapsed { "▸" } else { "▾" }),
        )
}

/// A group's name, or the field editing it.
fn group_name(name: SharedString, editing: Option<Entity<TextField>>, chrome: Chrome) -> Div {
    let name_box = div()
        .flex_1()
        .min_w_0()
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD);
    match editing {
        Some(field) => name_box.text_color(chrome.text).child(field),
        None => name_box.truncate().text_color(chrome.muted).child(name),
    }
}

/// What a folded group holds: how many tabs, and the most urgent of their marks.
fn group_summary(count: usize, status: TabStatus, chrome: Chrome) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(11.))
        .text_color(chrome.muted)
        .child(count.to_string())
        .when(status != TabStatus::Idle, |summary| {
            summary.child(status_mark(status, chrome))
        })
}

/// The sidebar's right edge: dragging it resizes the sidebar, and a double-click puts it
/// back to its usual width (A31).
fn sidebar_edge(width: f32, cx: &mut Context<Workspace>) -> impl IntoElement {
    div()
        .id("sidebar-edge")
        .debug_selector(|| "sidebar-edge".to_owned())
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(width - 3.))
        .w(px(6.))
        .cursor(CursorStyle::ResizeLeftRight)
        // The pane under its right half doesn't take the press.
        .occlude()
        .on_drag(SidebarDrag, |_, _, _, cx| cx.new(|_| EmptyDrag))
        .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
            if event.click_count() == 2 {
                this.set_sidebar_width(SIDEBAR_WIDTH, cx);
            }
        }))
}

/// A pushpin in a pinned row's title line, drawn so Kil needs no icon font.
fn pin_mark(chrome: Chrome) -> Div {
    div()
        .flex_none()
        .relative()
        .w(px(8.))
        .h(px(11.))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px(4.))
                .rounded_sm()
                .border_1()
                .border_color(chrome.muted),
        )
        .child(
            div()
                .absolute()
                .top(px(4.))
                .bottom(px(1.))
                .left(px(3.5))
                .w(px(1.))
                .bg(chrome.muted),
        )
        .debug_selector(|| "pinned-tab".to_owned())
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
    let mut row = div().mx_2().mb_1().flex().gap_2().text_size(px(11.));
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
    if !shown {
        // An invisible line keeps the row's height while quiet, so the New Tab button
        // below never moves.
        row = row.child(div().text_color(chrome.muted.opacity(0.)).child("X"));
    }
    row
}

/// The keys bound to New Tab, as a menu writes them (`⌘T`), or `None` if it has none.
fn new_tab_keys(window: &Window) -> Option<String> {
    let context = KeyContext::parse("Workspace").ok()?;
    let binding = window.highest_precedence_binding_for_action_in_context(&NewTab, context)?;
    let keys: Vec<String> = binding
        .keystrokes()
        .iter()
        .map(KeybindingKeystroke::unparse)
        .collect();
    Some(keymap::display(&keys.join(" ")))
}

/// The sidebar's footer button. Its plus sits in the status column and "New Tab" in line
/// with the titles above it, so it reads as the row that would come next; the keys that do
/// the same show at the right while the pointer is over it, the way the rows' × does.
fn new_tab_button(
    keys: Option<String>,
    chrome: Chrome,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    div()
        .id("new-tab")
        .debug_selector(|| "new-tab".to_owned())
        .group("new-tab")
        .mx(px(ROWS_INSET))
        .mb(px(ROWS_INSET))
        .h(px(NEW_TAB_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .gap(px(TITLE_INSET - STATUS_SLOT))
        .rounded_lg()
        .text_size(px(13.))
        .text_color(chrome.muted)
        .cursor_pointer()
        .hover(|style| style.bg(chrome.hover).text_color(chrome.text))
        .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
            if event.click_count() == 1 {
                this.new_tab(&NewTab, window, cx);
            }
        }))
        .child(plus_mark(chrome))
        .child(div().flex_1().child("New Tab"))
        .when_some(keys, |button, keys| {
            button.child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(chrome.muted)
                    .opacity(0.)
                    .group_hover("new-tab", |style| style.opacity(1.))
                    .child(keys),
            )
        })
}

/// A plus drawn from two bars, so it is centered in its slot: a font's `+` sits where the
/// typeface puts it, which is not the middle of the line.
fn plus_mark(chrome: Chrome) -> Div {
    const BAR: f32 = 1.5;
    let bar = |chrome: Chrome| {
        div()
            .absolute()
            .rounded(px(BAR / 2.))
            .bg(chrome.muted)
            .group_hover("new-tab", move |style| style.bg(chrome.text))
    };
    let inset = (STATUS_SLOT - BAR) / 2.;
    div()
        .flex_none()
        .relative()
        .size(px(STATUS_SLOT))
        .child(bar(chrome).left_0().right_0().top(px(inset)).h(px(BAR)))
        .child(bar(chrome).top_0().bottom_0().left(px(inset)).w(px(BAR)))
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

/// A phone on one of a tab's panes (DESIGN.md §14.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    /// Attached: it sees the pane.
    Watching,
    /// It typed in the last few seconds: it drives the pane.
    Driving,
}

impl Presence {
    /// Driving wins over watching, across all of the tab's panes.
    fn of(tab: &Tab, cx: &App) -> Option<Self> {
        let panes = tab.tree.leaves();
        if panes.iter().any(|pane| pane.read(cx).controlled()) {
            Some(Self::Driving)
        } else if panes.iter().any(|pane| pane.read(cx).watched()) {
            Some(Self::Watching)
        } else {
            None
        }
    }
}

/// The row of a tab a phone is on, outlined in the accent: a hairline while it watches,
/// full strength with a faint wash while it drives. An overlay with no handlers, so the row
/// neither shifts nor loses a click, and static, so idle CPU stays at zero.
fn presence_ring(presence: Presence, chrome: Chrome) -> Div {
    let ring = div().absolute().inset_0().rounded_lg().border_1();
    match presence {
        Presence::Watching => ring
            .border_color(chrome.accent.opacity(0.5))
            .debug_selector(|| "watched-row".to_owned()),
        Presence::Driving => ring
            .border_color(chrome.accent)
            .bg(chrome.accent.opacity(0.08))
            .debug_selector(|| "controlling-row".to_owned()),
    }
}

/// "From the phone", in the row's title line: a handset in a capsule, tinted while the phone
/// watches and solid while it drives. Accent, never the failure red, which next to the
/// status mark would read as an error.
fn presence_badge(presence: Presence, chrome: Chrome) -> Div {
    let (fill, ink, selector) = match presence {
        Presence::Watching => (chrome.accent.opacity(0.18), chrome.accent, "watched-tab"),
        Presence::Driving => (chrome.accent, on_accent(), "controlling-tab"),
    };
    div()
        .flex_none()
        .h(px(16.))
        .px(px(5.))
        .flex()
        .items_center()
        .rounded_full()
        .bg(fill)
        .child(handset(ink))
        .debug_selector(move || selector.to_owned())
}

/// A phone, drawn: a rounded outline with a home bar. Kil ships no icon font or images.
fn handset(ink: Hsla) -> Div {
    div()
        .flex_none()
        .w(px(7.))
        .h(px(11.))
        .rounded(px(2.))
        .border_1()
        .border_color(ink)
        .flex()
        .justify_center()
        .items_end()
        .pb(px(1.))
        .child(div().w(px(3.)).h(px(1.)).rounded_full().bg(ink))
}

/// The row's title, or the field editing it.
fn row_title(
    title: SharedString,
    editing: Option<Entity<TextField>>,
    active: bool,
    chrome: Chrome,
) -> Div {
    let title_box = div()
        .flex_1()
        .min_w_0()
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD);
    match editing {
        Some(field) => title_box.text_color(chrome.text).child(field),
        None => title_box
            .truncate()
            .text_color(if active { chrome.text } else { chrome.muted })
            .child(title),
    }
}

/// The row's second line: where its focused pane is.
fn row_cwd(cwd: SharedString, chrome: Chrome) -> Div {
    div()
        .pl(px(TITLE_INSET))
        .truncate()
        .text_size(px(11.))
        .text_color(chrome.muted)
        .child(cwd)
}

/// A new row, grown in unless motion is off (then at full height at once), to the part of
/// it that shows (`reveal`) while its group folds or unfolds.
fn animate_row_in(row: Stateful<Div>, id: TabId, reveal: f32) -> AnyElement {
    if Motion::enabled() {
        row.with_animation(
            ElementId::NamedInteger("tab-in".into(), id.0),
            Animation::new(motion::ROW_IN).with_easing(|t| Ease::OutQuint.apply(t)),
            move |row, t| row.h(px(ROW_HEIGHT * t * reveal)).opacity(t * reveal),
        )
        .into_any_element()
    } else {
        row.into_any_element()
    }
}

/// The active tab's content, faded in unless motion is off.
fn animate_tab_show(base: Div, id: TabId) -> AnyElement {
    if Motion::enabled() {
        base.with_animation(
            ElementId::NamedInteger("tab-show".into(), id.0),
            Animation::new(motion::TAB_SWITCH).with_easing(|t| Ease::OutQuint.apply(t)),
            |el, t| el.opacity(t).top(px(6. * (1. - t))),
        )
        .into_any_element()
    } else {
        base.into_any_element()
    }
}

fn render_ghost(ghost: &Ghost, chrome: Chrome, cx: &mut Context<Workspace>) -> AnyElement {
    let row = div()
        .id(ElementId::NamedInteger("tab-ghost".into(), ghost.id.0))
        // A closing row swallows clicks: without this, the second click of a double-click
        // on its × falls through to the list and opens a new tab.
        .occlude()
        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
        }))
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
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(chrome.muted)
                .child(ghost.title.clone()),
        );
    if Motion::enabled() {
        row.with_animation(
            ElementId::NamedInteger("tab-out".into(), ghost.id.0),
            Animation::new(motion::ROW_OUT).with_easing(|t| Ease::InOut.apply(t)),
            |row, t| row.h(px(ROW_HEIGHT * (1. - t))).opacity(1. - t),
        )
        .into_any_element()
    } else {
        row.h(px(0.)).opacity(0.).into_any_element()
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn bind_actions(root: Stateful<Div>, cx: &mut Context<Workspace>) -> Stateful<Div> {
    root.on_action(cx.listener(Workspace::new_tab))
        .on_action(cx.listener(Workspace::close_tab))
        .on_action(cx.listener(Workspace::pin_tab))
        .on_action(cx.listener(Workspace::unpin_tab))
        .on_action(cx.listener(Workspace::close_pane))
        .on_action(cx.listener(Workspace::quit))
        .on_action(cx.listener(Workspace::quit_and_stop))
        .on_action(cx.listener(Workspace::restart))
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

impl Workspace {
    /// Notices at the top right of the tab; a click clears them.
    fn render_notices(
        &self,
        window: &Window,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        (!self.notices.is_empty()).then(|| {
            // Wide enough for a sentence, but never over the sidebar in a narrow window.
            let room = window.viewport_size().width - px(self.sidebar_width + 24.);
            let max_notice = if room < px(0.) {
                px(0.)
            } else if room < px(460.) {
                room
            } else {
                px(460.)
            };
            div()
                .id("notices")
                .absolute()
                .top(px(SPLIT_BUTTONS_HEIGHT + 16.))
                .right_3()
                .max_w(max_notice)
                .flex()
                .flex_col()
                .gap_2()
                .cursor_pointer()
                .occlude()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
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
        })
    }
}

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
        // A row let go outside the window drops nowhere; the drag just ends.
        if self.dragging.is_some() && !cx.has_active_drag() {
            self.dragging = None;
            self.sidebar.update(cx, |_, cx| cx.notify());
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
            let base = div().relative().size_full().child(render_node(
                tab.tree.root(),
                tab,
                now,
                chrome,
                cx,
            ));
            animate_tab_show(base, tab.id)
        });
        let notices = self.render_notices(window, chrome, cx);

        let root = div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle);
        let root = select_tab_listeners(bind_actions(root, cx), cx);
        root.relative()
            .flex()
            .size_full()
            .on_drop(cx.listener(|this, _: &TabDrag, window, cx| this.drop_row(window, cx)))
            .on_drop(cx.listener(|this, _: &GroupDrag, window, cx| this.drop_row(window, cx)))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<SidebarDrag>, _, cx| {
                    let x = event.event.position.x - event.bounds.left();
                    this.set_sidebar_width(x / px(1.), cx);
                }),
            )
            .child(
                AnyView::from(self.sidebar.clone()).cached(
                    StyleRefinement::default()
                        .w(px(self.sidebar_width))
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
                    .when(!self.tabs.is_empty(), |el| {
                        el.child(split_buttons(chrome, cx))
                    })
                    .children(notices),
            )
            .child(sidebar_edge(self.sidebar_width, cx))
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

    /// Right-click the first row, then pick "Rename Tab…".
    fn start_rename(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        click(cx, ROW, MouseButton::Right, 1);
        assert!(workspace.read_with(cx, |this, _| this.menu.is_some()));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let item = cx.debug_bounds("menu-rename").expect("rename item drawn");
        click(cx, item.center(), MouseButton::Left, 1);
        assert!(workspace.read_with(cx, |this, _| this.menu.is_none() && this.rename.is_some()));
    }

    fn toggle_pin_from_menu(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
        index: usize,
    ) {
        let row = ROW
            + point(
                px(0.),
                px(ROW_HEIGHT * f32::from(u16::try_from(index).expect("small test row"))),
            );
        click(cx, row, MouseButton::Right, 1);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let item = cx.debug_bounds("menu-pin").expect("pin item drawn");
        click(cx, item.center(), MouseButton::Left, 1);
        std::thread::sleep(motion::ROW_SHIFT + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |this, _| this.menu.is_none()));
    }

    fn tab_layout(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> (Vec<SharedString>, Vec<bool>, Option<SharedString>) {
        workspace.read_with(cx, |this, _| {
            (
                this.tabs
                    .iter()
                    .filter_map(|tab| tab.name.clone())
                    .collect(),
                this.tabs.iter().map(|tab| tab.pinned).collect(),
                this.tabs[this.active].name.clone(),
            )
        })
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
    fn the_memory_watch_should_read_the_panes_on_its_own(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        wait_for(cx, |cx| {
            workspace.read_with(cx, |this, cx| this.tabs[0].focused.read(cx).pid().is_some())
        });
        let mut memory = 0;
        for _ in 0..50 {
            cx.executor().advance_clock(MEMORY_INTERVAL);
            cx.run_until_parked();
            memory = workspace.read_with(cx, |this, _| this.tabs[0].memory);
            if memory > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(memory > 0, "the timer's reading never reached the tab");
        assert!(workspace.read_with(cx, |this, _| this.memory.total) >= memory);
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
    fn a_tab_made_before_its_shell_is_up_should_have_its_folder_and_splits_follow_it(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx);
        let dir = std::path::PathBuf::from("/usr");
        cx.update(|_, cx| {
            let settings = Settings {
                default_directory: Some(dir.clone()),
                ..cx.global::<Settings>().clone()
            };
            cx.set_global(settings);
        });
        // Each split comes before the pane it splits has its shell.
        workspace.update_in(cx, |this, window, cx| {
            this.new_tab(&NewTab, window, cx);
            this.split(Axis::Horizontal, window, cx);
            this.split(Axis::Vertical, window, cx);
        });
        // Named for its folder at once, not "shell" until its shell is up.
        let title = workspace.read_with(cx, |this, cx| this.tabs[this.active].title(cx));
        assert_eq!(title, SharedString::from("usr"));
        let dirs = workspace.read_with(cx, |this, cx| {
            let tab = &this.tabs[this.active];
            tab.tree
                .leaves()
                .iter()
                .map(|pane| pane.read(cx).start_dir().cloned())
                .collect::<Vec<_>>()
        });
        assert_eq!(dirs, vec![Some(dir); 3]);
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
        // The row is lifted off the list, its slot left open where it will drop.
        let lifted = workspace.read_with(cx, |this, _| this.dragging.map(|drag| drag.item));
        assert_eq!(
            lifted,
            workspace.read_with(cx, |this, _| Some(Item::Tab(this.tabs[2].id)))
        );
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
        assert!(workspace.read_with(cx, |this, _| this.dragging.is_none()));
    }

    #[gpui::test]
    fn pin_menu_should_group_tabs_and_keep_the_active_one(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        for _ in 0..3 {
            cx.dispatch_action(NewTab);
        }
        workspace.update(cx, |this, _| {
            for (tab, name) in this.tabs.iter_mut().zip(["a", "b", "c", "d"]) {
                tab.name = Some(name.into());
            }
        });
        std::thread::sleep(motion::ROW_IN + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        toggle_pin_from_menu(&workspace, cx, 2);
        assert_eq!(
            tab_layout(&workspace, cx),
            (
                vec!["c".into(), "a".into(), "b".into(), "d".into()],
                vec![true, false, false, false],
                Some("d".into()),
            )
        );
        assert!(cx.debug_bounds("pinned-tab").is_some());

        toggle_pin_from_menu(&workspace, cx, 2);
        assert_eq!(
            tab_layout(&workspace, cx),
            (
                vec!["c".into(), "b".into(), "a".into(), "d".into()],
                vec![true, true, false, false],
                Some("d".into()),
            )
        );

        toggle_pin_from_menu(&workspace, cx, 0);
        assert_eq!(
            tab_layout(&workspace, cx),
            (
                vec!["b".into(), "c".into(), "a".into(), "d".into()],
                vec![true, false, false, false],
                Some("d".into()),
            )
        );
    }

    #[gpui::test]
    fn pin_shortcuts_should_change_only_the_active_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        for _ in 0..2 {
            cx.dispatch_action(NewTab);
        }
        workspace.update(cx, |this, _| {
            for (tab, name) in this.tabs.iter_mut().zip(["a", "b", "c"]) {
                tab.name = Some(name.into());
            }
        });
        cx.update(|_, cx| {
            let (table, errors) = crate::keymap::resolve(None);
            assert_eq!(errors, Vec::new());
            cx.bind_keys(crate::keymap::bindings(&table));
        });

        cx.simulate_keystrokes("cmd-shift-p");
        let pinned = tab_layout(&workspace, cx);
        assert_eq!(
            pinned,
            (
                vec!["c".into(), "a".into(), "b".into()],
                vec![true, false, false],
                Some("c".into()),
            )
        );
        // Pinning is idempotent, not a toggle.
        cx.simulate_keystrokes("cmd-shift-p");
        assert_eq!(tab_layout(&workspace, cx), pinned);

        cx.simulate_keystrokes("cmd-alt-shift-p");
        assert_eq!(
            tab_layout(&workspace, cx),
            (
                vec!["c".into(), "a".into(), "b".into()],
                vec![false, false, false],
                Some("c".into()),
            )
        );
    }

    #[gpui::test]
    fn dragging_a_row_should_stay_in_its_pinned_group(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        for _ in 0..3 {
            cx.dispatch_action(NewTab);
        }
        workspace.update(cx, |this, cx| {
            for (tab, name) in this.tabs.iter_mut().zip(["a", "b", "c", "d"]) {
                tab.name = Some(name.into());
            }
            this.toggle_pin(this.tabs[0].id, cx);
            this.toggle_pin(this.tabs[1].id, cx);
        });
        std::thread::sleep(motion::ROW_IN + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        let drag = |cx: &mut VisualTestContext, from: usize, to: usize| {
            let start = ROW
                + point(
                    px(0.),
                    px(ROW_HEIGHT * f32::from(u16::try_from(from).expect("small test row"))),
                );
            let end = ROW
                + point(
                    px(0.),
                    px(ROW_HEIGHT * f32::from(u16::try_from(to).expect("small test row"))),
                );
            cx.simulate_event(MouseDownEvent {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
                click_count: 1,
                first_mouse: false,
            });
            for step in (1..=8u8).map(f32::from) {
                cx.simulate_event(gpui::MouseMoveEvent {
                    position: start + (end - start) * (step / 8.),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::none(),
                });
            }
            cx.simulate_event(MouseUpEvent {
                position: end,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
                click_count: 1,
            });
            std::thread::sleep(motion::ROW_DROP + Duration::from_millis(20));
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        };
        // An ordinary row cannot take a pinned row's place.
        drag(cx, 2, 1);
        assert_eq!(
            tab_layout(&workspace, cx).0,
            vec![
                SharedString::from("a"),
                SharedString::from("b"),
                SharedString::from("c"),
                SharedString::from("d"),
            ]
        );
        // It still reorders among ordinary rows, and the active tab stays active.
        drag(cx, 2, 3);
        assert_eq!(
            tab_layout(&workspace, cx),
            (
                vec!["a".into(), "b".into(), "d".into(), "c".into()],
                vec![true, true, false, false],
                Some("d".into()),
            )
        );
    }

    /// A workspace of tabs named `names`, the last one active, at rest.
    fn named_tabs<'a>(
        names: &[&'static str],
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        let (workspace, cx) = workspace(cx);
        for _ in 1..names.len() {
            cx.dispatch_action(NewTab);
        }
        workspace.update(cx, |this, _| {
            for (tab, name) in this.tabs.iter_mut().zip(names) {
                tab.name = Some((*name).into());
            }
        });
        settle(cx, motion::ROW_IN);
        (workspace, cx)
    }

    /// Lets animations of `duration` finish, then draws.
    fn settle(cx: &mut VisualTestContext, duration: Duration) {
        std::thread::sleep(duration + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    /// Presses at `from` and drags to `to` in small moves, as a hand would, then lets go.
    fn drag_between(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
        cx.simulate_event(MouseDownEvent {
            position: from,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        });
        for step in (1..=8u8).map(f32::from) {
            cx.simulate_event(gpui::MouseMoveEvent {
                position: from + (to - from) * (step / 8.),
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers::none(),
            });
        }
        cx.simulate_event(MouseUpEvent {
            position: to,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        settle(cx, motion::ROW_DROP);
    }

    /// A point in the sidebar `y` pixels down the tab list.
    fn in_list(y: f32) -> Point<Pixels> {
        point(px(100.), px(SIDEBAR_TOP + y))
    }

    /// Each tab's name, and the name of the group it is in.
    fn grouping(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Vec<(String, Option<String>)> {
        workspace.read_with(cx, |this, _| {
            this.tabs
                .iter()
                .map(|tab| {
                    (
                        tab.name
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        tab.group
                            .and_then(|id| this.group(id))
                            .map(|group| group.name.to_string()),
                    )
                })
                .collect()
        })
    }

    fn grouped(rows: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        rows.iter()
            .map(|(name, group)| ((*name).to_owned(), group.map(str::to_owned)))
            .collect()
    }

    #[gpui::test]
    fn a_group_should_be_made_from_the_menu_and_take_rows_dragged_in_and_out(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = named_tabs(&["a", "b", "c", "d"], cx);
        // New Group from Tab on "b", named as it is made.
        click(cx, in_list(ROW_HEIGHT * 1.5), MouseButton::Right, 1);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let item = cx
            .debug_bounds("menu-new-group")
            .expect("new group item drawn");
        click(cx, item.center(), MouseButton::Left, 1);
        assert!(workspace.read_with(cx, |this, _| {
            this.rename
                .as_ref()
                .is_some_and(|rename| matches!(rename.item, Item::Group(_)))
        }));
        cx.simulate_input("srv");
        cx.simulate_keystrokes("enter");
        settle(cx, motion::ROW_SHIFT);
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[("a", None), ("b", Some("srv")), ("c", None), ("d", None)])
        );
        // a 0–44, the header 44–72, b 72–116, c 116–160, d 160–204. "c" dragged up onto
        // "b" takes its place in the group.
        drag_between(cx, in_list(138.), in_list(94.));
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[
                ("a", None),
                ("c", Some("srv")),
                ("b", Some("srv")),
                ("d", None)
            ])
        );
        // "d" dragged up through the group and past its header leaves it, above the header.
        drag_between(cx, in_list(182.), in_list(60.));
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[
                ("a", None),
                ("d", None),
                ("c", Some("srv")),
                ("b", Some("srv"))
            ])
        );
        // "a" dragged down past the header goes in at the top of the group.
        drag_between(cx, in_list(22.), in_list(102.));
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[
                ("d", None),
                ("a", Some("srv")),
                ("c", Some("srv")),
                ("b", Some("srv"))
            ])
        );
        // Below the last row: the end of the list, in no group.
        drag_between(cx, in_list(94.), in_list(400.));
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[
                ("d", None),
                ("c", Some("srv")),
                ("b", Some("srv")),
                ("a", None)
            ])
        );
        // The active tab stayed active throughout.
        assert_eq!(tab_layout(&workspace, cx).2, Some("d".into()));
        // Remove from Group puts "c" after the group; pinning "b" takes it out too, and the
        // emptied group goes.
        workspace.update(cx, |this, cx| {
            this.leave_group(this.tabs[1].id, cx);
            let b = this.tabs[1].id;
            assert_eq!(this.tabs[1].name, Some("b".into()));
            this.toggle_pin(b, cx);
        });
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[("b", None), ("d", None), ("c", None), ("a", None)])
        );
        assert!(workspace.read_with(cx, |this, _| this.groups.is_empty()));
    }

    #[gpui::test]
    fn a_folded_group_should_hide_its_tabs_from_the_tab_shortcuts(cx: &mut TestAppContext) {
        let (workspace, cx) = named_tabs(&["a", "b", "c", "d"], cx);
        let group = workspace.update_in(cx, |this, window, cx| {
            this.new_group(this.tabs[1].id, window, cx);
            this.rename = None;
            let group = this.tabs[1].group.expect("grouped");
            this.tabs[2].group = Some(group);
            this.activate(0, window, cx);
            group
        });
        settle(cx, motion::ROW_SHIFT);
        // A click on the header folds the group.
        let selector: &'static str = format!("group-{}", group.0).leak();
        let header = cx.debug_bounds(selector).expect("header drawn");
        click(cx, header.center(), MouseButton::Left, 1);
        settle(cx, motion::ROW_OUT);
        assert!(workspace.read_with(cx, |this, _| this.groups[0].collapsed));
        let active = |cx: &mut VisualTestContext| tab_layout(&workspace, cx).2;
        cx.dispatch_action(SelectTab2);
        assert_eq!(active(cx), Some("d".into()));
        cx.dispatch_action(NextTab);
        assert_eq!(active(cx), Some("a".into()));
        cx.dispatch_action(PrevTab);
        assert_eq!(active(cx), Some("d".into()));
        cx.dispatch_action(SelectTab9);
        assert_eq!(active(cx), Some("d".into()));
        // A folded tab made active (a notification's click) shows under the header, and
        // goes back out of sight once another tab is.
        workspace.update_in(cx, |this, window, cx| this.activate(1, window, cx));
        settle(cx, motion::ROW_IN);
        let shown = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |this, _| {
                this.tabs
                    .iter()
                    .map(|tab| tab.reveal.target() > 0.)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(shown(cx), vec![true, true, false, true]);
        cx.dispatch_action(NextTab);
        assert_eq!(active(cx), Some("d".into()));
        settle(cx, motion::ROW_OUT);
        assert_eq!(shown(cx), vec![true, false, false, true]);
        // Clicked again, it unfolds.
        click(cx, header.center(), MouseButton::Left, 1);
        settle(cx, motion::ROW_IN);
        assert_eq!(shown(cx), vec![true, true, true, true]);
    }

    #[gpui::test]
    fn a_tab_let_go_on_a_folded_header_should_go_into_its_group(cx: &mut TestAppContext) {
        let (workspace, cx) = named_tabs(&["a", "b", "c", "d"], cx);
        workspace.update_in(cx, |this, window, cx| {
            this.new_group(this.tabs[1].id, window, cx);
            this.rename = None;
            let group = this.tabs[1].group.expect("grouped");
            this.toggle_group(group, cx);
        });
        settle(cx, motion::ROW_OUT);
        // a 0–44, the folded header 44–72, c 72–116, d 116–160.
        drag_between(cx, in_list(94.), in_list(58.));
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[
                ("a", None),
                ("b", Some("Group")),
                ("c", Some("Group")),
                ("d", None)
            ])
        );
        settle(cx, motion::ROW_OUT);
        let hidden = workspace.read_with(cx, |this, _| this.tabs[2].reveal.target());
        assert!(hidden < 0.5, "folded away with its group");
    }

    #[gpui::test]
    fn dragging_a_group_header_should_move_the_whole_group(cx: &mut TestAppContext) {
        let (workspace, cx) = named_tabs(&["a", "b", "c", "d", "e"], cx);
        workspace.update_in(cx, |this, window, cx| {
            this.toggle_pin(this.tabs[0].id, cx);
            this.new_group(this.tabs[1].id, window, cx);
            this.rename = None;
            let group = this.tabs[1].group.expect("grouped");
            this.tabs[2].group = Some(group);
        });
        settle(cx, motion::ROW_SHIFT);
        // a 0–44 (pinned), the header 44–72, then (b and c out of sight while their group
        // is dragged) d 72–116 and e 116–160. Down past "d": the group goes after it, whole.
        drag_between(cx, in_list(58.), in_list(120.));
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[
                ("a", None),
                ("d", None),
                ("b", Some("Group")),
                ("c", Some("Group")),
                ("e", None),
            ])
        );
        // Never above a pinned tab.
        settle(cx, motion::ROW_IN);
        drag_between(cx, in_list(102.), in_list(4.));
        assert_eq!(
            grouping(&workspace, cx),
            grouped(&[
                ("a", None),
                ("b", Some("Group")),
                ("c", Some("Group")),
                ("d", None),
                ("e", None),
            ])
        );
        assert_eq!(tab_layout(&workspace, cx).2, Some("e".into()));
        assert!(workspace.read_with(cx, |this, _| this.dragging.is_none()));
    }

    #[gpui::test]
    fn dragging_the_sidebar_edge_should_resize_it_within_bounds(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let width =
            |cx: &mut VisualTestContext| workspace.read_with(cx, |this, _| this.sidebar_width);
        let edge = point(px(SIDEBAR_WIDTH), px(300.));
        drag_between(cx, edge, point(px(300.), px(300.)));
        assert!((width(cx) - 300.).abs() < 1., "{}", width(cx));
        let sidebar = cx.debug_bounds("new-tab").expect("new tab button drawn");
        assert!(sidebar.right() > px(280.), "{sidebar:?}");
        drag_between(cx, point(px(300.), px(300.)), point(px(900.), px(300.)));
        assert!((width(cx) - SIDEBAR_MAX).abs() < f32::EPSILON);
        drag_between(
            cx,
            point(px(SIDEBAR_MAX), px(300.)),
            point(px(10.), px(300.)),
        );
        assert!((width(cx) - SIDEBAR_MIN).abs() < f32::EPSILON);
        // A double-click puts it back.
        click(cx, point(px(SIDEBAR_MIN), px(300.)), MouseButton::Left, 2);
        assert!((width(cx) - SIDEBAR_WIDTH).abs() < f32::EPSILON);
    }

    #[gpui::test]
    fn the_split_buttons_should_split_the_focused_pane(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let axes = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |this, _| {
                fn axes(node: &Node<Entity<TerminalPane>>, out: &mut Vec<Axis>) {
                    if let Node::Split(split) = node {
                        out.push(split.axis);
                        axes(&split.first, out);
                        axes(&split.second, out);
                    }
                }
                let mut out = Vec::new();
                axes(this.tabs[this.active].tree.root(), &mut out);
                out
            })
        };
        let right = cx.debug_bounds("split-right").expect("split right drawn");
        click(cx, right.center(), MouseButton::Left, 1);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let down = cx.debug_bounds("split-down").expect("split down drawn");
        click(cx, down.center(), MouseButton::Left, 1);
        assert_eq!(axes(cx), [Axis::Horizontal, Axis::Vertical]);
    }

    #[gpui::test]
    fn the_new_tab_button_should_open_a_tab_and_show_its_keys(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.update(|_, cx| {
            let (table, _) = crate::keymap::resolve(None);
            cx.bind_keys(crate::keymap::bindings(&table));
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert_eq!(
            cx.update(|window, _| new_tab_keys(window)),
            Some("⌘T".into())
        );
        let button = cx.debug_bounds("new-tab").expect("new tab button drawn");
        click(cx, button.center(), MouseButton::Left, 1);
        cx.run_until_parked();
        assert_eq!(workspace.read_with(cx, |this, _| this.tabs.len()), 2);
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

    fn assert_focus_ring_clears_divider(axis: Axis, cx: &mut TestAppContext) {
        let (_, cx) = workspace(cx);
        match axis {
            Axis::Horizontal => cx.dispatch_action(SplitRight),
            Axis::Vertical => cx.dispatch_action(SplitDown),
        }
        std::thread::sleep(motion::SPLIT + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        let divider = cx.debug_bounds("divider").expect("divider drawn");
        let ring = cx.debug_bounds("focus-ring").expect("focus ring drawn");
        match axis {
            Axis::Horizontal => {
                assert!(ring.left() >= divider.left() + px(4.));
                cx.dispatch_action(FocusLeft);
            }
            Axis::Vertical => {
                assert!(ring.top() >= divider.top() + px(4.));
                cx.dispatch_action(FocusUp);
            }
        }
        cx.run_until_parked();
        let ring = cx.debug_bounds("focus-ring").expect("focus ring drawn");
        match axis {
            Axis::Horizontal => assert!(ring.right() <= divider.left() + px(3.)),
            Axis::Vertical => assert!(ring.bottom() <= divider.top() + px(3.)),
        }
    }

    #[gpui::test]
    fn focused_panes_should_clear_a_vertical_divider(cx: &mut TestAppContext) {
        assert_focus_ring_clears_divider(Axis::Horizontal, cx);
    }

    #[gpui::test]
    fn focused_panes_should_clear_a_horizontal_divider(cx: &mut TestAppContext) {
        assert_focus_ring_clears_divider(Axis::Vertical, cx);
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
    fn wait_for(cx: &mut VisualTestContext, done: impl FnMut(&mut VisualTestContext) -> bool) {
        wait_for_timeout(cx, 5, done);
    }

    /// Polls `done` while letting the window draw, for up to `secs` of real time.
    fn wait_for_timeout(
        cx: &mut VisualTestContext,
        secs: u64,
        mut done: impl FnMut(&mut VisualTestContext) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(secs);
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
    fn drag_reports_should_reach_a_program_that_asks_for_them(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        // killi's fullscreen mode asks for drags (1002) in SGR (1006); each report is read up
        // to its final byte, then shown. Byte by byte with `dd`, so any login shell does it.
        cx.simulate_input(
            "printf '\\033[?1002h\\033[?1006h'; stty raw -echo; \
             r=; while [ ${#r} -lt 12 ]; do c=$(dd bs=1 count=1 2>/dev/null); r=\"$r$c\"; \
             case \"$r\" in *M|*m) break;; esac; done; echo \"PRESS:$r\" | cat -v; \
             r=; while [ ${#r} -lt 12 ]; do c=$(dd bs=1 count=1 2>/dev/null); r=\"$r$c\"; \
             case \"$r\" in *M|*m) break;; esac; done; echo \"DRAG:$r\" | cat -v; \
             r=; while [ ${#r} -lt 12 ]; do c=$(dd bs=1 count=1 2>/dev/null); r=\"$r$c\"; \
             case \"$r\" in *M|*m) break;; esac; done; echo \"UP:$r\" | cat -v\n",
        );
        wait_for(cx, |cx| {
            pane.read_with(cx, |pane, _| {
                pane.input_mode()
                    .is_some_and(kil_term::InputMode::mouse_drag)
            })
        });
        let start = point(px(SIDEBAR_WIDTH + 200.), px(300.));
        let moved = start + point(px(0.), px(60.));
        cx.simulate_event(MouseDownEvent {
            position: start,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        });
        wait_for(cx, |cx| {
            pane.read_with(cx, |pane, _| pane.screen_text().contains("PRESS:^[[<0;"))
        });
        cx.simulate_event(gpui::MouseMoveEvent {
            position: moved,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        wait_for(cx, |cx| {
            pane.read_with(cx, |pane, _| pane.screen_text().contains("DRAG:^[[<32;"))
        });
        cx.simulate_event(MouseUpEvent {
            position: moved,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        wait_for(cx, |cx| {
            pane.read_with(cx, |pane, _| pane.screen_text().contains("UP:^[[<0;"))
        });
        let screen = pane.read_with(cx, |pane, _| pane.screen_text());
        let up = screen
            .lines()
            .find(|line| line.contains("UP:^[[<0;"))
            .unwrap_or("");
        // The shell's `%` marks the missing newline; the report itself ends in lowercase m.
        let report = up.split("UP:").nth(1).unwrap_or("");
        assert!(
            report.starts_with("^[[<0;") && report.contains('m'),
            "the release names button 0 and ends in lowercase m: {screen}"
        );
    }

    #[gpui::test]
    fn shift_drag_should_keep_kils_selection_under_1002(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        cx.simulate_input(
            "printf '\\033[?1002h\\033[?1006h'; stty raw -echo; \
             r=; while [ ${#r} -lt 12 ]; do c=$(dd bs=1 count=1 2>/dev/null); r=\"$r$c\"; \
             case \"$r\" in *M|*m) break;; esac; done; echo \"GOT:$r\" | cat -v\n",
        );
        wait_for(cx, |cx| {
            pane.read_with(cx, |pane, _| {
                pane.input_mode()
                    .is_some_and(kil_term::InputMode::mouse_drag)
            })
        });
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let start = point(px(SIDEBAR_WIDTH + 200.), px(300.));
        let moved = start + point(px(0.), px(60.));
        cx.simulate_event(MouseDownEvent {
            position: start,
            button: MouseButton::Left,
            modifiers: shift,
            click_count: 1,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseMoveEvent {
            position: moved,
            pressed_button: Some(MouseButton::Left),
            modifiers: shift,
        });
        cx.simulate_event(MouseUpEvent {
            position: moved,
            button: MouseButton::Left,
            modifiers: shift,
            click_count: 1,
        });
        // No button bytes may have leaked: the next bytes the shell reads are a wheel notch's.
        cx.simulate_event(ScrollWheelEvent {
            position: moved,
            delta: ScrollDelta::Lines(point(0., 1.)),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        wait_for(cx, |cx| {
            pane.read_with(cx, |pane, _| pane.screen_text().contains("GOT:^[[<"))
        });
        let screen = pane.read_with(cx, |pane, _| pane.screen_text());
        assert!(
            screen.contains("GOT:^[[<64;"),
            "Shift keeps the native selection, the wheel still reports: {screen}"
        );
    }

    #[gpui::test]
    fn command_click_should_open_the_link_under_the_pointer(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        // The row holding exactly `line`: the shell echoes the command too, on its own row.
        let find_row = |cx: &mut VisualTestContext, line: &str| {
            pane.read_with(cx, |pane, _| {
                (0..pane.frame.rows).find(|&row| pane.frame.row_text(row) == line)
            })
        };
        let click = |cx: &mut VisualTestContext, row: usize, col: usize, modifiers: Modifiers| {
            let position = pane.read_with(cx, |pane, _| {
                let geometry = pane.geometry.expect("laid out");
                point(
                    geometry.origin.x + geometry.cell.width * col + geometry.cell.width * 0.5,
                    geometry.origin.y + geometry.cell.height * row + geometry.cell.height * 0.5,
                )
            });
            cx.simulate_event(MouseDownEvent {
                position,
                button: MouseButton::Left,
                modifiers,
                click_count: 1,
                first_mouse: false,
            });
            cx.simulate_event(MouseUpEvent {
                position,
                button: MouseButton::Left,
                modifiers,
                click_count: 1,
            });
            cx.run_until_parked();
        };
        let command = Modifiers {
            platform: true,
            ..Default::default()
        };

        // Typed before the session attaches, input is dropped: wait for the shell first.
        wait_for(cx, |cx| pane.read_with(cx, |pane, _| pane.pid().is_some()));
        assert!(pane.read_with(cx, |pane, _| pane.pid().is_some()));
        // The output must arrive and the pane must redraw from it; under a full run's
        // load each can lag, so this waits longer than `wait_for`. It only reads the
        // drawn frame: snapshotting the session would clear its dirty flag, which is
        // what tells the next draw to copy the output.
        let wait_for_line = |cx: &mut VisualTestContext, line: &str| {
            wait_for_timeout(cx, 30, |cx| find_row(cx, line).is_some());
            find_row(cx, line).expect("echoed back")
        };

        cx.simulate_input("echo https://example.com/docs\n");
        let row = wait_for_line(cx, "https://example.com/docs");
        // A plain click selects; only the modified one opens.
        click(cx, row, 10, Modifiers::none());
        assert_eq!(cx.opened_url(), None);
        click(cx, row, 10, command);
        assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/docs"));

        cx.simulate_input("echo '[the guide](https://example.org/g)'\n");
        let row = wait_for_line(cx, "[the guide](https://example.org/g)");
        // On the link's text, not its target.
        click(cx, row, 5, command);
        assert_eq!(cx.opened_url().as_deref(), Some("https://example.org/g"));

        cx.simulate_input(
            "printf '\\033]8;;https://example.net\\033\\\\linked\\033]8;;\\033\\\\\\n'\n",
        );
        let row = wait_for_line(cx, "linked");
        click(cx, row, 2, command);
        assert_eq!(cx.opened_url().as_deref(), Some("https://example.net"));
    }

    #[gpui::test]
    fn a_pane_split_off_below_should_start_its_prompt_at_the_top(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(SplitDown);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        let settled = |cx: &mut VisualTestContext| {
            let animating = workspace.read_with(cx, |this, _| this.tabs[0].animating);
            let cursor = pane.read_with(cx, |pane, _| pane.frame.cursor);
            !animating
                && pane.read_with(cx, |pane, _| !pane.screen_text().trim().is_empty())
                && cursor.is_some()
        };
        wait_for(cx, settled);
        // The shell's first output, once the split has settled.
        std::thread::sleep(Duration::from_millis(300));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let (rows, cursor) = pane.read_with(cx, |pane, _| (pane.frame.rows, pane.frame.cursor));
        let row = cursor.expect("cursor in view").row;
        assert!(rows > 10, "{rows} rows");
        assert!(row < 4, "prompt on row {row} of {rows}");
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

    /// Runs one presence poll, as the watchers timer does.
    fn poll_watchers(
        workspace: &Entity<Workspace>,
        client: &Arc<Client>,
        cx: &mut VisualTestContext,
    ) {
        workspace.update(cx, |this, cx| {
            this.apply_watchers(client.watchers(), client.controlling(), cx);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_phone_on_a_session_should_mark_its_tab_row_until_it_leaves(cx: &mut TestAppContext) {
        let client = test_client();
        let (workspace, cx) = workspace_on(Arc::clone(&client), None, cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        let id = pane_id(&pane, cx);
        // The pane's session starts on a background thread.
        for _ in 0..500 {
            if client.sessions().is_ok_and(|ids| ids.contains(&id)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(client.sessions().is_ok_and(|ids| ids.contains(&id)));
        poll_watchers(&workspace, &client, cx);
        assert!(cx.debug_bounds("watched-tab").is_none());
        assert!(cx.debug_bounds("watched-row").is_none());

        // A phone attaches to the session: the next poll rings its row and badges it.
        let phone = client.attach(&id, 1_000, |_| {}).unwrap().unwrap();
        for _ in 0..500 {
            if client
                .watchers()
                .is_ok_and(|watched| watched == [id.as_str()])
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        poll_watchers(&workspace, &client, cx);
        let (ring, badge) = (
            cx.debug_bounds("watched-row").unwrap(),
            cx.debug_bounds("watched-tab").unwrap(),
        );
        // The ring outlines the whole row, and the badge sits on it.
        assert_eq!(ring.size.height, px(ROW_HEIGHT));
        assert!(ring.left() <= badge.left() && badge.right() <= ring.right());
        assert!(ring.top() <= badge.top() && badge.bottom() <= ring.bottom());
        // Attached without typing: watching only, never driving.
        assert!(cx.debug_bounds("controlling-tab").is_none());
        assert!(cx.debug_bounds("controlling-row").is_none());

        // It detaches: the next poll clears the state, and the marks follow it. Debug
        // bounds accumulate across frames, so absence is read from the state; the initial
        // `is_none` above proves the marks only draw while it is set.
        drop(phone);
        for _ in 0..500 {
            if client.watchers().is_ok_and(|watched| watched.is_empty()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        poll_watchers(&workspace, &client, cx);
        assert!(!pane.read_with(cx, |pane, _| pane.watched()));

        // A second visit marks them again: the binding is live, not stuck.
        let phone = client.attach(&id, 1_000, |_| {}).unwrap().unwrap();
        for _ in 0..500 {
            if client
                .watchers()
                .is_ok_and(|watched| watched == [id.as_str()])
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        poll_watchers(&workspace, &client, cx);
        assert!(pane.read_with(cx, |pane, _| pane.watched()));
        assert!(cx.debug_bounds("watched-tab").is_some());
        assert!(cx.debug_bounds("watched-row").is_some());
        drop(phone);
    }

    #[gpui::test]
    fn a_phone_typing_should_mark_its_tab_row_driven(cx: &mut TestAppContext) {
        use std::io::{Read as _, Write as _};

        // A server of this test's own, with its directory for a raw phone.
        let dir = std::env::temp_dir().join(format!("kil-wst-{}-drives", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        kil_term::server::serve_in_thread(&dir).unwrap();
        let client = Arc::new(Client::connect(&dir).unwrap());
        let (workspace, cx) = workspace_on(Arc::clone(&client), None, cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        let id = pane_id(&pane, cx);
        for _ in 0..500 {
            if client.sessions().is_ok_and(|ids| ids.contains(&id)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(client.sessions().is_ok_and(|ids| ids.contains(&id)));

        // The phone declares itself remote before its hello, as the bridge sends it.
        let frame = |kind: u8, payload: &[u8]| {
            let mut out = u32::try_from(payload.len() + 1)
                .unwrap()
                .to_le_bytes()
                .to_vec();
            out.push(kind);
            out.extend_from_slice(payload);
            out
        };
        let mut phone =
            std::os::unix::net::UnixStream::connect(kil_term::server::socket_path(&dir)).unwrap();
        phone
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        phone.write_all(&frame(19, &[])).unwrap();
        phone.write_all(&frame(1, &2_u32.to_le_bytes())).unwrap();
        let mut attach = u32::try_from(id.len()).unwrap().to_le_bytes().to_vec();
        attach.extend_from_slice(id.as_bytes());
        phone.write_all(&frame(5, &attach)).unwrap();
        // Takes the server's output, or it is dropped as stalled.
        let drain = |phone: &mut std::os::unix::net::UnixStream| {
            let mut buf = [0u8; 8192];
            loop {
                match phone.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        };
        drain(&mut phone);

        // Attached but idle: the watching ring and badge, not the driving ones.
        for _ in 0..500 {
            if client
                .watchers()
                .is_ok_and(|watched| watched == [id.as_str()])
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        poll_watchers(&workspace, &client, cx);
        assert!(cx.debug_bounds("watched-tab").is_some());
        assert!(cx.debug_bounds("watched-row").is_some());
        assert!(cx.debug_bounds("controlling-tab").is_none());
        assert!(cx.debug_bounds("controlling-row").is_none());

        // A keystroke drives the session: the stronger ring and badge win.
        phone.write_all(&frame(6, b"x")).unwrap();
        drain(&mut phone);
        for _ in 0..500 {
            if client
                .controlling()
                .is_ok_and(|controlling| controlling == [id.as_str()])
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        poll_watchers(&workspace, &client, cx);
        assert!(pane.read_with(cx, |pane, _| pane.controlled()));
        assert!(cx.debug_bounds("controlling-tab").is_some());
        assert!(cx.debug_bounds("controlling-row").is_some());

        // It detaches: the state clears, and the marks follow it.
        drop(phone);
        for _ in 0..500 {
            if client.watchers().is_ok_and(|watched| watched.is_empty()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        poll_watchers(&workspace, &client, cx);
        assert!(!pane.read_with(cx, |pane, _| pane.controlled()));
        assert!(!pane.read_with(cx, |pane, _| pane.watched()));
    }

    fn notification() -> kil_term::Notification {
        kil_term::Notification {
            title: "killi".into(),
            body: "Done".into(),
        }
    }

    fn pane_id(pane: &Entity<TerminalPane>, cx: &mut VisualTestContext) -> String {
        pane.read_with(cx, |pane, _| pane.id().to_owned())
    }

    /// The workspace following clicks on notifications, which `clicks` sends.
    fn following_clicks(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> mpsc::UnboundedSender<String> {
        let (clicks, heard) = mpsc::unbounded();
        workspace.update_in(cx, |_, window, cx| {
            Workspace::go_to_clicked_notifications(heard, window, cx);
        });
        clicks
    }

    #[gpui::test]
    fn a_notification_from_a_background_tab_marks_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(NewTab);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        cx.deactivate_window();
        pane.update(cx, |_, cx| cx.emit(PaneEvent::Notified(notification())));
        cx.run_until_parked();
        assert_eq!(first_status(&workspace, cx), TabStatus::Done);
        assert_eq!(workspace.read_with(cx, |this, _| this.active), 1);
    }

    #[gpui::test]
    fn a_click_on_a_notification_goes_to_the_pane_that_posted_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(NewTab);
        cx.dispatch_action(NewTab);
        let panes: Vec<_> = workspace.read_with(cx, |this, _| {
            this.tabs.iter().map(|tab| tab.focused.clone()).collect()
        });
        let clicks = following_clicks(&workspace, cx);
        assert_eq!(workspace.read_with(cx, |this, _| this.active), 2);

        // Not the notification posted last: each click goes to its own pane.
        for index in [0, 1, 0] {
            clicks.unbounded_send(pane_id(&panes[index], cx)).unwrap();
            cx.run_until_parked();
            assert_eq!(workspace.read_with(cx, |this, _| this.active), index);
        }
    }

    #[gpui::test]
    fn a_click_on_a_notification_focuses_its_pane_among_a_tab_s_split_panes(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(SplitRight);
        let posted = workspace.read_with(cx, |this, _| this.tabs[0].tree.leaves()[0].clone());
        assert!(workspace.read_with(cx, |this, _| this.tabs[0].focused != posted));
        cx.dispatch_action(NewTab);
        let clicks = following_clicks(&workspace, cx);

        clicks.unbounded_send(pane_id(&posted, cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(workspace.read_with(cx, |this, _| this.active), 0);
        assert!(workspace.read_with(cx, |this, _| this.tabs[0].focused == posted));
        cx.update(|window, cx| assert!(posted.focus_handle(cx).is_focused(window)));
    }

    #[gpui::test]
    fn a_click_on_a_notification_of_a_pane_no_longer_there_stays_put(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(NewTab);
        let clicks = following_clicks(&workspace, cx);

        clicks.unbounded_send("closed-since".into()).unwrap();
        cx.run_until_parked();
        assert_eq!(workspace.read_with(cx, |this, _| this.active), 1);
    }

    #[gpui::test]
    fn a_notification_from_the_tab_in_view_is_not_shown(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.update(|window, _| window.activate_window());
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        pane.update(cx, |_, cx| cx.emit(PaneEvent::Notified(notification())));
        cx.run_until_parked();
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
                    pinned: false,
                    group: None,
                    root: shell_in("/", None),
                },
                SavedTab {
                    name: Some("构建".to_owned()),
                    focused: 2,
                    pinned: true,
                    group: None,
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
            ..SavedWorkspace::default()
        };
        let store = Store::at(scratch_dir("reopen"));
        store.save(store.ticket(), &saved, &[]).unwrap();
        let (workspace, cx) = workspace_saving_to(Some(store), cx);
        workspace.read_with(cx, |this, _| {
            let tab = &this.tabs[0];
            let leaves = tab.tree.leaves();
            assert_eq!((this.tabs.len(), this.active), (2, 0));
            assert_eq!(tab.name, Some("构建".into()));
            assert!(tab.pinned);
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
        workspace.update(cx, |this, _| {
            this.tabs[0].name = Some("dev".into());
            this.tabs[0].pinned = true;
        });
        cx.dispatch_action(Quit);
        cx.run_until_parked();
        let saved = saved_layout(&dir);
        assert_eq!(saved.tabs.len(), 1);
        assert_eq!(saved.tabs[0].name.as_deref(), Some("dev"));
        assert!(saved.tabs[0].pinned);
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
    fn groups_and_the_sidebar_width_should_be_saved_for_the_next_launch(cx: &mut TestAppContext) {
        let dir = scratch_dir("groups");
        let (workspace, cx) = workspace_saving_to(Some(Store::at(dir.clone())), cx);
        cx.dispatch_action(NewTab);
        cx.dispatch_action(NewTab);
        workspace.update_in(cx, |this, window, cx| {
            this.new_group(this.tabs[1].id, window, cx);
            this.rename = None;
            let group = this.tabs[1].group.expect("grouped");
            this.tabs[2].group = Some(group);
            if let Some(group) = this.group_mut(group) {
                group.name = "服务".into();
                group.collapsed = true;
            }
            this.set_sidebar_width(280., cx);
            // Out of the name field, back to the terminal.
            this.focus_active(window, cx);
        });
        cx.dispatch_action(Quit);
        cx.run_until_parked();
        let saved = saved_layout(&dir);
        assert_eq!(
            saved.groups,
            vec![SavedGroup {
                name: "服务".to_owned(),
                collapsed: true,
            }]
        );
        let groups: Vec<_> = saved.tabs.iter().map(|tab| tab.group).collect();
        assert_eq!(groups, vec![None, Some(0), Some(0)]);
        assert_eq!(saved.sidebar_width, Some(280.));
    }

    #[gpui::test]
    fn launch_should_reopen_groups_folded_and_in_one_piece(cx: &mut TestAppContext) {
        let tab = |group: Option<usize>, pinned: bool| SavedTab {
            name: None,
            focused: 0,
            pinned,
            group,
            root: shell_in("/", None),
        };
        // A hand-edited save: the group split by an ungrouped tab, a pinned tab claiming a
        // group, a group named by no one, and a width past the limit.
        let saved = SavedWorkspace {
            active: 0,
            sidebar_width: Some(9000.),
            groups: vec![
                SavedGroup {
                    name: "  ".to_owned(),
                    collapsed: true,
                },
                SavedGroup::default(),
            ],
            tabs: vec![
                tab(Some(0), false),
                tab(None, false),
                tab(Some(0), false),
                tab(Some(1), true),
            ],
            ..SavedWorkspace::default()
        };
        let store = Store::at(scratch_dir("reopen-groups"));
        store.save(store.ticket(), &saved, &[]).unwrap();
        let (workspace, cx) = workspace_saving_to(Some(store), cx);
        workspace.read_with(cx, |this, _| {
            let rows: Vec<_> = this
                .tabs
                .iter()
                .map(|tab| (tab.pinned, tab.group.is_some()))
                .collect();
            assert_eq!(
                rows,
                vec![(true, false), (false, true), (false, true), (false, false)]
            );
            assert_eq!(this.groups.len(), 1);
            assert_eq!(this.groups[0].name, SharedString::from(GROUP_NAME));
            assert!(this.groups[0].collapsed);
            assert!((this.sidebar_width - SIDEBAR_MAX).abs() < f32::EPSILON);
            // The active tab shows under its folded header; the other one is folded away.
            let active = this.tabs[this.active].group;
            assert!(active.is_some());
            let shown: Vec<_> = this
                .tabs
                .iter()
                .map(|tab| tab.reveal.target() > 0.)
                .collect();
            assert_eq!(shown, vec![true, true, false, true]);
        });
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

    /// Restart to Update is a global action: from the terminal window's own menu it arrives
    /// inside that window's update, where the window can't be updated again.
    #[gpui::test]
    fn restart_to_update_should_quit_from_the_terminal_windows_own_menu(cx: &mut TestAppContext) {
        let dir = scratch_dir("restart");
        let (workspace, cx) = workspace_saving_to(Some(Store::at(dir.clone())), cx);
        cx.update(|_, cx| {
            cx.on_action(|_: &crate::update::RestartToUpdate, cx| detach_and_quit(cx));
        });
        workspace.update(cx, |this, _| this.tabs[0].name = Some("dev".into()));
        cx.dispatch_action(crate::update::RestartToUpdate);
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |this, _| this.ended));
        assert_eq!(saved_layout(&dir).tabs[0].name.as_deref(), Some("dev"));
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
    fn restart_should_stop_the_jobs_and_save_the_layout(cx: &mut TestAppContext) {
        let client = test_client();
        let dir = scratch_dir("restart-stops-jobs");
        let (workspace, cx) = workspace_on(Arc::clone(&client), Some(Store::at(dir.clone())), cx);
        let (_id, pid) = started(&workspace, cx);
        workspace.update(cx, |this, _| this.tabs[0].name = Some("dev".into()));
        cx.dispatch_action(Restart);
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |this, _| this.ended));
        assert_eq!(client.sessions().unwrap(), Vec::<String>::new());
        assert!(!kil_term::diagnostics::exists(
            kil_term::Pid::new(pid).unwrap()
        ));
        assert_eq!(saved_layout(&dir).tabs[0].name.as_deref(), Some("dev"));
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

    fn cell_center(
        pane: &Entity<TerminalPane>,
        cx: &mut VisualTestContext,
        row: usize,
        col: usize,
    ) -> Point<Pixels> {
        pane.read_with(cx, |pane, _| {
            let geometry = pane.geometry.expect("laid out");
            point(
                geometry.origin.x + geometry.cell.width * col + geometry.cell.width * 0.5,
                geometry.origin.y + geometry.cell.height * row + geometry.cell.height * 0.5,
            )
        })
    }

    fn wait_for_row(cx: &mut VisualTestContext, pane: &Entity<TerminalPane>, line: &str) -> usize {
        wait_for_timeout(cx, 15, |cx| {
            pane.read_with(cx, |pane, _| {
                (0..pane.frame.rows).any(|row| pane.frame.row_text(row) == line)
            })
        });
        pane.read_with(cx, |pane, _| {
            (0..pane.frame.rows)
                .find(|&row| pane.frame.row_text(row) == line)
                .expect("echoed back")
        })
    }

    #[gpui::test]
    fn scroll_should_keep_bottom_tab_visible_when_selecting_below_fold(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        // Thirty rows overflow the ~22-row list; they share one pane, the rows only
        // read its title.
        workspace.update(cx, |this, cx| {
            let pane = this.tabs[0].focused.clone();
            for n in 1..30u64 {
                this.tabs.push(Tab {
                    name: Some(format!("t{n}").into()),
                    ..Tab::new(
                        TabId(n),
                        crate::pane_tree::PaneTree::new(pane.clone()),
                        pane.clone(),
                    )
                });
            }
            this.next_tab = 30;
            cx.notify();
        });
        // Rows grow in over `ROW_IN`; scroll from full height.
        std::thread::sleep(motion::ROW_IN + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        workspace.update_in(cx, |this, window, cx| this.activate(29, window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        workspace.read_with(cx, |this, _| {
            let index = this.active_child_index();
            assert_eq!(index, 29);
            let item = this.tabs_scroll.bounds_for_item(index).expect("laid out");
            let offset = this.tabs_scroll.offset();
            let viewport = this.tabs_scroll.bounds();
            let top = item.top() + offset.y;
            let bottom = item.bottom() + offset.y;
            assert!(top >= viewport.top(), "{top:?} above {viewport:?}");
            assert!(bottom <= viewport.bottom(), "{bottom:?} below {viewport:?}");
            assert!(
                top > viewport.top() + px(1.),
                "yanked to top: {top:?} in {viewport:?}"
            );
        });
        // Ordinary renders don't rescroll.
        let before = workspace.read_with(cx, |this, _| this.tabs_scroll.offset());
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert_eq!(
            before,
            workspace.read_with(cx, |this, _| this.tabs_scroll.offset())
        );
    }

    #[gpui::test]
    fn selection_should_extend_when_dragging_outside_pane(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        wait_for(cx, |cx| pane.read_with(cx, |pane, _| pane.pid().is_some()));
        cx.simulate_input("echo hello-selection\n");
        let row = wait_for_row(cx, &pane, "hello-selection");
        let start = cell_center(&pane, cx, row, 1);
        cx.simulate_event(MouseDownEvent {
            position: start,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.selecting));
        assert_eq!(pane.read_with(cx, |pane, _| pane.selection_text()), None);
        // Over the sidebar: outside the pane, inside the window. The pane's own
        // move handler never fires there; only the window capture path extends.
        // Re-sent until it lands: the one-frame capture listener may miss the
        // first move under load.
        let outside = point(px(100.), px(300.));
        wait_for(cx, |cx| {
            cx.simulate_event(gpui::MouseMoveEvent {
                position: outside,
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers::none(),
            });
            pane.read_with(cx, |pane, _| pane.selection_text().is_some())
        });
        assert!(
            pane.read_with(cx, |pane, _| pane.selection_text())
                .is_some(),
            "capture drag extends the selection"
        );
        cx.simulate_event(MouseUpEvent {
            position: outside,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
    }

    #[gpui::test]
    fn shift_click_should_extend_selection_when_anchor_exists(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        wait_for(cx, |cx| pane.read_with(cx, |pane, _| pane.pid().is_some()));
        cx.simulate_input("echo hello-selection\n");
        let row = wait_for_row(cx, &pane, "hello-selection");
        let start = cell_center(&pane, cx, row, 1);
        let end = cell_center(&pane, cx, row, 5);
        cx.simulate_event(MouseDownEvent {
            position: start,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseMoveEvent {
            position: end,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        cx.simulate_event(MouseUpEvent {
            position: end,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        cx.run_until_parked();
        let before = pane.read_with(cx, |pane, _| pane.selection_text());
        assert!(before.is_some(), "drag selects");
        let before_len = before.map_or(0, |text| text.len());
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let further = cell_center(&pane, cx, row, 10);
        cx.simulate_event(MouseDownEvent {
            position: further,
            button: MouseButton::Left,
            modifiers: shift,
            click_count: 1,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position: further,
            button: MouseButton::Left,
            modifiers: shift,
            click_count: 1,
        });
        cx.run_until_parked();
        let after = pane.read_with(cx, |pane, _| pane.selection_text());
        assert!(after.is_some(), "shift-click keeps the selection");
        assert!(
            after.map_or(0, |text| text.len()) > before_len,
            "shift-click extends from the anchor"
        );
    }

    #[gpui::test]
    fn right_click_should_focus_pane_when_in_split(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.dispatch_action(SplitRight);
        std::thread::sleep(motion::SPLIT + Duration::from_millis(20));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let leaves: Vec<_> = workspace.read_with(cx, |this, _| {
            this.tabs[0].tree.leaves().into_iter().cloned().collect()
        });
        assert_eq!(leaves.len(), 2);
        assert!(workspace.read_with(cx, |this, _| this.tabs[0].focused == leaves[1]));
        cx.simulate_event(MouseDownEvent {
            position: point(px(SIDEBAR_WIDTH + 100.), px(300.)),
            button: MouseButton::Right,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position: point(px(SIDEBAR_WIDTH + 100.), px(300.)),
            button: MouseButton::Right,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |this, _| this.tabs[0].focused == leaves[0]));
        cx.update(|window, cx| assert!(leaves[0].focus_handle(cx).is_focused(window)));
    }

    #[gpui::test]
    fn forwarding_should_clear_when_window_blurs(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let pane = workspace.read_with(cx, |this, _| this.tabs[0].focused.clone());
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        wait_for(cx, |cx| pane.read_with(cx, |pane, _| pane.pid().is_some()));
        cx.simulate_input("printf '\\033[?1002h\\033[?1006h'\n");
        wait_for_timeout(cx, 15, |cx| {
            pane.read_with(cx, |pane, _| {
                pane.input_mode()
                    .is_some_and(kil_term::InputMode::mouse_drag)
            })
        });
        assert!(
            pane.read_with(cx, |pane, _| pane
                .input_mode()
                .is_some_and(kil_term::InputMode::mouse_drag)),
            "1002 enabled"
        );
        let at = point(px(SIDEBAR_WIDTH + 200.), px(300.));
        cx.simulate_event(MouseDownEvent {
            position: at,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.forwarding));
        assert!(pane.read_with(cx, |pane, _| pane.forwarding_guarded));
        cx.deactivate_window();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(!pane.read_with(cx, |pane, _| pane.forwarding));
    }
}
