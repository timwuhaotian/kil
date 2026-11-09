//! The Agent on Edge bridge, owned by the app rather than the Settings window:
//! the QR it shows stays valid until the bridge is switched off, not until the
//! panel that showed it is closed (DESIGN.md §14.4). Kil ▸ Quit and Kil ▸
//! Restart stop it like any other job.

use std::sync::Arc;

use gpui::{App, Global};
use kil_remote::{Bridge, Config};

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

    /// Stops the bridge: every phone hangs up, and the router's port mapping
    /// (`--public`) is removed rather than left behind.
    pub fn stop(&mut self) {
        if let Some(bridge) = self.bridge.take() {
            bridge.stop();
        }
    }
}

/// Owns the bridge for the app's lifetime and stops it on quit, so a public
/// mapping never outlives Kil.
pub fn init(cx: &mut App) {
    cx.set_global(Remote::default());
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
        }
    }

    #[test]
    fn the_bridge_runs_until_it_is_stopped_and_a_start_replaces_one() {
        let dir = std::env::temp_dir().join(format!("kil-remote-{}", std::process::id()));
        kil_term::server::serve_in_thread(&dir).unwrap();
        let mut remote = Remote::default();
        assert!(!remote.on(), "nothing runs at first");

        remote.start(&config(&dir)).unwrap();
        let first = Arc::as_ptr(remote.bridge().unwrap());
        assert!(remote.on());

        // Starting again replaces the running bridge rather than leaking it.
        remote.start(&config(&dir)).unwrap();
        assert!(remote.on());
        assert_ne!(Arc::as_ptr(remote.bridge().unwrap()), first);

        remote.stop();
        assert!(!remote.on());
        assert!(remote.bridge().is_none());
    }
}
