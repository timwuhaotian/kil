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
    ("cmd-shift-c", "close_pane"),
    ("cmd-shift-d", "split_right"),
    ("cmd-d", "split_down"),
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
    /// Every command the Settings window lists, in menu order.
    pub fn all() -> Vec<Self> {
        NAMED
            .iter()
            .map(|(_, command)| *command)
            .filter(|command| *command != Self::Unbind)
            .chain((1..=9).map(Self::SelectTab))
            .collect()
    }

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

    /// The name it has in the Settings list and in a conflict message.
    pub fn title(self) -> String {
        match self {
            Self::NewTab => "New tab".into(),
            Self::CloseTab => "Close tab".into(),
            Self::ClosePane => "Close pane".into(),
            Self::SplitRight => "Split right".into(),
            Self::SplitDown => "Split down".into(),
            Self::FocusLeft => "Focus left".into(),
            Self::FocusRight => "Focus right".into(),
            Self::FocusUp => "Focus up".into(),
            Self::FocusDown => "Focus down".into(),
            Self::NextTab => "Next tab".into(),
            Self::PrevTab => "Previous tab".into(),
            Self::SelectTab(n) => format!("Select tab {n}"),
            Self::Copy => "Copy".into(),
            Self::Paste => "Paste".into(),
            Self::ClearScrollback => "Clear scrollback".into(),
            Self::FontBigger => "Bigger text".into(),
            Self::FontSmaller => "Smaller text".into(),
            Self::FontReset => "Reset text size".into(),
            Self::OpenSettings => "Settings…".into(),
            Self::Hide => "Hide Kil".into(),
            Self::HideOthers => "Hide Others".into(),
            Self::Quit => "Quit Kil".into(),
            Self::Unbind => "Unbind".into(),
        }
    }

    /// The name it has in `keymap.toml`.
    pub fn key(self) -> String {
        match self {
            Self::SelectTab(n) => format!("select_tab_{n}"),
            _ => NAMED
                .iter()
                .find(|(_, command)| *command == self)
                .map_or_else(|| self.title(), |(name, _)| (*name).to_owned()),
        }
    }

    /// The keys it runs on by default, as written in `keymap.toml`.
    pub fn default_keys(self) -> Option<&'static str> {
        DEFAULTS
            .iter()
            .find(|(_, name)| Command::parse(name) == Some(self))
            .map(|(keys, _)| *keys)
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
# "cmd-shift-d" = "split_right"
# "cmd-d"       = "split_down"
# "cmd-shift-c" = "close_pane"
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

/// The keys a pressed chord runs on, as the file writes them: `cmd-shift-d`.
pub fn keystroke_of(event: &Keystroke) -> Option<String> {
    let keys = event.unparse();
    // Releasing a lone modifier arrives as a keystroke named after it; that is not a shortcut.
    let alone = matches!(
        keys.as_str(),
        "shift" | "control" | "alt" | "platform" | "fn"
    );
    (!alone).then_some(keys)
}

/// The same keys as macOS writes them in a menu: `cmd-shift-d` → `⌘⇧D`.
pub fn display(keys: &str) -> String {
    keys.split(' ')
        .map(|stroke| {
            let (modifiers, key) = stroke.rsplit_once('-').unwrap_or(("", stroke));
            let symbols: String = modifiers
                .split('-')
                .filter(|modifier| !modifier.is_empty())
                .map(|modifier| match modifier {
                    "cmd" => "⌘",
                    "shift" => "⇧",
                    "ctrl" => "⌃",
                    "alt" => "⌥",
                    "fn" => "fn",
                    _ => "",
                })
                .collect();
            let key = match key {
                "left" => "←",
                "right" => "→",
                "up" => "↑",
                "down" => "↓",
                "enter" => "↩",
                "space" => "␣",
                "tab" => "⇥",
                "escape" => "esc",
                _ if key.chars().count() == 1 => return format!("{symbols}{}", key.to_uppercase()),
                _ => key,
            };
            format!("{symbols}{key}")
        })
        .collect::<Vec<_>>()
        .join(" then ")
}

/// Drops every line in `[bindings]` that gives `command` its keys, so it runs on its default
/// again. Only the lines naming that command are touched.
pub fn clear_command(text: &str, command: Command) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_bindings = false;
    for line in text.lines() {
        let header = line.trim();
        if header.starts_with('[') {
            in_bindings = header == "[bindings]";
        }
        let mine = parses(line).is_some_and(|(keys, name)| {
            name == command.key() || (name == "unbind" && Some(keys) == command.default_keys())
        });
        if !(in_bindings && mine) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `text` with `command` running on `keys` and nowhere else: whatever the file said about it
/// goes, and its default is unbound unless it is the key pressed.
pub fn rebind(text: &str, command: Command, keys: &str) -> String {
    let text = clear_command(text, command);
    match command.default_keys() {
        Some(default) if default == keys => text,
        Some(default) => set_binding(&set_binding(&text, default, "unbind"), keys, &command.key()),
        None => set_binding(&text, keys, &command.key()),
    }
}

/// Binds `keys` to `name` in `[bindings]`: an existing line for those keys is replaced, else
/// one is added to the table, which is written if the file has none. Comments are kept.
pub fn set_binding(text: &str, keys: &str, name: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_bindings = false;
    let mut found = false;
    for line in text.lines() {
        let header = line.trim();
        if header.starts_with('[') {
            in_bindings = header == "[bindings]";
        }
        if in_bindings && !found && parses(line).is_some_and(|(written, _)| written == keys) {
            out.push_str(&assignment(keys, name));
            found = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !found {
        if !in_bindings {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("[bindings]\n");
        }
        out.push_str(&assignment(keys, name));
        out.push('\n');
    }
    out
}

/// The line a `[bindings]` entry is written as.
fn assignment(keys: &str, name: &str) -> String {
    format!("\"{keys}\" = \"{name}\"")
}

/// A `[bindings]` line as its keys and command name, when it is one. A trailing comment
/// after the value is ignored.
fn parses(line: &str) -> Option<(&str, &str)> {
    let (keys, rest) = line.trim_start().strip_prefix('"')?.split_once('"')?;
    let name = rest
        .trim_start()
        .strip_prefix('=')?
        .trim_start()
        .strip_prefix('"')?;
    Some((keys, name.split_once('"')?.0))
}

#[cfg(test)]
mod tests {
    use gpui::Modifiers;

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
    fn resolve_should_bind_the_split_keys_as_documented() {
        let (table, _) = resolve(None);
        assert_eq!(
            (
                command_for(&table, "cmd-d"),
                command_for(&table, "cmd-shift-d"),
                command_for(&table, "cmd-shift-c")
            ),
            (
                Some(Command::SplitDown),
                Some(Command::SplitRight),
                Some(Command::ClosePane)
            )
        );
    }

    #[test]
    fn resolve_should_let_user_rebind_split_keys() {
        let (table, _) = resolve(Some("[bindings]\n\"cmd-y\" = \"split_down\"\n"));
        assert_eq!(command_for(&table, "cmd-y"), Some(Command::SplitDown));
    }

    #[test]
    fn resolve_should_treat_modifier_order_as_same_key() {
        let (table, _) = resolve(Some("[bindings]\n\"shift-cmd-c\" = \"copy\"\n"));
        assert_eq!(command_for(&table, "cmd-shift-c"), Some(Command::Copy));
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

    #[test]
    fn keystroke_should_ignore_a_lone_modifier() {
        let shift = Keystroke {
            key: "shift".into(),
            key_char: None,
            modifiers: Modifiers::default(),
        };
        assert_eq!(keystroke_of(&shift), None);
        let chord = Keystroke {
            key: "d".into(),
            key_char: Some("d".into()),
            modifiers: Modifiers {
                platform: true,
                ..Default::default()
            },
        };
        assert_eq!(keystroke_of(&chord).as_deref(), Some("cmd-d"));
    }

    #[test]
    fn display_should_write_keys_as_macos_shows_them() {
        assert_eq!(
            ["cmd-d", "cmd-shift-]", "ctrl-tab", "cmd-alt-left"]
                .map(display)
                .join(" "),
            "⌘D ⌘⇧] ⌃⇥ ⌘⌥←"
        );
    }

    #[test]
    fn set_binding_should_replace_the_line_and_keep_comments() {
        let text = "# mine\n[bindings]\n\"cmd-x\" = \"new_tab\" # note\nfont_size = 1\n";
        assert_eq!(
            set_binding(text, "cmd-x", "split_right"),
            "# mine\n[bindings]\n\"cmd-x\" = \"split_right\"\nfont_size = 1\n"
        );
    }

    #[test]
    fn set_binding_should_add_a_key_to_the_table() {
        assert_eq!(
            set_binding(
                "# mine\n[bindings]\n\"cmd-x\" = \"new_tab\"\n",
                "cmd-k",
                "copy"
            ),
            "# mine\n[bindings]\n\"cmd-x\" = \"new_tab\"\n\"cmd-k\" = \"copy\"\n"
        );
    }

    #[test]
    fn set_binding_should_write_the_table_when_the_file_has_none() {
        assert_eq!(
            set_binding("# mine\n", "cmd-k", "copy"),
            "# mine\n\n[bindings]\n\"cmd-k\" = \"copy\"\n"
        );
    }

    #[test]
    fn clear_command_should_leave_other_commands_alone() {
        let text =
            "[bindings]\n\"cmd-x\" = \"new_tab\"\n\"cmd-y\" = \"copy\"\n\"cmd-z\" = \"unbind\"\n";
        assert_eq!(
            clear_command(text, Command::Copy),
            "[bindings]\n\"cmd-x\" = \"new_tab\"\n\"cmd-z\" = \"unbind\"\n"
        );
    }

    #[test]
    fn clear_command_should_take_the_unbind_of_a_default_away_too() {
        let text = "[bindings]\n\"cmd-k\" = \"unbind\"\n\"cmd-y\" = \"copy\"\n";
        assert_eq!(
            clear_command(text, Command::ClearScrollback),
            "[bindings]\n\"cmd-y\" = \"copy\"\n"
        );
    }

    #[test]
    fn rebind_should_move_a_default_key_off_and_onto_the_pressed_one() {
        let rebound = rebind(TEMPLATE, Command::SplitRight, "cmd-shift-k");
        let (table, errors) = resolve(Some(&rebound));
        assert_eq!(
            (
                errors,
                command_for(&table, "cmd-shift-d"),
                command_for(&table, "cmd-shift-k")
            ),
            (vec![], None, Some(Command::SplitRight))
        );
    }

    #[test]
    fn rebind_should_replace_a_binding_the_user_wrote() {
        let once = rebind(TEMPLATE, Command::SplitRight, "cmd-shift-k");
        let twice = rebind(&once, Command::SplitRight, "cmd-k");
        let (table, _) = resolve(Some(&twice));
        assert_eq!(
            (
                command_for(&table, "cmd-shift-d"),
                command_for(&table, "cmd-shift-k"),
                command_for(&table, "cmd-k")
            ),
            (None, None, Some(Command::SplitRight))
        );
    }

    #[test]
    fn clearing_a_rebound_command_should_give_it_the_default_back() {
        let rebound = rebind(TEMPLATE, Command::SplitRight, "cmd-shift-k");
        let (table, _) = resolve(Some(&clear_command(&rebound, Command::SplitRight)));
        assert_eq!(
            (
                command_for(&table, "cmd-shift-d"),
                command_for(&table, "cmd-shift-k")
            ),
            (Some(Command::SplitRight), None)
        );
    }
}
