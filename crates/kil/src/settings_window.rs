//! Kil ▸ Settings…: a separate window that edits `config.toml` one key at a time
//! (DESIGN.md §9). Each change is written, then applied by the same reload that runs when
//! the terminal window regains focus.

use std::fs;

use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, ClickEvent, Context, Div, Entity, FocusHandle,
    Focusable, Hsla, KeyBinding, MouseButton, MouseDownEvent, SharedString, Stateful, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, point, prelude::*, px,
    size,
};

use crate::{
    config,
    settings::{Settings, config_dir},
    text_field::{FieldEvent, TextField},
    theme::{Theme, hsla},
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
            size(px(460.), px(470.)),
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
        let subscriptions = vec![
            cx.subscribe_in(&font_family, window, Self::font_family_edited),
            cx.subscribe_in(&scrollback, window, Self::scrollback_edited),
        ];
        Self {
            focus_handle: cx.focus_handle(),
            file,
            font_family,
            scrollback,
            error,
            _subscriptions: subscriptions,
        }
    }

    fn write(&mut self, key: &str, value: &toml::Value, cx: &mut Context<Self>) {
        match config::write_setting(key, value) {
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

    fn step_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let size = (f32::from(self.file.font_size) + delta).clamp(6., 72.);
        self.write("font_size", &f64::from(size).into(), cx);
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
            div().flex().flex_col().child(label).children(
                hint.map(|hint| div().text_size(px(11.)).text_color(c.muted).child(hint)),
            ),
        )
        .child(control)
        .into_any_element()
}

fn button(id: &'static str, label: &'static str, c: Colors) -> Stateful<Div> {
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
        .child(label)
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

    fn terminal_rows(&self, c: Colors, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let file = &self.file;
        vec![
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
                Some("Applies to new tabs and panes"),
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
                    .flex_1()
                    .px_5()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(section("FONT", c, font))
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
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn open_settings_should_open_one_window_and_reuse_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::dark());
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
