//! Applies `config.toml` and `keymap.toml`; re-applied when either file changes.

use std::{fs, io, path::PathBuf, time::SystemTime};

use gpui::{App, SharedString};

use crate::{
    keymap,
    settings::{Settings, config_dir},
};

/// Modification times of both files; a change triggers a reload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    settings: Option<SystemTime>,
    keymap: Option<SystemTime>,
}

fn file(name: &str) -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(name))
}

fn modified(name: &str) -> Option<SystemTime> {
    file(name)
        .and_then(|path| fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
}

pub fn stamp() -> Stamp {
    Stamp {
        settings: modified("config.toml"),
        keymap: modified("keymap.toml"),
    }
}

/// Reads `name`; a missing file is not an error.
fn read(name: &str, notices: &mut Vec<SharedString>) -> Option<String> {
    match fs::read_to_string(file(name)?) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            notices.push(format!("{name}: {err}").into());
            None
        }
    }
}

/// Applies both files and returns problems to show the user. An invalid `config.toml`
/// keeps the settings already in effect; invalid keymap entries are skipped one by one.
pub fn apply(cx: &mut App) -> Vec<SharedString> {
    let mut notices = Vec::new();
    match read("config.toml", &mut notices).map(|text| Settings::parse(&text)) {
        Some(Ok(settings)) => cx.set_global(settings),
        Some(Err(err)) => notices.push(format!("config.toml: {err}").into()),
        None => cx.set_global(Settings::default()),
    }
    if !cx.has_global::<Settings>() {
        cx.set_global(Settings::default());
    }
    let user_keymap = read("keymap.toml", &mut notices);
    let (table, errors) = keymap::resolve(user_keymap.as_deref());
    notices.extend(
        errors
            .into_iter()
            .map(|err| SharedString::from(format!("keymap.toml: {}: {}", err.key, err.reason))),
    );
    cx.clear_key_bindings();
    cx.bind_keys(keymap::bindings(&table));
    notices
}
