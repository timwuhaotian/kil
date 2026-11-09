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
    Batman,
    Fjord,
    Ember,
    Dusk,
}

impl ThemeName {
    pub const ALL: [Self; 5] = [
        Self::Night,
        Self::Batman,
        Self::Fjord,
        Self::Ember,
        Self::Dusk,
    ];

    /// As written in `config.toml`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Night => "night",
            Self::Batman => "batman",
            Self::Fjord => "fjord",
            Self::Ember => "ember",
            Self::Dusk => "dusk",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Night => "Night",
            Self::Batman => "Batman",
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
    pub field_border: Hsla,
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
    field_border: u32,
    dim: u32,
    banner: u32,
}

#[expect(clippy::unreadable_literal, reason = "hex colors read as #RRGGBB(AA)")]
impl Theme {
    pub fn named(name: ThemeName) -> Self {
        match name {
            ThemeName::Night => Self::night(),
            ThemeName::Batman => Self::batman(),
            ThemeName::Fjord => Self::fjord(),
            ThemeName::Ember => Self::ember(),
            ThemeName::Dusk => Self::dusk(),
        }
    }

    /// This theme with the sidebar at `opacity` (Settings), or as the theme has it.
    #[must_use]
    pub fn with_sidebar_opacity(mut self, opacity: Option<f32>) -> Self {
        if let Some(opacity) = opacity {
            self.sidebar_bg.a = opacity.clamp(0., 1.);
        }
        self
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
                muted: 0x848b97,
                hover: 0xffffff0f,
                selected: 0xffffff1f,
                accent: 0x6ca8ff,
                failure: 0xf06c6c,
                divider: 0x262930,
                field_border: 0x6d6f74,
                // Half: on a near-black background a lighter dim moves the background by
                // ~2/255 and leaves only the text to show which pane has focus.
                dim: 0x05060880,
                banner: 0x1d2026f2,
            },
        )
    }

    /// Batman: black, and the yellow of the logo as the accent (the focus ring).
    fn batman() -> Self {
        Self::build(
            palette(
                [
                    0x2a2a2a, 0xff5f5f, 0x66dd88, 0xffff36, 0x0fc3ff, 0xc79bff, 0x5ce1e6, 0xc7c7c7,
                    0x5f5f5f, 0xff8a8a, 0x9be89b, 0xffff7a, 0x5fd4ff, 0xd9b8ff, 0x8beef0, 0xffeac9,
                ],
                0xffeac9,
                0x000000,
                0xffff36,
            ),
            &Chrome {
                selection: 0x0fc3ff4d,
                sidebar: 0x0b0b0bb8,
                text: 0xffeac9,
                muted: 0x8a8a8a,
                hover: 0xffffff12,
                selected: 0xffffff24,
                accent: 0xffff36,
                failure: 0xff5f5f,
                divider: 0x333333,
                field_border: 0x666666,
                dim: 0x00000080,
                banner: 0x111111f2,
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
                muted: 0x8f98a7,
                hover: 0xffffff0f,
                selected: 0xffffff1f,
                accent: 0x88b4e6,
                failure: 0xe07a84,
                divider: 0x2a313d,
                field_border: 0x767a83,
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
                muted: 0x9b8f7f,
                hover: 0xffffff0f,
                selected: 0xffffff1c,
                accent: 0xe8a15a,
                failure: 0xea6b55,
                divider: 0x2f2822,
                field_border: 0x797471,
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
                muted: 0x928aa0,
                hover: 0xffffff0f,
                selected: 0xffffff1f,
                accent: 0xb48cff,
                failure: 0xef7591,
                divider: 0x2b2638,
                field_border: 0x72707b,
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
            field_border: opaque(chrome.field_border),
            inactive_dim: translucent(chrome.dim),
            banner_bg: translucent(chrome.banner),
        }
    }
}

/// Near-black foreground for use ON accent backgrounds (menu hover text, toggle knob).
#[expect(clippy::unreadable_literal, reason = "hex color reads as #RRGGBB")]
pub fn on_accent() -> Hsla {
    rgb(0x14161a).into()
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
    fn sidebar_opacity_should_replace_the_themes_and_none_should_keep_it() {
        let theme = Theme::named(ThemeName::Batman);
        let own = theme.sidebar_bg.a;
        assert_eq!(
            (
                Theme::named(ThemeName::Batman)
                    .with_sidebar_opacity(None)
                    .sidebar_bg
                    .a,
                theme.with_sidebar_opacity(Some(0.25)).sidebar_bg.a,
            ),
            (own, 0.25)
        );
    }

    /// An sRGB channel (0..=1) linearized for relative luminance (WCAG).
    fn linear(channel: f32) -> f32 {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(color: Hsla) -> f32 {
        let rgb = color.to_rgb();
        0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// The settings card: `sidebar_hover` composited over `terminal_bg`
    /// (see `settings_window::Colors::of`).
    fn settings_card(theme: &Theme) -> Hsla {
        let hover = theme.sidebar_hover.to_rgb();
        let bg = theme.terminal_bg.to_rgb();
        let alpha = hover.a;
        GpuiRgba {
            r: hover.r * alpha + bg.r * (1. - alpha),
            g: hover.g * alpha + bg.g * (1. - alpha),
            b: hover.b * alpha + bg.b * (1. - alpha),
            a: 1.,
        }
        .into()
    }

    #[test]
    fn on_accent_should_meet_aa_against_every_theme_accent() {
        for name in ThemeName::ALL {
            let theme = Theme::named(name);
            let ratio = contrast(on_accent(), theme.accent);
            assert!(ratio >= 4.5, "{name:?}: on_accent vs accent = {ratio:.2}");
        }
    }

    #[test]
    fn field_border_should_meet_three_to_one_on_settings_card_in_every_theme() {
        for name in ThemeName::ALL {
            let theme = Theme::named(name);
            let ratio = contrast(theme.field_border, settings_card(&theme));
            assert!(ratio >= 3.0, "{name:?}: field_border vs card = {ratio:.2}");
        }
    }

    #[test]
    fn sidebar_muted_should_meet_aa_on_card_and_terminal_bg_in_every_theme() {
        for name in ThemeName::ALL {
            let theme = Theme::named(name);
            let on_card = contrast(theme.sidebar_muted, settings_card(&theme));
            let on_bg = contrast(theme.sidebar_muted, theme.terminal_bg);
            assert!(on_card >= 4.5, "{name:?}: muted vs card = {on_card:.2}");
            assert!(on_bg >= 4.5, "{name:?}: muted vs terminal_bg = {on_bg:.2}");
        }
    }

    #[test]
    fn sidebar_text_should_keep_eleven_point_five_contrast_on_terminal_bg_in_every_theme() {
        for name in ThemeName::ALL {
            let theme = Theme::named(name);
            let ratio = contrast(theme.sidebar_text, theme.terminal_bg);
            assert!(ratio >= 11.5, "{name:?}: text vs terminal_bg = {ratio:.2}");
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
