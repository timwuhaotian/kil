//! Kil ▸ Settings…: a separate window that edits `config.toml` one key at a time
//! (DESIGN.md §9). Each change is written, then applied by the same reload that runs when
//! the terminal window regains focus.

use std::fs;

use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, ClickEvent, Context, Div, ElementId, Entity,
    FocusHandle, Focusable, Hsla, KeyBinding, Keystroke, MouseButton, MouseDownEvent,
    PathPromptOptions, SharedString, Stateful, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowOptions, actions, div, point, prelude::*, px, size,
};

use crate::{
    config,
    keymap::{self, Command},
    settings::{Settings, abbreviate_home, config_dir, expand_home},
    text_field::{FieldEvent, TextField},
    theme::{Theme, ThemeName, hsla},
    workspace,
};

actions!(settings_window, [CloseWindow]);

const TITLEBAR: f32 = 40.;

pub fn bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("cmd-w", CloseWindow, Some("Settings"))]
}

/// Opens the window, or brings the open one forward.
pub fn open(cx: &mut App) {
    if let Some(open) = cx
        .windows()
        .iter()
        .find_map(AnyWindowHandle::downcast::<SettingsWindow>)
    {
        let _ = open.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(460.), px(700.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(14.), px(13.))),
        }),
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            let this = SettingsWindow::new(window, cx);
            window.focus(&this.focus_handle);
            this
        })
    });
    if let Err(err) = opened {
        log::error!("failed to open settings: {err:#}");
    }
}

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    /// What `config.toml` holds, not the live font zoom.
    file: Settings,
    font_family: Entity<TextField>,
    scrollback: Entity<TextField>,
    directory: Entity<TextField>,
    /// `keymap.toml` resolved over the defaults: which keys run which command.
    keys: Vec<(String, Command)>,
    /// The command whose next keypress is being recorded.
    recording: Option<Command>,
    /// Swallows every keypress while recording, so `cmd-w` is read and not acted on.
    recorder: Option<Subscription>,
    /// Last problem reading or writing the file.
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (file, error) = match config::read_settings() {
            Ok(file) => (file, None),
            Err(err) => (cx.global::<Settings>().clone(), Some(err.into())),
        };
        let font_family = cx.new(|cx| TextField::new(&file.font_family, window, cx));
        let scrollback =
            cx.new(|cx| TextField::new(&file.scrollback_lines.to_string(), window, cx));
        let directory = cx.new(|cx| TextField::new(&shown_directory(&file), window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&font_family, window, Self::font_family_edited),
            cx.subscribe_in(&scrollback, window, Self::scrollback_edited),
            cx.subscribe_in(&directory, window, Self::directory_edited),
        ];
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            file,
            font_family,
            scrollback,
            directory,
            keys: Vec::new(),
            recording: None,
            recorder: None,
            error,
            _subscriptions: subscriptions,
        };
        this.load_keys();
        this
    }

    fn write(&mut self, key: &str, value: &toml::Value, cx: &mut Context<Self>) {
        self.applied(config::write_setting(key, value), cx);
    }

    fn applied(&mut self, written: Result<Settings, String>, cx: &mut Context<Self>) {
        match written {
            Ok(file) => {
                self.file = file;
                self.error = None;
                workspace::reload_config(cx);
            }
            Err(err) => self.error = Some(err.into()),
        }
        cx.notify();
    }

    fn font_family_edited(
        &mut self,
        field: &Entity<TextField>,
        event: &FieldEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let FieldEvent::Confirmed(text) = event else {
            return;
        };
        match text.trim() {
            "" => self.error = Some("Font family can't be empty.".into()),
            family => self.write("font_family", &family.into(), cx),
        }
        let shown = self.file.font_family.clone();
        field.update(cx, |field, cx| field.set_text(&shown, cx));
    }

    fn scrollback_edited(
        &mut self,
        field: &Entity<TextField>,
        event: &FieldEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let FieldEvent::Confirmed(text) = event else {
            return;
        };
        match text.trim().parse::<u32>() {
            Ok(lines) => self.write("scrollback_lines", &i64::from(lines).into(), cx),
            Err(_) => self.error = Some("Scrollback is a whole number of lines.".into()),
        }
        let shown = self.file.scrollback_lines.to_string();
        field.update(cx, |field, cx| field.set_text(&shown, cx));
    }

    /// Saves `dir` as where new tabs open; `None` clears it.
    fn set_directory(&mut self, dir: Option<&std::path::Path>, cx: &mut Context<Self>) {
        match dir {
            None => self.write("default_directory", &"".into(), cx),
            Some(dir) if dir.is_dir() => {
                self.write("default_directory", &abbreviate_home(dir).into(), cx);
            }
            Some(dir) => self.error = Some(format!("No such folder: {}", dir.display()).into()),
        }
        let shown = shown_directory(&self.file);
        self.directory
            .update(cx, |field, cx| field.set_text(&shown, cx));
    }

    fn directory_edited(
        &mut self,
        _: &Entity<TextField>,
        event: &FieldEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let FieldEvent::Confirmed(text) = event {
            self.set_directory(expand_home(text).as_deref(), cx);
        }
    }

    fn choose_directory(cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await
                && let Some(dir) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.set_directory(Some(&dir), cx));
            }
        })
        .detach();
    }

    /// The keys `command` runs on now, resolved from `keymap.toml` over the defaults.
    fn keys_for(&self, command: Command) -> Option<&str> {
        self.keys
            .iter()
            .find(|(_, bound)| *bound == command)
            .map(|(keys, _)| keys.as_str())
    }

    /// `keymap.toml` resolved over the defaults.
    fn load_keys(&mut self) {
        match config::read_keymap() {
            Ok(text) => self.keys = keymap::resolve(text.as_deref()).0,
            Err(err) => self.error = Some(err.into()),
        }
    }

    /// Rewrites `keymap.toml` with `edit`, then shows and applies what it now holds.
    fn write_keymap(&mut self, edit: impl FnOnce(&str) -> String, cx: &mut Context<Self>) {
        match config::update_keymap(edit) {
            Ok(()) => {
                self.error = None;
                self.load_keys();
                workspace::reload_config(cx);
            }
            Err(err) => self.error = Some(err.into()),
        }
        cx.notify();
    }

    /// Waits for a keypress and saves it as `command`'s shortcut. Keys are read, not acted
    /// on, so `cmd-w` can be recorded without closing the window. Clicking the row that is
    /// already listening puts it back.
    fn record(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.recording == Some(command) {
            self.recording = None;
            self.recorder = None;
            cx.notify();
            return;
        }
        self.recording = Some(command);
        self.recorder = Some(cx.intercept_keystrokes(|event, window, cx| {
            if let Some(handle) = window.window_handle().downcast::<SettingsWindow>() {
                let keystroke = event.keystroke.clone();
                let _ = handle.update(cx, |this, _, cx| this.recorded(&keystroke, cx));
            }
            cx.stop_propagation();
        }));
        cx.notify();
    }

    /// A key arrived while recording.
    fn recorded(&mut self, event: &Keystroke, cx: &mut Context<Self>) {
        let Some(command) = self.recording else {
            return;
        };
        // A modifier on its own arrives as a keystroke of its own; the shortcut is the key
        // pressed with it.
        let Some(keys) = keymap::keystroke_of(event) else {
            return;
        };
        self.recording = None;
        self.recorder = None;
        let taken = self
            .keys
            .iter()
            .find(|(bound, other)| bound == &keys && *other != command)
            .map(|(_, other)| *other);
        match taken {
            Some(taken) => {
                self.error = Some(
                    format!("{} is already {}.", keymap::display(&keys), taken.title()).into(),
                );
                cx.notify();
            }
            None => self.write_keymap(|text| keymap::rebind(text, command, &keys), cx),
        }
    }

    /// Drops the user's shortcut, so the default runs again.
    fn reset_key(&mut self, command: Command, cx: &mut Context<Self>) {
        self.recording = None;
        self.recorder = None;
        self.write_keymap(|text| keymap::clear_command(text, command), cx);
    }

    fn step_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let size = (f32::from(self.file.font_size) + delta).clamp(6., 72.);
        self.write("font_size", &f64::from(size).into(), cx);
    }

    fn step_sidebar_opacity(&mut self, up: bool, cx: &mut Context<Self>) {
        let opacity = step_percent(sidebar_opacity(&self.file), up);
        self.write("sidebar_opacity", &f64::from(opacity).into(), cx);
    }

    fn unset(&mut self, key: &str, cx: &mut Context<Self>) {
        self.applied(config::unset_setting(key), cx);
    }

    fn open_folder(&mut self, cx: &mut Context<Self>) {
        match config_dir() {
            None => self.error = Some("No home directory.".into()),
            Some(dir) => match fs::create_dir_all(&dir) {
                Ok(()) => cx.open_with_system(&dir),
                Err(err) => self.error = Some(format!("{}: {err}", dir.display()).into()),
            },
        }
        cx.notify();
    }
}

/// The sidebar opacity in effect: the file's, else the theme's.
fn sidebar_opacity(file: &Settings) -> f32 {
    file.sidebar_opacity
        .unwrap_or_else(|| Theme::named(file.theme).sidebar_bg.a)
}

/// `opacity` moved one 5 % step up or down to a multiple of 5 %, within 0–1: the themes'
/// own 72 % steps to 75 % or 70 %, not 77 % or 67 %.
fn step_percent(opacity: f32, up: bool) -> f32 {
    let percent = (opacity * 100.).round();
    let stepped = if up {
        (percent / 5.).floor() * 5. + 5.
    } else {
        (percent / 5.).ceil() * 5. - 5.
    };
    (stepped / 100.).clamp(0., 1.)
}

/// The default directory as typed in the field: `~` for home, empty for none.
fn shown_directory(file: &Settings) -> String {
    file.default_directory
        .as_deref()
        .map(abbreviate_home)
        .unwrap_or_default()
}

/// A theme's card: its background, its accent and a run of its ANSI colors.
fn swatch(name: ThemeName, chosen: bool, c: Colors) -> Stateful<Div> {
    let theme = Theme::named(name);
    let strip = [1, 2, 3, 4, 5, 6].map(|index| {
        div()
            .size(px(7.))
            .rounded_full()
            .bg(hsla(theme.terminal.ansi[index]))
    });
    div()
        .id(name.key())
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .child(
            div()
                .w(px(64.))
                .h(px(44.))
                .p(px(7.))
                .flex()
                .flex_col()
                .justify_between()
                .rounded_md()
                .border_2()
                .border_color(if chosen { c.accent } else { c.divider })
                .bg(theme.terminal_bg)
                .child(div().w(px(26.)).h(px(4.)).rounded_full().bg(theme.accent))
                .child(div().flex().gap(px(2.)).children(strip)),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(if chosen { c.text } else { c.muted })
                .child(name.label()),
        )
}

/// Theme colors copied out so `cx` stays free for listeners while rendering.
#[derive(Clone, Copy)]
struct Colors {
    bg: Hsla,
    card: Hsla,
    text: Hsla,
    muted: Hsla,
    hover: Hsla,
    off: Hsla,
    accent: Hsla,
    divider: Hsla,
    error: Hsla,
}

impl Colors {
    fn of(theme: &Theme) -> Self {
        Self {
            bg: theme.terminal_bg,
            card: theme.sidebar_hover,
            text: theme.sidebar_text,
            muted: theme.sidebar_muted,
            hover: theme.sidebar_selected,
            off: theme.sidebar_selected,
            accent: theme.accent,
            divider: theme.divider,
            error: hsla(theme.terminal.ansi[9]),
        }
    }
}

fn section(title: &'static str, c: Colors, rows: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .px_1()
                .text_size(px(11.))
                .text_color(c.muted)
                .child(title),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .px_3()
                .rounded_lg()
                .bg(c.card)
                .children(rows),
        )
}

fn row(
    label: &'static str,
    hint: Option<&'static str>,
    c: Colors,
    control: impl IntoElement,
) -> AnyElement {
    div()
        .h(px(44.))
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(div().truncate().child(label))
                .children(hint.map(|hint| {
                    div()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(c.muted)
                        .child(hint)
                })),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, c: Colors) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(24.))
        .min_w(px(24.))
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(c.divider)
        .cursor_pointer()
        .hover(|style| style.bg(c.hover))
        .child(label.into())
}

/// A command and its shortcut. The key box is a button: press it, then press the keys.
fn key_row(
    command: Command,
    keys: Option<&str>,
    recording: Option<Command>,
    c: Colors,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let name = command.key();
    let listening = recording == Some(command);
    let changed = keys != command.default_keys();
    let keys_box = div()
        .id(ElementId::Name(format!("keys-{name}").into()))
        .h(px(22.))
        .min_w(px(58.))
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(if listening { c.accent } else { c.divider })
        .cursor_pointer()
        .hover(|style| style.bg(c.hover))
        .text_size(px(12.))
        .text_color(if changed { c.accent } else { c.text })
        .child(match (keys, listening) {
            (_, true) => "Press keys…".to_owned(),
            (Some(keys), false) => keymap::display(keys),
            (None, false) => "None".to_owned(),
        })
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.record(command, cx)));
    let reset = button(ElementId::Name(format!("reset-{name}").into()), "Reset", c)
        .text_size(px(11.))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.reset_key(command, cx)));
    div()
        .h(px(32.))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .child(command.title()),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .child(keys_box)
                .child(
                    div()
                        .w(px(48.))
                        .flex()
                        .justify_end()
                        .when(changed, |row| row.child(reset)),
                ),
        )
        .into_any_element()
}

fn toggle(id: &'static str, on: bool, c: Colors) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(34.))
        .h(px(20.))
        .p(px(2.))
        .flex()
        .rounded_full()
        .cursor_pointer()
        .bg(if on { c.accent } else { c.off })
        .when(on, Styled::justify_end)
        .child(div().size(px(16.)).rounded_full().bg(c.text))
}

fn field_box(field: &Entity<TextField>, width: f32, window: &Window, c: Colors, cx: &App) -> Div {
    let focused = field.focus_handle(cx).is_focused(window);
    div()
        .w(px(width))
        .h(px(26.))
        .px_2()
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(if focused { c.accent } else { c.divider })
        .bg(c.bg)
        .child(field.clone())
}

impl SettingsWindow {
    fn font_rows(&self, c: Colors, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let file = &self.file;
        vec![
            row(
                "Family",
                None,
                c,
                field_box(&self.font_family, 180., window, c, cx),
            ),
            row(
                "Size",
                None,
                c,
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(button("font-smaller", "−", c).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.step_font_size(-1., cx)),
                    ))
                    .child(
                        div()
                            .w(px(40.))
                            .flex()
                            .justify_center()
                            .child(f32::from(file.font_size).to_string()),
                    )
                    .child(button("font-bigger", "+", c).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.step_font_size(1., cx)),
                    )),
            ),
        ]
    }

    fn appearance_rows(&self, c: Colors, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let chosen = self.file.theme;
        let swatches = ThemeName::ALL.map(|name| {
            swatch(name, name == chosen, c).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| this.write("theme", &name.key().into(), cx),
            ))
        });
        let custom = self.file.sidebar_opacity.is_some();
        let percent = (sidebar_opacity(&self.file) * 100.).round();
        vec![
            div()
                .py_3()
                .flex()
                .justify_between()
                .children(swatches)
                .into_any_element(),
            row(
                "Sidebar opacity",
                Some("Lower lets the desktop show through"),
                c,
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(custom, |stepper| {
                        stepper.child(
                            button("sidebar-opacity-reset", "Reset", c)
                                .text_size(px(11.))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.unset("sidebar_opacity", cx);
                                })),
                        )
                    })
                    .child(button("sidebar-opacity-less", "−", c).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| this.step_sidebar_opacity(false, cx),
                    )))
                    .child(
                        div()
                            .w(px(44.))
                            .flex()
                            .justify_center()
                            .child(format!("{percent}%")),
                    )
                    .child(button("sidebar-opacity-more", "+", c).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| this.step_sidebar_opacity(true, cx),
                    ))),
            ),
        ]
    }

    /// Every command with the keys it runs on: click a shortcut and press the new one.
    fn keyboard_rows(&self, c: Colors, cx: &mut Context<Self>) -> Vec<AnyElement> {
        Command::all()
            .into_iter()
            .map(|command| key_row(command, self.keys_for(command), self.recording, c, cx))
            .collect()
    }

    fn terminal_rows(&self, c: Colors, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let file = &self.file;
        vec![
            row(
                "Default directory",
                Some("New tabs and splits start here"),
                c,
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(field_box(&self.directory, 130., window, c, cx))
                    .child(button("choose-directory", "Choose…", c).on_click(
                        cx.listener(|_, _: &ClickEvent, _, cx| Self::choose_directory(cx)),
                    )),
            ),
            row(
                "Scrollback lines",
                Some("Applies to new tabs and panes"),
                c,
                field_box(&self.scrollback, 100., window, c, cx),
            ),
            row(
                "Option as Meta",
                Some("Off to type é, ü … with Option"),
                c,
                toggle("option-as-meta", file.option_as_meta, c).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        let on = !this.file.option_as_meta;
                        this.write("option_as_meta", &on.into(), cx);
                    },
                )),
            ),
            row(
                "Cursor blink",
                None,
                c,
                toggle("cursor-blink", file.cursor_blink, c).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        let on = !this.file.cursor_blink;
                        this.write("cursor_blink", &on.into(), cx);
                    },
                )),
            ),
        ]
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx.global::<Theme>());
        let font = self.font_rows(c, window, cx);
        let terminal = self.terminal_rows(c, window, cx);
        let appearance = self.appearance_rows(c, cx);
        let keyboard = self.keyboard_rows(c, cx);
        let files = vec![row(
            "Config files",
            Some("config.toml, keymap.toml"),
            c,
            button("open-folder", "Open Folder", c)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_folder(cx))),
        )];

        div()
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            // A click on empty space ends a field edit (blur confirms it).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, _| {
                    window.focus(&this.focus_handle);
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(c.bg)
            .text_size(px(13.))
            .text_color(c.text)
            .child(
                div()
                    .h(px(TITLEBAR))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(c.muted)
                    .child("Settings"),
            )
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .px_5()
                    .pb_4()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(section("THEME", c, appearance))
                    .child(section("FONT", c, font))
                    .child(section("KEYBOARD", c, keyboard))
                    .child(section("TERMINAL", c, terminal))
                    .child(section("FILES", c, files))
                    .child(
                        div()
                            .min_h(px(16.))
                            .px_1()
                            .text_size(px(11.))
                            .text_color(c.error)
                            .children(self.error.clone()),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, WindowHandle};

    use super::*;

    fn settings_window(cx: &mut TestAppContext) -> WindowHandle<SettingsWindow> {
        cx.update(|cx| {
            cx.set_global(Theme::default());
            cx.set_global(Settings::default());
            crate::register_app_actions(cx);
            cx.dispatch_action(&crate::OpenSettings);
        });
        cx.run_until_parked();
        cx.windows()
            .iter()
            .find_map(AnyWindowHandle::downcast::<SettingsWindow>)
            .expect("the Settings window")
    }

    #[test]
    fn opacity_steps_should_land_on_multiples_of_5_percent_within_0_to_1() {
        let steps = [
            (0.72, true),
            (0.72, false),
            (0.7, false),
            (0.98, true),
            (0.02, false),
        ]
        .map(|(opacity, up)| (step_percent(opacity, up) * 100.).round());
        assert_eq!(steps, [75., 70., 65., 100., 0.]);
    }

    #[gpui::test]
    fn a_modifier_on_its_own_should_not_end_recording(cx: &mut TestAppContext) {
        let settings = settings_window(cx);
        let modifier = Keystroke {
            key: "shift".into(),
            key_char: None,
            modifiers: Modifiers::default(),
        };
        settings
            .update(cx, |this, _, cx| {
                this.record(Command::SplitRight, cx);
                this.recorded(&modifier, cx);
                assert_eq!(this.recording, Some(Command::SplitRight));
            })
            .unwrap();
        // Drops the recorder, which is armed and holds a keystroke interceptor.
        settings
            .update(cx, |this, _, _| this.recording = None)
            .unwrap();
    }

    #[gpui::test]
    fn clicking_the_listening_row_should_stop_recording(cx: &mut TestAppContext) {
        let settings = settings_window(cx);
        settings
            .update(cx, |this, _, cx| {
                this.record(Command::SplitRight, cx);
                this.record(Command::SplitRight, cx);
                assert_eq!(this.recording, None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn open_settings_should_open_one_window_and_reuse_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::default());
            cx.set_global(Settings::default());
            crate::register_app_actions(cx);
            cx.dispatch_action(&crate::OpenSettings);
            cx.dispatch_action(&crate::OpenSettings);
        });
        cx.run_until_parked();
        let open = cx
            .windows()
            .iter()
            .filter_map(AnyWindowHandle::downcast::<SettingsWindow>)
            .count();
        assert_eq!(open, 1);
    }
}
