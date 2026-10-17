//! `~/.config/kil/config.toml` (DESIGN.md §9).

use std::path::PathBuf;

use gpui::{Global, Pixels, SharedString, px};
use serde::Deserialize;

/// User settings; every key is optional in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub font_family: SharedString,
    pub font_size: Pixels,
    pub scrollback_lines: usize,
    pub option_as_meta: bool,
    pub cursor_blink: bool,
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
        Ok(Self {
            font_family: file.font_family.map_or(defaults.font_family, Into::into),
            font_size,
            scrollback_lines: file.scrollback_lines.unwrap_or(defaults.scrollback_lines),
            option_as_meta: file.option_as_meta.unwrap_or(defaults.option_as_meta),
            cursor_blink: file.cursor_blink.unwrap_or(defaults.cursor_blink),
        })
    }
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
}
