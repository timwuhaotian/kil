//! Kil: a native macOS terminal.

mod config;
mod keymap;
mod keys;
mod motion;
mod pane_tree;
mod restore;
mod settings;
mod settings_window;
mod terminal_element;
mod terminal_pane;
mod text_field;
mod theme;
mod workspace;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    App, Application, Bounds, Menu, MenuItem, SharedString, SystemMenuType, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point, prelude::*, px, size,
};
use kil_term::Client;

use crate::workspace::Workspace;

actions!(kil, [OpenSettings, OpenKeymap, Hide, HideOthers, ShowAll]);

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

/// The menu bar. Set again whenever the keymap changes, so the shortcuts it shows are the
/// ones bound.
fn menus() -> Vec<Menu> {
    use workspace as w;
    vec![
        Menu {
            name: "Kil".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Keyboard Shortcuts…", OpenKeymap),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Kil", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Kil", w::Quit),
                MenuItem::action("Quit and Stop All Jobs", w::QuitAndStopJobs),
            ],
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

/// Fallbacks for actions no focused element handled, e.g. from the Settings window.
fn register_app_actions(cx: &mut App) {
    cx.on_action(|_: &OpenSettings, cx| settings_window::open(cx));
    cx.on_action(|_: &OpenKeymap, cx| {
        if let Err(err) = config::open_keymap(cx) {
            log::error!("{err}");
        }
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &workspace::Quit, cx| workspace::quit_app(cx));
    cx.on_action(|_: &workspace::QuitAndStopJobs, cx| workspace::quit_and_stop_app(cx));
}

/// `kil --server <dir>` runs the session server instead of the app (DESIGN.md §14).
fn server_dir() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != "--server" {
        return None;
    }
    args.next().map(PathBuf::from)
}

fn serve(dir: &Path) {
    // Before any thread exists: the process that launched it exits here.
    if let Err(err) = kil_term::server::daemonize() {
        eprintln!("kil --server: {err}");
        std::process::exit(1);
    }
    env_logger::init();
    if let Err(err) = kil_term::server::serve(dir) {
        log::error!("session server failed: {err}");
        std::process::exit(1);
    }
}

/// The session server that runs every pane's program, started if none runs. If it can't
/// be, one on a thread of the app stands in, and jobs end when Kil quits.
fn session_server(dir: &Path, notices: &mut Vec<SharedString>) -> Option<Arc<Client>> {
    let started = std::env::current_exe().and_then(|exe| Client::start(dir, &exe));
    let err = match started {
        Ok(client) => return Some(Arc::new(client)),
        Err(err) => err,
    };
    log::error!("session server didn't start: {err}");
    notices.push(
        format!("Kil's session server didn't start ({err}), so jobs will stop when Kil quits.")
            .into(),
    );
    let private = std::env::temp_dir().join(format!("kil-{}", std::process::id()));
    kil_term::server::serve_in_thread(&private)
        .and_then(|()| Client::connect(&private))
        .map(Arc::new)
        .inspect_err(|err| log::error!("no session server at all: {err}"))
        .ok()
}

fn main() {
    forget_launcher_identity();
    if let Some(dir) = server_dir() {
        serve(&dir);
        return;
    }
    env_logger::init();
    Application::new().run(|cx: &mut App| {
        let mut notices = config::apply(cx);
        let store = restore::Store::standard();
        let restored = match store.as_ref().map(restore::Store::load) {
            Some(Ok(restored)) => restored,
            Some(Err(err)) => {
                notices.push(err.into());
                None
            }
            None => None,
        };
        let state_dir = store.as_ref().map_or_else(
            || std::env::temp_dir().join("kil"),
            |store| store.dir().to_owned(),
        );
        let Some(client) = session_server(&state_dir, &mut notices) else {
            cx.quit();
            return;
        };
        register_app_actions(cx);
        // Settings alone doesn't keep the app alive.
        cx.on_window_closed(|cx| {
            if !cx
                .windows()
                .iter()
                .any(|w| w.downcast::<Workspace>().is_some())
            {
                cx.quit();
            }
        })
        .detach();

        let displays: Vec<_> = cx
            .displays()
            .iter()
            .map(|display| display.bounds())
            .collect();
        let bounds = restored
            .as_ref()
            .and_then(|restored| restored.saved.window?.on_screen(&displays))
            .unwrap_or_else(|| {
                WindowBounds::Windowed(Bounds::centered(None, size(px(1100.), px(700.)), cx))
            });
        let options = WindowOptions {
            window_bounds: Some(bounds),
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
            cx.new(|cx| Workspace::new(notices, client, store, restored, window, cx))
        }) {
            log::error!("failed to open window: {err:#}");
            cx.quit();
        }
        cx.activate(true);
    });
}
