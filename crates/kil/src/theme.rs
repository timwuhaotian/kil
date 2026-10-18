//! The five built-in themes, picked in Settings or with `theme` in `config.toml`
//! (DESIGN.md §7).

use gpui::{Global, Hsla, Rgba as GpuiRgba, rgb, rgba};
use kil_term::{Palette, Rgba};
use serde::Deserialize;

/// A theme's name in `config.toml`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Night,
    Paper,
    Fjord,
    Ember,
    Dusk,
}

impl ThemeName {
    pub const ALL: [Self; 5] = [
        Self::Night,
        Self::Paper,
        Self::Fjord,
        Self::Ember,
        Self::Dusk,
    ];

    /// As written in `config.toml`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Night => "night",
            Self::Paper => "paper",
            Self::Fjord => "fjord",
            Self::Ember => "ember",
            Self::Dusk => "dusk",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Night => "Night",
            Self::Paper => "Paper",
            Self::Fjord => "Fjord",
            Self::Ember => "Ember",
            Self::Dusk => "Dusk",
        }
    }
}

/// Colors for chrome and terminal content.
pub struct Theme {
    pub terminal: Palette,
    pub terminal_bg: Hsla,
    pub selection_bg: Hsla,
    pub sidebar_bg: Hsla,
    pub sidebar_text: Hsla,
    pub sidebar_muted: Hsla,
    pub sidebar_hover: Hsla,
    pub sidebar_selected: Hsla,
    pub accent: Hsla,
    pub failure: Hsla,
    pub divider: Hsla,
    pub inactive_dim: Hsla,
    pub banner_bg: Hsla,
}

impl Global for Theme {}

impl Default for Theme {
    fn default() -> Self {
        Self::named(ThemeName::Night)
    }
}

/// `[normal 0-7, bright 8-15]`, then foreground, background, cursor.
fn palette(ansi: [u32; 16], foreground: u32, background: u32, cursor: u32) -> Palette {
    Palette {
        ansi: ansi.map(Rgba::hex),
        foreground: Rgba::hex(foreground),
        background: Rgba::hex(background),
        cursor: Rgba::hex(cursor),
    }
}

/// Chrome colors as `0xRRGGBB` or `0xRRGGBBAA` (the translucent ones).
struct Chrome {
    selection: u32,
    sidebar: u32,
    text: u32,
    muted: u32,
    hover: u32,
    selected: u32,
    accent: u32,
    failure: u32,
    divider: u32,
    dim: u32,
    banner: u32,
}

#[expect(clippy::unreadable_literal, reason = "hex colors read as #RRGGBB(AA)")]
impl Theme {
    pub fn named(name: ThemeName) -> Self {
        match name {
            ThemeName::Night => Self::night(),
            ThemeName::Paper => Self::paper(),
            ThemeName::Fjord => Self::fjord(),
            ThemeName::Ember => Self::ember(),
            ThemeName::Dusk => Self::dusk(),
        }
    }

    fn night() -> Self {
        Self::build(
            palette(
                [
                    0x1d1f24, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
                    0x5c6370, 0xef7b85, 0xa8d38a, 0xf0cd8a, 0x74bdf7, 0xd48ae8, 0x67c7d2, 0xe6e9ef,
                ],
                0xd7dae0,
                0x111316,
                0xf2f3f5,
            ),
            &Chrome {
                selection: 0x3d5a80cc,
                sidebar: 0x16181db8,
                text: 0xe3e6eb,
                muted: 0x7d8490,
                hover: 0xffffff0f,
                selected: 0xffffff1f,
                accent: 0x6ca8ff,
                failure: 0xf06c6c,
                divider: 0x262930,
                // Half: on a near-black background a lighter dim moves the background by
                // ~2/255 and leaves only the text to show which pane has focus.
                dim: 0x05060880,
                banner: 0x1d2026f2,
            },
        )
    }

    /// Light: inactive panes fade toward the paper instead of darkening.
    fn paper() -> Self {
        Self::build(
            palette(
                [
                    0x24292f, 0xc4242f, 0x1a7f37, 0x845600, 0x0550ae, 0x8250df, 0x1b7c83, 0x6e7781,
                    0x57606a, 0xa40e26, 0x116329, 0x633c01, 0x1a71d0, 0x835dc7, 0x297b8f, 0x8c959f,
                ],
                0x2e3138,
                0xfbfaf7,
                0x3a3f4b,
            ),
            &Chrome {
                selection: 0xb6d4fecc,
                sidebar: 0xf1efeac8,
                text: 0x23262c,
                muted: 0x7a7f88,
                hover: 0x0000000d,
                selected: 0x00000017,
                accent: 0x2f6fde,
                failure: 0xcf3a3a,
                divider: 0xdcd9d2,
                dim: 0xfbfaf780,
                banner: 0xe9e7e2f2,
            },
        )
    }

    fn fjord() -> Self {
        Self::build(
            palette(
                [
                    0x2a303c, 0xd0707a, 0xa3c48c, 0xe4c98a, 0x7fa6d6, 0xb896c8, 0x82c3c9, 0xd4dae6,
                    0x637089, 0xdd8890, 0xb4d29e, 0xedd7a0, 0x95b8e3, 0xc7a9d6, 0x98d1d6, 0xeef1f6,
                ],
                0xd4dae6,
                0x1c212b,
                0xe5e9f0,
            ),
            &Chrome {
                selection: 0x3e5679cc,
                sidebar: 0x191d26b8,
                text: 0xdde3ee,
                muted: 0x7c8698,
                hover: 0xffffff0f,
                selected: 0xffffff1f,
                accent: 0x88b4e6,
                failure: 0xe07a84,
                divider: 0x2a313d,
                dim: 0x0b0e1480,
                banner: 0x232936f2,
            },
        )
    }

    fn ember() -> Self {
        Self::build(
            palette(
                [
                    0x2b2520, 0xe06c50, 0xa9b665, 0xe3a64f, 0x7daea3, 0xd3869b, 0x89b482, 0xd4c4a8,
                    0x766859, 0xf08a6e, 0xbfcc7c, 0xf0bd6b, 0x95c4b8, 0xe29db0, 0xa2c99a, 0xf2e8d5,
                ],
                0xe8dcc6,
                0x1d1915,
                0xf2c48d,
            ),
            &Chrome {
                selection: 0x5a4632cc,
                sidebar: 0x1a1612b8,
                text: 0xece2d0,
                muted: 0x8f8170,
                hover: 0xffffff0f,
                selected: 0xffffff1c,
                accent: 0xe8a15a,
                failure: 0xea6b55,
                divider: 0x2f2822,
                dim: 0x0d0a0880,
                banner: 0x2a241ef2,
            },
        )
    }

    fn dusk() -> Self {
        Self::build(
            palette(
                [
                    0x262233, 0xe4708e, 0x8fd1a0, 0xe8c88a, 0x8aa8f0, 0xc09af0, 0x7cc8d8, 0xcbc5dc,
                    0x6b628a, 0xf088a4, 0xa6deb4, 0xf0d6a2, 0xa2baf5, 0xd0b2f6, 0x96d6e3, 0xf1eefa,
                ],
                0xddd8ea,
                0x17141f,
                0xe9e4f6,
            ),
            &Chrome {
                selection: 0x4b3d6ecc,
                sidebar: 0x15121cb8,
                text: 0xe3def0,
                muted: 0x8a8199,
                hover: 0xffffff0f,
                selected: 0xffffff1f,
                accent: 0xb48cff,
                failure: 0xef7591,
                divider: 0x2b2638,
                dim: 0x0a080f80,
                banner: 0x221e2df2,
            },
        )
    }

    fn build(terminal: Palette, chrome: &Chrome) -> Self {
        let opaque = |hex: u32| -> Hsla { rgb(hex).into() };
        let translucent = |hex: u32| -> Hsla { rgba(hex).into() };
        Self {
            terminal_bg: hsla(terminal.background),
            terminal,
            selection_bg: translucent(chrome.selection),
            sidebar_bg: translucent(chrome.sidebar),
            sidebar_text: opaque(chrome.text),
            sidebar_muted: opaque(chrome.muted),
            sidebar_hover: translucent(chrome.hover),
            sidebar_selected: translucent(chrome.selected),
            accent: opaque(chrome.accent),
            failure: opaque(chrome.failure),
            divider: opaque(chrome.divider),
            inactive_dim: translucent(chrome.dim),
            banner_bg: translucent(chrome.banner),
        }
    }
}

/// A terminal color as a GPUI color.
pub fn hsla(color: Rgba) -> Hsla {
    GpuiRgba {
        r: f32::from(color.r) / 255.,
        g: f32::from(color.g) / 255.,
        b: f32::from(color.b) / 255.,
        a: 1.,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn every_theme_should_round_trip_through_config_toml() {
        for name in ThemeName::ALL {
            let text = format!("theme = \"{}\"", name.key());
            assert_eq!(Settings::parse(&text).unwrap().theme, name);
        }
    }

    #[test]
    fn themes_should_have_distinct_backgrounds() {
        let mut backgrounds: Vec<_> = ThemeName::ALL
            .iter()
            .map(|&name| Theme::named(name).terminal.background)
            .collect();
        backgrounds.dedup();
        assert_eq!(backgrounds.len(), ThemeName::ALL.len());
    }
}
