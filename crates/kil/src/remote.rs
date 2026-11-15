//! The Agent on Edge bridge, owned by the app rather than the Settings window:
//! the QR it shows stays valid until the bridge is switched off, not until the
//! panel that showed it is closed (DESIGN.md §14.4). Kil ▸ Quit and Kil ▸
//! Restart stop it like any other job.

use std::sync::Arc;

use gpui::{App, Global};
use kil_remote::{Bridge, Config};

use crate::{restore, settings::Settings};

/// The running bridge, if any. A global, so the Settings window can start and
/// stop it without holding it: closing the panel leaves the tunnel up.
#[derive(Default)]
pub struct Remote {
    bridge: Option<Arc<Bridge>>,
}

impl Global for Remote {}

impl Remote {
    /// Whether the bridge runs.
    pub fn on(&self) -> bool {
        self.bridge.is_some()
    }

    /// The bridge to read the QR, the pairing state and the server's answers
    /// from; `None` while it is off.
    pub fn bridge(&self) -> Option<&Arc<Bridge>> {
        self.bridge.as_ref()
    }

    /// Starts the bridge, replacing any running one.
    ///
    /// # Errors
    /// Why it could not start, for the Settings panel to show.
    pub fn start(&mut self, config: &Config) -> Result<(), String> {
        self.stop();
        match Bridge::start(config) {
            Ok(bridge) => {
                self.bridge = Some(bridge);
                Ok(())
            }
            Err(err) => Err(format!("{err:#}")),
        }
    }

    /// Stops the bridge: every phone hangs up, and the tunnel (`--public`)
    /// is closed rather than left behind.
    pub fn stop(&mut self) {
        if let Some(bridge) = self.bridge.take() {
            bridge.stop();
        }
    }
}

/// The app's bridge config from its settings: the standard session dir, the
/// remembered default port (a remembered phone dials the one in its QR), and
/// this binary as launcher. Shared by startup and the Settings panel.
pub fn bridge_config(settings: &Settings, public: bool, reset_pairing: bool) -> Option<Config> {
    let dir = restore::Store::standard().map(|store| store.dir().to_owned())?;
    Some(Config {
        dir,
        port: kil_remote::DEFAULT_PORT,
        host: None,
        launcher: std::env::current_exe().ok(),
        public,
        tunnel_hostname: settings.tunnel_hostname.clone(),
        tunnel_token: settings.tunnel_token.clone(),
        reset_pairing,
    })
}

/// Owns the bridge for the app's lifetime and stops it on quit, so a public
/// tunnel never outlives Kil. Runs after `config::apply`; a remembered
/// `remote_enabled` starts the bridge here, logging rather than crashing
/// on failure (Settings shows the same error path when toggled by hand).
pub fn init(cx: &mut App) {
    cx.set_global(Remote::default());
    let settings = if cx.has_global::<Settings>() {
        cx.global::<Settings>().clone()
    } else {
        Settings::default()
    };
    if settings.remote_enabled {
        match bridge_config(&settings, settings.remote_public, false) {
            Some(config) => {
                if let Err(err) = cx.global_mut::<Remote>().start(&config) {
                    log::error!("Agent on Edge bridge did not auto-start: {err}");
                }
            }
            None => log::error!("Agent on Edge bridge did not auto-start: no session dir"),
        }
    }
    cx.on_app_quit(|cx| {
        cx.global_mut::<Remote>().stop();
        async {}
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &std::path::Path) -> Config {
        Config {
            dir: dir.to_owned(),
            port: 0,
            host: None,
            launcher: None,
            public: false,
            tunnel_hostname: None,
            tunnel_token: None,
            reset_pairing: false,
        }
    }

    #[test]
    fn the_bridge_runs_until_it_is_stopped_and_a_start_replaces_one() {
        let dir = std::env::temp_dir().join(format!("kil-remote-{}", std::process::id()));
        kil_term::server::serve_in_thread(&dir).unwrap();
        let mut remote = Remote::default();
        assert!(!remote.on(), "nothing runs at first");

        remote.start(&config(&dir)).unwrap();
        // Kept alive: a freed first bridge's address could be reused.
        let first = remote.bridge().unwrap().clone();
        assert!(remote.on());

        // Starting again replaces the running bridge rather than leaking it.
        remote.start(&config(&dir)).unwrap();
        assert!(remote.on());
        assert!(!Arc::ptr_eq(remote.bridge().unwrap(), &first));
        drop(first);

        remote.stop();
        assert!(!remote.on());
        assert!(remote.bridge().is_none());
    }
}
