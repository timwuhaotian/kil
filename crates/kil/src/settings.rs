//! `~/.config/kil/config.toml` (DESIGN.md §9).

use std::path::{Path, PathBuf};

use gpui::{Global, Pixels, SharedString, px};
use serde::Deserialize;

use crate::theme::ThemeName;

/// User settings; every key is optional in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub font_family: SharedString,
    pub font_size: Pixels,
    pub scrollback_lines: usize,
    pub option_as_meta: bool,
    pub cursor_blink: bool,
    pub theme: ThemeName,
    /// Where new tabs open; splits follow their pane instead. `None`: new tabs follow the
    /// focused pane too.
    pub default_directory: Option<PathBuf>,
    /// The sidebar background's opacity, 0–1, over the blurred desktop. `None`: the theme's.
    pub sidebar_opacity: Option<f32>,
    /// Check for, download and install updates without being asked (DESIGN.md §15).
    pub auto_update: bool,
}

impl Global for Settings {}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_family: "Menlo".into(),
            font_size: px(13.),
            scrollback_lines: 10_000,
            option_as_meta: true,
            cursor_blink: true,
            theme: ThemeName::default(),
            default_directory: None,
            sidebar_opacity: None,
            auto_update: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SettingsFile {
    font_family: Option<String>,
    font_size: Option<f32>,
    scrollback_lines: Option<usize>,
    option_as_meta: Option<bool>,
    cursor_blink: Option<bool>,
    theme: Option<ThemeName>,
    default_directory: Option<String>,
    sidebar_opacity: Option<f32>,
    auto_update: Option<bool>,
}

impl Settings {
    /// Parses `text` over the defaults.
    ///
    /// # Errors
    /// On invalid TOML, unknown keys or out-of-range values.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: SettingsFile = toml::from_str(text).map_err(|err| err.to_string())?;
        let defaults = Self::default();
        let font_size = match file.font_size {
            Some(size) if (6.0..=72.0).contains(&size) => px(size),
            Some(size) => return Err(format!("font_size {size} is outside 6–72")),
            None => defaults.font_size,
        };
        let sidebar_opacity = match file.sidebar_opacity {
            Some(opacity) if (0.0..=1.0).contains(&opacity) => Some(opacity),
            Some(opacity) => return Err(format!("sidebar_opacity {opacity} is outside 0–1")),
            None => None,
        };
        Ok(Self {
            font_family: file.font_family.map_or(defaults.font_family, Into::into),
            font_size,
            scrollback_lines: file.scrollback_lines.unwrap_or(defaults.scrollback_lines),
            option_as_meta: file.option_as_meta.unwrap_or(defaults.option_as_meta),
            cursor_blink: file.cursor_blink.unwrap_or(defaults.cursor_blink),
            theme: file.theme.unwrap_or(defaults.theme),
            default_directory: file.default_directory.as_deref().and_then(expand_home),
            sidebar_opacity,
            auto_update: file.auto_update.unwrap_or(defaults.auto_update),
        })
    }
}

/// `~` and `~/…` under `$HOME`; an empty path is no path.
pub fn expand_home(path: &str) -> Option<PathBuf> {
    let path = path.trim();
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    match path {
        "" => None,
        "~" => home(),
        _ => match path.strip_prefix("~/") {
            Some(rest) => home().map(|home| home.join(rest)),
            None => Some(PathBuf::from(path)),
        },
    }
}

/// `path` with `$HOME` written as `~`, for showing and saving.
pub fn abbreviate_home(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `text` with `key = value`: the line assigning `key` replaced, else one appended. The
/// file is flat (no tables), so any line assigning `key` is the one. Other lines, comments
/// included, are kept as they were.
pub fn set_key(text: &str, key: &str, value: &toml::Value) -> String {
    let assignment = format!("{key} = {value}");
    let mut found = false;
    let mut out = String::with_capacity(text.len() + assignment.len() + 1);
    for line in text.lines() {
        if !found && assigns(line, key) {
            found = true;
            out.push_str(&assignment);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !found {
        out.push_str(&assignment);
        out.push('\n');
    }
    out
}

/// `text` without the lines assigning `key`, so the default applies again.
pub fn unset_key(text: &str, key: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines().filter(|line| !assigns(line, key)) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn assigns(line: &str, key: &str) -> bool {
    line.trim_start()
        .strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// `$XDG_CONFIG_HOME/kil`, else `~/.config/kil`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|dir| dir.join("kil"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_should_keep_defaults_for_missing_keys() {
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
    }

    #[test]
    fn parse_should_override_font_size() {
        assert_eq!(
            Settings::parse("font_size = 15.0").unwrap().font_size,
            px(15.)
        );
    }

    #[test]
    fn parse_should_reject_unknown_keys() {
        assert!(Settings::parse("fontsize = 15.0").is_err());
    }

    #[test]
    fn parse_should_reject_absurd_font_size() {
        assert!(Settings::parse("font_size = 400.0").is_err());
    }

    #[test]
    fn parse_should_read_the_theme_and_expand_the_default_directory() {
        let settings =
            Settings::parse("theme = \"paper\"\ndefault_directory = \"~/code\"").unwrap();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(
            (settings.theme, settings.default_directory),
            (ThemeName::Paper, Some(home.join("code")))
        );
    }

    #[test]
    fn parse_should_treat_an_empty_default_directory_as_none() {
        let settings = Settings::parse("default_directory = \"\"").unwrap();
        assert_eq!(settings.default_directory, None);
    }

    #[test]
    fn parse_should_reject_an_unknown_theme() {
        assert!(Settings::parse("theme = \"neon\"").is_err());
    }

    #[test]
    fn set_key_should_replace_the_line_and_keep_comments() {
        let text = "# mine\nfont_size = 13.0 # big\ncursor_blink = false\n";
        assert_eq!(
            set_key(text, "font_size", &toml::Value::Float(15.)),
            "# mine\nfont_size = 15.0\ncursor_blink = false\n"
        );
    }

    #[test]
    fn set_key_should_append_a_missing_key() {
        assert_eq!(
            set_key("font_size = 13.0", "font_family", &"SF Mono".into()),
            "font_size = 13.0\nfont_family = \"SF Mono\"\n"
        );
    }

    #[test]
    fn parse_should_read_the_sidebar_opacity_and_reject_one_outside_0_to_1() {
        assert_eq!(
            Settings::parse("sidebar_opacity = 0.5")
                .unwrap()
                .sidebar_opacity,
            Some(0.5)
        );
        assert!(Settings::parse("sidebar_opacity = 1.5").is_err());
    }

    #[test]
    fn unset_key_should_drop_the_assignment_and_keep_the_rest() {
        assert_eq!(
            unset_key(
                "# mine\nsidebar_opacity = 0.5\ntheme = \"dusk\"",
                "sidebar_opacity"
            ),
            "# mine\ntheme = \"dusk\"\n"
        );
    }

    #[test]
    fn set_key_should_not_match_a_longer_key() {
        assert_eq!(
            set_key("cursor_blinking = 1\n", "cursor_blink", &true.into()),
            "cursor_blinking = 1\ncursor_blink = true\n"
        );
    }
}
