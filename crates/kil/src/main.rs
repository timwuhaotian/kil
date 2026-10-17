//! Kil: a native macOS terminal.

mod config;
mod keymap;
mod keys;
mod motion;
mod pane_tree;
mod settings;
mod terminal_element;
mod terminal_pane;
mod theme;
mod workspace;

use gpui::{
    App, Application, Bounds, Menu, MenuItem, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, point, prelude::*, px, size,
};

use crate::{theme::Theme, workspace::Workspace};

/// An unbundled binary started from another terminal inherits that app's
/// `__CFBundleIdentifier`, and `AppKit` then names and badges kil as that app (e.g. "Otty").
/// Inside our own `.app`, launchd sets the variable correctly, so keep it there.
fn forget_launcher_identity() {
    let bundled = std::env::current_exe()
        .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    if !bundled && std::env::var_os("__CFBundleIdentifier").is_some() {
        // SAFETY: first statement of `main`; no other thread exists yet to read the env.
        #[expect(unsafe_code, reason = "edition 2024 makes env mutation unsafe")]
        unsafe {
            std::env::remove_var("__CFBundleIdentifier");
        }
    }
}

fn menus() -> Vec<Menu> {
    use workspace as w;
    vec![
        Menu {
            name: "Kil".into(),
            items: vec![MenuItem::action("Quit Kil", w::Quit)],
        },
        Menu {
            name: "Shell".into(),
            items: vec![
                MenuItem::action("New Tab", w::NewTab),
                MenuItem::action("Split Right", w::SplitRight),
                MenuItem::action("Split Down", w::SplitDown),
                MenuItem::separator(),
                MenuItem::action("Close Pane", w::ClosePane),
                MenuItem::action("Close Tab", w::CloseTab),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Copy", terminal_pane::Copy),
                MenuItem::action("Paste", terminal_pane::Paste),
                MenuItem::separator(),
                MenuItem::action("Clear Scrollback", terminal_pane::ClearScrollback),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Bigger", w::FontBigger),
                MenuItem::action("Smaller", w::FontSmaller),
                MenuItem::action("Actual Size", w::FontReset),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Next Tab", w::NextTab),
                MenuItem::action("Previous Tab", w::PrevTab),
                MenuItem::separator(),
                MenuItem::action("Focus Left Pane", w::FocusLeft),
                MenuItem::action("Focus Right Pane", w::FocusRight),
                MenuItem::action("Focus Pane Above", w::FocusUp),
                MenuItem::action("Focus Pane Below", w::FocusDown),
            ],
        },
    ]
}

fn main() {
    forget_launcher_identity();
    env_logger::init();
    Application::new().run(|cx: &mut App| {
        cx.set_global(Theme::dark());
        let notices = config::apply(cx);
        cx.set_menus(menus());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1100.), px(700.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Kil".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(18.))),
            }),
            window_background: WindowBackgroundAppearance::Blurred,
            window_min_size: Some(size(px(480.), px(320.))),
            ..Default::default()
        };
        if let Err(err) = cx.open_window(options, |window, cx| {
            cx.new(|cx| Workspace::new(notices, window, cx))
        }) {
            log::error!("failed to open window: {err:#}");
            cx.quit();
        }
        cx.activate(true);
    });
}
