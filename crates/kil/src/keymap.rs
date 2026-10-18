//! Default bindings plus `~/.config/kil/keymap.toml` overrides (DESIGN.md §8).

use std::collections::BTreeMap;

use gpui::{KeyBinding, Keystroke};
use serde::Deserialize;

use crate::{terminal_pane, workspace};

/// What a key can be bound to. Names in the file are `snake_case`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
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
    SelectTab(u8),
    Copy,
    Paste,
    ClearScrollback,
    FontBigger,
    FontSmaller,
    FontReset,
    OpenSettings,
    Hide,
    HideOthers,
    Quit,
    Unbind,
}

const NAMED: &[(&str, Command)] = &[
    ("new_tab", Command::NewTab),
    ("close_tab", Command::CloseTab),
    ("close_pane", Command::ClosePane),
    ("split_right", Command::SplitRight),
    ("split_down", Command::SplitDown),
    ("focus_left", Command::FocusLeft),
    ("focus_right", Command::FocusRight),
    ("focus_up", Command::FocusUp),
    ("focus_down", Command::FocusDown),
    ("next_tab", Command::NextTab),
    ("prev_tab", Command::PrevTab),
    ("copy", Command::Copy),
    ("paste", Command::Paste),
    ("clear_scrollback", Command::ClearScrollback),
    ("font_bigger", Command::FontBigger),
    ("font_smaller", Command::FontSmaller),
    ("font_reset", Command::FontReset),
    ("open_settings", Command::OpenSettings),
    ("hide", Command::Hide),
    ("hide_others", Command::HideOthers),
    ("quit", Command::Quit),
    ("unbind", Command::Unbind),
];

const DEFAULTS: &[(&str, &str)] = &[
    ("cmd-t", "new_tab"),
    ("cmd-shift-w", "close_tab"),
    ("cmd-w", "close_pane"),
    ("cmd-d", "split_right"),
    ("cmd-shift-d", "split_down"),
    ("cmd-alt-left", "focus_left"),
    ("cmd-alt-right", "focus_right"),
    ("cmd-alt-up", "focus_up"),
    ("cmd-alt-down", "focus_down"),
    ("cmd-shift-]", "next_tab"),
    ("cmd-shift-[", "prev_tab"),
    ("ctrl-tab", "next_tab"),
    ("ctrl-shift-tab", "prev_tab"),
    ("cmd-1", "select_tab_1"),
    ("cmd-2", "select_tab_2"),
    ("cmd-3", "select_tab_3"),
    ("cmd-4", "select_tab_4"),
    ("cmd-5", "select_tab_5"),
    ("cmd-6", "select_tab_6"),
    ("cmd-7", "select_tab_7"),
    ("cmd-8", "select_tab_8"),
    ("cmd-9", "select_tab_9"),
    ("cmd-c", "copy"),
    ("cmd-v", "paste"),
    ("cmd-k", "clear_scrollback"),
    ("cmd-=", "font_bigger"),
    ("cmd--", "font_smaller"),
    ("cmd-0", "font_reset"),
    ("cmd-,", "open_settings"),
    ("cmd-h", "hide"),
    ("cmd-alt-h", "hide_others"),
    ("cmd-q", "quit"),
];

impl Command {
    pub fn parse(name: &str) -> Option<Self> {
        if let Some(n) = name.strip_prefix("select_tab_") {
            return n
                .parse::<u8>()
                .ok()
                .filter(|n| (1..=9).contains(n))
                .map(Self::SelectTab);
        }
        NAMED
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, command)| *command)
    }

    fn binding(self, keys: &str) -> Option<KeyBinding> {
        use workspace as w;
        let cx = self.context();
        Some(match self {
            Self::NewTab => KeyBinding::new(keys, w::NewTab, cx),
            Self::CloseTab => KeyBinding::new(keys, w::CloseTab, cx),
            Self::ClosePane => KeyBinding::new(keys, w::ClosePane, cx),
            Self::SplitRight => KeyBinding::new(keys, w::SplitRight, cx),
            Self::SplitDown => KeyBinding::new(keys, w::SplitDown, cx),
            Self::FocusLeft => KeyBinding::new(keys, w::FocusLeft, cx),
            Self::FocusRight => KeyBinding::new(keys, w::FocusRight, cx),
            Self::FocusUp => KeyBinding::new(keys, w::FocusUp, cx),
            Self::FocusDown => KeyBinding::new(keys, w::FocusDown, cx),
            Self::NextTab => KeyBinding::new(keys, w::NextTab, cx),
            Self::PrevTab => KeyBinding::new(keys, w::PrevTab, cx),
            Self::SelectTab(1) => KeyBinding::new(keys, w::SelectTab1, cx),
            Self::SelectTab(2) => KeyBinding::new(keys, w::SelectTab2, cx),
            Self::SelectTab(3) => KeyBinding::new(keys, w::SelectTab3, cx),
            Self::SelectTab(4) => KeyBinding::new(keys, w::SelectTab4, cx),
            Self::SelectTab(5) => KeyBinding::new(keys, w::SelectTab5, cx),
            Self::SelectTab(6) => KeyBinding::new(keys, w::SelectTab6, cx),
            Self::SelectTab(7) => KeyBinding::new(keys, w::SelectTab7, cx),
            Self::SelectTab(8) => KeyBinding::new(keys, w::SelectTab8, cx),
            Self::SelectTab(_) => KeyBinding::new(keys, w::SelectTab9, cx),
            Self::Copy => KeyBinding::new(keys, terminal_pane::Copy, cx),
            Self::Paste => KeyBinding::new(keys, terminal_pane::Paste, cx),
            Self::ClearScrollback => KeyBinding::new(keys, terminal_pane::ClearScrollback, cx),
            Self::FontBigger => KeyBinding::new(keys, w::FontBigger, cx),
            Self::FontSmaller => KeyBinding::new(keys, w::FontSmaller, cx),
            Self::FontReset => KeyBinding::new(keys, w::FontReset, cx),
            Self::OpenSettings => KeyBinding::new(keys, crate::OpenSettings, cx),
            Self::Hide => KeyBinding::new(keys, crate::Hide, cx),
            Self::HideOthers => KeyBinding::new(keys, crate::HideOthers, cx),
            Self::Quit => KeyBinding::new(keys, w::Quit, cx),
            Self::Unbind => return None,
        })
    }

    /// App-level commands bind in every window, Settings included.
    fn context(self) -> Option<&'static str> {
        match self {
            Self::Copy | Self::Paste | Self::ClearScrollback => Some("Terminal"),
            Self::OpenSettings | Self::Hide | Self::HideOthers | Self::Quit => None,
            _ => Some("Workspace"),
        }
    }
}

/// A user entry that was skipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapError {
    pub key: String,
    pub reason: String,
}

/// What Kil ▸ Keyboard Shortcuts… writes when there is no `keymap.toml` yet.
pub const TEMPLATE: &str = r#"# Kil key bindings. Each entry replaces the default for its keys; "unbind" removes one.
# Saved changes apply when Kil's window is focused again, and the menus show them.
#
# Commands: new_tab, close_tab, close_pane, split_right, split_down,
#   focus_left, focus_right, focus_up, focus_down, next_tab, prev_tab, select_tab_1 … select_tab_9,
#   copy, paste, clear_scrollback, font_bigger, font_smaller, font_reset,
#   open_settings, hide, hide_others, quit, unbind
# Keys: cmd, ctrl, alt, shift and a key, joined with "-" ("cmd-shift-d"); "ctrl-a |" is two strokes.

[bindings]
# Panes and tabs, as they are by default:
# "cmd-d"       = "split_right"
# "cmd-shift-d" = "split_down"
# "cmd-w"       = "close_pane"
# "cmd-shift-w" = "close_tab"
# "cmd-t"       = "new_tab"
"#;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeymapFile {
    #[serde(default)]
    bindings: BTreeMap<String, String>,
}

/// Merges the defaults with `user` (the file's contents). A user entry replaces whatever
/// the default bound to the same keys; invalid entries are reported and skipped.
pub fn resolve(user: Option<&str>) -> (Vec<(String, Command)>, Vec<KeymapError>) {
    let mut table: BTreeMap<String, Command> = DEFAULTS
        .iter()
        .filter_map(|(keys, name)| Some((normalize(keys).ok()?, Command::parse(name)?)))
        .collect();
    let mut errors = Vec::new();
    let file = match user.map(toml::from_str::<KeymapFile>) {
        None => None,
        Some(Ok(file)) => Some(file),
        Some(Err(err)) => {
            errors.push(KeymapError {
                key: "keymap.toml".into(),
                reason: err.message().to_owned(),
            });
            None
        }
    };
    for (keys, name) in file.map(|file| file.bindings).unwrap_or_default() {
        let keys = match normalize(&keys) {
            Ok(keys) => keys,
            Err(reason) => {
                errors.push(KeymapError { key: keys, reason });
                continue;
            }
        };
        match Command::parse(&name) {
            Some(Command::Unbind) => {
                table.remove(&keys);
            }
            Some(command) => {
                table.insert(keys, command);
            }
            None => errors.push(KeymapError {
                key: keys,
                reason: format!("unknown command \"{name}\""),
            }),
        }
    }
    (table.into_iter().collect(), errors)
}

/// GPUI bindings for a resolved table. Keys were validated by `resolve`.
pub fn bindings(table: &[(String, Command)]) -> Vec<KeyBinding> {
    table
        .iter()
        .filter_map(|(keys, command)| command.binding(keys))
        .collect()
}

/// Parses and re-prints each stroke so `shift-cmd-d` and `cmd-shift-d` are the same key.
fn normalize(keys: &str) -> Result<String, String> {
    let strokes = keys
        .split_whitespace()
        .map(|stroke| {
            Keystroke::parse(stroke)
                .map(|k| k.unparse())
                .map_err(|err| err.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if strokes.is_empty() {
        return Err("empty key".into());
    }
    Ok(strokes.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_for(table: &[(String, Command)], keys: &str) -> Option<Command> {
        table
            .iter()
            .find(|(k, _)| k == keys)
            .map(|(_, command)| *command)
    }

    #[test]
    fn template_should_parse_and_change_nothing() {
        assert_eq!(resolve(Some(TEMPLATE)), resolve(None));
    }

    #[test]
    fn resolve_should_bind_split_right_to_cmd_d_by_default() {
        let (table, _) = resolve(None);
        assert_eq!(command_for(&table, "cmd-d"), Some(Command::SplitRight));
    }

    #[test]
    fn resolve_should_let_user_rebind_split_keys() {
        let (table, _) = resolve(Some("[bindings]\n\"cmd-d\" = \"split_down\"\n"));
        assert_eq!(command_for(&table, "cmd-d"), Some(Command::SplitDown));
    }

    #[test]
    fn resolve_should_treat_modifier_order_as_same_key() {
        let (table, _) = resolve(Some("[bindings]\n\"shift-cmd-d\" = \"split_right\"\n"));
        assert_eq!(
            command_for(&table, "cmd-shift-d"),
            Some(Command::SplitRight)
        );
    }

    #[test]
    fn resolve_should_remove_default_on_unbind() {
        let (table, _) = resolve(Some("[bindings]\n\"cmd-k\" = \"unbind\"\n"));
        assert_eq!(command_for(&table, "cmd-k"), None);
    }

    #[test]
    fn resolve_should_report_unknown_command_and_keep_the_rest() {
        let (table, errors) = resolve(Some(
            "[bindings]\n\"cmd-x\" = \"splt\"\n\"cmd-y\" = \"new_tab\"\n",
        ));
        assert_eq!(
            (errors.len(), command_for(&table, "cmd-y")),
            (1, Some(Command::NewTab))
        );
    }

    #[test]
    fn resolve_should_accept_multi_stroke_bindings() {
        let (table, errors) = resolve(Some("[bindings]\n\"ctrl-a |\" = \"split_right\"\n"));
        assert_eq!(
            (errors, command_for(&table, "ctrl-a |")),
            (vec![], Some(Command::SplitRight))
        );
    }

    #[test]
    fn bindings_should_build_one_per_bound_key() {
        let (table, _) = resolve(None);
        assert_eq!(bindings(&table).len(), DEFAULTS.len());
    }
}
