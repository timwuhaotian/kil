//! Kil: a native macOS terminal.

mod config;
mod keymap;
mod keys;
mod links;
mod motion;
mod notify;
mod pane_tree;
mod qr;
mod remote;
mod restore;
mod settings;
mod settings_window;
mod terminal_element;
mod terminal_pane;
mod text_field;
mod theme;
mod update;
mod update_window;
mod workspace;

use std::{
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, SharedString, SystemMenuType,
    TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point,
    prelude::*, px, size,
};
use kil_term::Client;

use crate::workspace::Workspace;

actions!(
    kil,
    [
        OpenSettings,
        OpenKeymap,
        Hide,
        HideOthers,
        ShowAll,
        About,
        Minimize,
        Zoom
    ]
);

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
/// ones bound, and when an update is ready to install.
fn menus(cx: &App) -> Vec<Menu> {
    use workspace as w;
    vec![
        Menu {
            name: "Kil".into(),
            items: vec![
                MenuItem::action("About Kil", About),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Keyboard Shortcuts…", OpenKeymap),
                update::menu_item(cx),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Kil", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Restart", w::Restart),
                MenuItem::action("Quit and Stop All Jobs", w::QuitAndStopJobs),
                MenuItem::action("Quit Kil", w::Quit),
            ],
        },
        Menu {
            name: "Shell".into(),
            items: vec![
                MenuItem::action("New Tab", w::NewTab),
                MenuItem::action("Pin Tab", w::PinTab),
                MenuItem::action("Unpin Tab", w::UnpinTab),
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
                MenuItem::action("Bigger Text", w::FontBigger),
                MenuItem::action("Smaller Text", w::FontSmaller),
                MenuItem::action("Actual Size", w::FontReset),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Next Tab", w::NextTab),
                MenuItem::action("Previous Tab", w::PrevTab),
                MenuItem::separator(),
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Focus Left Pane", w::FocusLeft),
                MenuItem::action("Focus Right Pane", w::FocusRight),
                MenuItem::action("Focus Upper Pane", w::FocusUp),
                MenuItem::action("Focus Lower Pane", w::FocusDown),
            ],
        },
    ]
}

/// Fixed window bindings, bound after the merged keymap set (see `config::apply`):
/// they survive every keymap reload, as the `TextField` and Settings bindings do.
#[must_use]
pub fn bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("cmd-m", Minimize, None)]
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
    cx.on_action(|_: &workspace::Restart, cx| workspace::restart_app(cx));
    cx.on_action(|_: &About, _| show_about_panel());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.zoom_window());
        }
    });
    // config::apply already chained the fixed bindings; binding them again is harmless,
    // and a menu shows the keys bound when it was set, so the menus are set again here.
    cx.bind_keys(bindings());
    cx.set_menus(menus(cx));
}

/// The standard About panel: version and copyright from the bundle, no window of our own.
#[expect(
    unsafe_code,
    reason = "one standardAboutPanel call; no owned objects cross it"
)]
fn show_about_panel() {
    use objc2::runtime::{AnyClass, AnyObject};
    let Some(class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    // SAFETY: `sharedApplication` answers an object, and `orderFrontStandardAboutPanel:`
    // takes any sender; both are main-thread AppKit both here and in GPUI's platform code.
    unsafe {
        let app: *mut AnyObject = objc2::msg_send![class, sharedApplication];
        if !app.is_null() {
            let _: () = objc2::msg_send![app, orderFrontStandardAboutPanel: app];
        }
    }
}

fn is_bundle(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.as_encoded_bytes().eq_ignore_ascii_case(b"app"))
}

/// The path that starts Kil again: the app bundle the binary is in (as `*.app/Contents/
/// MacOS/<exe>`), or the binary itself.
fn relaunch_target(exe: &Path) -> PathBuf {
    exe.parent()
        .filter(|dir| dir.ends_with("Contents/MacOS"))
        .and_then(|dir| dir.parent()?.parent())
        .filter(|bundle| is_bundle(bundle))
        .map_or_else(|| exe.to_path_buf(), Path::to_path_buf)
}

/// Opens `target` again once this process has ended: a `/bin/sh` waits for the pid to go
/// away, then `open`s the app bundle, or runs the binary again. False if the waiter
/// couldn't be started, so callers that stop jobs for the restart can stay open instead.
pub(crate) fn open_after_exit(target: &Path) -> bool {
    let script = if is_bundle(target) {
        r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$2""#
    } else {
        r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$2""#
    };
    // Its own process group: closing the terminal that started an unbundled Kil must not
    // take the waiter (and the reopened Kil) down with its SIGHUP.
    let spawned = Command::new("/bin/sh")
        .args(["-c", script, "sh"])
        .arg(std::process::id().to_string())
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match spawned {
        Ok(_) => true,
        Err(err) => {
            log::error!("reopening Kil: {err}");
            false
        }
    }
}

/// Opens Kil again once this process has ended. False if Kil is run from a path that is
/// gone (moved or deleted while running) or the waiter couldn't be started.
#[cfg(not(test))]
pub(crate) fn restart_kil() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        log::error!("reopening Kil: its own path is unknown");
        return false;
    };
    let target = relaunch_target(&exe);
    if !target.exists() {
        log::error!("reopening Kil: {} is gone", target.display());
        return false;
    }
    open_after_exit(&target)
}

/// The test build doesn't start Kil again.
#[cfg(test)]
pub(crate) fn restart_kil() -> bool {
    true
}

/// `kil --server <dir>` runs the session server instead of the app (DESIGN.md §14).
fn server_dir() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != "--server" {
        return None;
    }
    args.next().map(PathBuf::from)
}

/// `kil --remote` exposes the session server to Agent on Edge over a paired
/// TLS tunnel, printing a QR code to scan. Keeps the same session server the
/// app runs, so the panes on the phone are the panes on the desk.
///
/// Optional flags after `--remote`: `--host <ip>` (override the QR address),
/// `--port <n>` (default: an OS-picked free port), and `--public` (set up a
/// `UPnP` port mapping on the router and print a QR for the public IP).
fn remote_config() -> Option<kil_remote::Config> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != "--remote" {
        return None;
    }
    let mut port = 0;
    let mut host = None;
    let mut public = false;
    while let Some(arg) = args.next() {
        match arg.to_str()? {
            "--host" => host = Some(args.next()?.to_string_lossy().into_owned()),
            "--port" => port = args.next()?.to_string_lossy().parse().ok()?,
            "--public" => public = true,
            other => {
                eprintln!("kil --remote: ignoring unknown flag {other}");
                return None;
            }
        }
    }
    let dir = restore::Store::standard().map_or_else(
        || std::env::temp_dir().join("kil"),
        |store| store.dir().to_owned(),
    );
    Some(kil_remote::Config {
        dir,
        port,
        host,
        launcher: std::env::current_exe().ok(),
        public,
    })
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
    if let Some(config) = remote_config() {
        if let Err(err) = kil_remote::run(&config) {
            eprintln!("kil --remote: {err:#}");
            std::process::exit(1);
        }
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
        remote::init(cx);
        update::init(cx);
        let clicks = notify::init();
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
        let opened = cx.open_window(options, |window, cx| {
            cx.new(|cx| Workspace::new(notices, client, store, restored, window, cx))
        });
        match opened {
            Ok(window) => {
                if let Some(clicks) = clicks {
                    let _ = window.update(cx, |_, window, cx| {
                        Workspace::go_to_clicked_notifications(clicks, window, cx);
                    });
                }
            }
            Err(err) => {
                log::error!("failed to open window: {err:#}");
                cx.quit();
            }
        }
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relaunch_target_should_be_the_bundle_the_binary_is_in() {
        assert_eq!(
            relaunch_target(Path::new("/Applications/Kil.app/Contents/MacOS/kil")),
            PathBuf::from("/Applications/Kil.app")
        );
        assert_eq!(
            relaunch_target(Path::new("/Users/me/code/kil/target/debug/kil")),
            PathBuf::from("/Users/me/code/kil/target/debug/kil")
        );
        // A plain folder named *.app is not a bundle LaunchServices can open.
        assert_eq!(
            relaunch_target(Path::new("/tmp/test.app/kil")),
            PathBuf::from("/tmp/test.app/kil")
        );
        assert_eq!(
            relaunch_target(Path::new("/Applications/Kil.APP/Contents/MacOS/kil")),
            PathBuf::from("/Applications/Kil.APP")
        );
    }

    #[gpui::test]
    fn the_kil_menu_should_have_a_restart_item(cx: &mut gpui::TestAppContext) {
        let all = cx.read(menus);
        let kil = all
            .iter()
            .find(|menu| menu.name == "Kil")
            .expect("the Kil menu");
        assert!(
            kil.items
                .iter()
                .any(|item| matches!(item, MenuItem::Action { name, .. } if name == "Restart"))
        );
    }

    #[gpui::test]
    fn minimize_should_stay_bound_when_keymap_reloads(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_app_actions(cx);
            let _ = config::apply(cx);
            let keymap = cx.key_bindings();
            let bound: Vec<String> = keymap
                .borrow()
                .bindings_for_action(&Minimize)
                .map(|binding| {
                    binding
                        .keystrokes()
                        .iter()
                        .map(gpui::KeybindingKeystroke::unparse)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            assert!(
                bound.iter().any(|keys| keys == "cmd-m"),
                "cmd-m for Minimize after reload: {bound:?}"
            );
        });
    }
}
