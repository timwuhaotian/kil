//! The nine built-in themes, matching the skins killi's `/theme` offers, picked in Settings
//! or with `theme` in `config.toml` (DESIGN.md §7). Each paints the terminal with the skin's
//! own palette (`crates/killi/src/tui/text.rs` in the killi repo): its ground, its lightest
//! text, its brand color as the accent, and the skin's role colors as the ANSI slots.

use gpui::{Global, Hsla, Rgba as GpuiRgba, rgb, rgba};
use kil_term::{Palette, Rgba};
use serde::Deserialize;

/// A theme's name in `config.toml`, the same names killi's `/theme` accepts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    #[default]
    Batman,
    IcedCoffee,
    CreameryStation,
    PastelTweencore,
    Cappuccino,
    ShadesOfGray,
    OffWhite,
    RedOrange,
    #[serde(rename = "snow-and-ice")]
    SnowIce,
}

impl ThemeName {
    pub const ALL: [Self; 9] = [
        Self::Batman,
        Self::IcedCoffee,
        Self::CreameryStation,
        Self::PastelTweencore,
        Self::Cappuccino,
        Self::ShadesOfGray,
        Self::OffWhite,
        Self::RedOrange,
        Self::SnowIce,
    ];

    /// As written in `config.toml`, exactly as killi's `/theme` writes `[ui] theme`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Batman => "batman",
            Self::IcedCoffee => "iced-coffee",
            Self::CreameryStation => "creamery-station",
            Self::PastelTweencore => "pastel-tweencore",
            Self::Cappuccino => "cappuccino",
            Self::ShadesOfGray => "shades-of-gray",
            Self::OffWhite => "off-white",
            Self::RedOrange => "red-orange",
            Self::SnowIce => "snow-and-ice",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Batman => "Batman",
            Self::IcedCoffee => "Iced Coffee",
            Self::CreameryStation => "Creamery Station",
            Self::PastelTweencore => "Pastel Tweencore",
            Self::Cappuccino => "Cappuccino",
            Self::ShadesOfGray => "Shades of Gray",
            Self::OffWhite => "Off White",
            Self::RedOrange => "Red Orange",
            Self::SnowIce => "Snow and Ice",
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
        Self::named(ThemeName::Batman)
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

/// The chrome a skin's role colors give: its selection ground, its "a step off the terminal"
/// tint as the sidebar and banner, its lightest text, its muted shade, its brand color as the
/// accent and its failure red. Hairlines and borders use the skin's rule and muted shades.
#[expect(clippy::unreadable_literal, reason = "hex colors read as #RRGGBB(AA)")]
fn chrome(
    selection: u32,
    tint: u32,
    text: u32,
    muted: u32,
    accent: u32,
    failure: u32,
    rule: u32,
) -> Chrome {
    Chrome {
        selection: selection << 8 | 0xcc,
        sidebar: (tint << 8) | 0xb8,
        text,
        muted,
        hover: 0xffffff0f,
        selected: 0xffffff1f,
        accent,
        failure,
        divider: rule,
        field_border: muted,
        dim: 0x00000080,
        banner: (tint << 8) | 0xf2,
    }
}

#[expect(clippy::unreadable_literal, reason = "hex colors read as #RRGGBB(AA)")]
impl Theme {
    pub fn named(name: ThemeName) -> Self {
        match name {
            ThemeName::Batman => Self::batman(),
            ThemeName::IcedCoffee => Self::iced_coffee(),
            ThemeName::CreameryStation => Self::creamery_station(),
            ThemeName::PastelTweencore => Self::pastel_tweencore(),
            ThemeName::Cappuccino => Self::cappuccino(),
            ThemeName::ShadesOfGray => Self::shades_of_gray(),
            ThemeName::OffWhite => Self::off_white(),
            ThemeName::RedOrange => Self::red_orange(),
            ThemeName::SnowIce => Self::snow_and_ice(),
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

    /// Batman: signal yellow and olive on slate and black — killi's `batman` skin.
    fn batman() -> Self {
        Self::build(
            palette(
                [
                    0x282e3c, 0xff6f7d, 0x988829, 0xe6c832, 0x7e93bd, 0x9fb0d4, 0x8b96b3, 0xc7cfdf,
                    0x686d76, 0xff8c97, 0xada054, 0xead051, 0x98a9ca, 0xb2c0dd, 0xa2abc2, 0xcbd3e2,
                ],
                0xc7cfdf,
                0x101014,
                0xfdff00,
            ),
            &chrome(
                0x39445f, 0x282e3c, 0xc7cfdf, 0x8b96b3, 0xfdff00, 0xff6f7d, 0x3a435c,
            ),
        )
    }

    /// Iced coffee: sky blue, coral and amber on deep slate — killi's `iced-coffee` skin.
    fn iced_coffee() -> Self {
        Self::build(
            palette(
                [
                    0x22303e, 0xef6868, 0x96a4ad, 0xe09c26, 0x6fa8dc, 0xf4cccc, 0x8fc0e8, 0xcfe2f3,
                    0x646e78, 0xf28686, 0xabb6bd, 0xe5ab47, 0x8cb9e3, 0xf6d6d6, 0xa5cded, 0xd3e4f4,
                ],
                0xcfe2f3,
                0x141a22,
                0x6fa8dc,
            ),
            &chrome(
                0x2f4a63, 0x22303e, 0xcfe2f3, 0x9db4c9, 0x6fa8dc, 0xef6868, 0x3d4c5c,
            ),
        )
    }

    /// Creamery station: cream, butter and tan on dark — killi's `creamery-station` skin.
    fn creamery_station() -> Self {
        Self::build(
            palette(
                [
                    0x3a3222, 0xff6f7d, 0x9d9884, 0xd9a84f, 0xd6bd8f, 0xdec092, 0xb3a086, 0xf8f2e7,
                    0x757064, 0xff8c97, 0xb1ad9d, 0xdfb569, 0xdecaa5, 0xe5cda8, 0xc2b39e, 0xf9f3e9,
                ],
                0xf8f2e7,
                0x1a1610,
                0xfce9b2,
            ),
            &chrome(
                0x55482c, 0x3a3222, 0xf8f2e7, 0xb3a086, 0xfce9b2, 0xff6f7d, 0x5f5340,
            ),
        )
    }

    /// Pastel tweencore: pastel pink, lavender and baby blue on dark — killi's `pastel-tweencore`
    /// skin.
    fn pastel_tweencore() -> Self {
        Self::build(
            palette(
                [
                    0x402735, 0xff6f7d, 0x999ea9, 0xffcffc, 0xb9d8fa, 0xcbbeff, 0xd9ecff, 0xfffbee,
                    0x796872, 0xff8c97, 0xadb1ba, 0xffd6fc, 0xc7e0fb, 0xd5cbff, 0xe1f0ff, 0xfffbef,
                ],
                0xfffbee,
                0x181120,
                0xff9cd2,
            ),
            &chrome(
                0x593d54, 0x402735, 0xfffbee, 0xada2d9, 0xff9cd2, 0xff6f7d, 0x514c66,
            ),
        )
    }

    /// Cappuccino: cream, tan and rust over coffee browns — killi's `cappuccino` skin.
    fn cappuccino() -> Self {
        Self::build(
            palette(
                [
                    0x3c2f2f, 0xe65f50, 0x999288, 0xeebe6e, 0xcdaa8c, 0xc9786e, 0xbe9b7b, 0xf8f2e7,
                    0x766d6d, 0xeb7f73, 0xada8a0, 0xf1c884, 0xd7bba3, 0xd4938b, 0xcbaf95, 0xf9f3e9,
                ],
                0xfff4e6,
                0x171110,
                0xe6c49b,
            ),
            &chrome(
                0x4b3832, 0x3c2f2f, 0xfff4e6, 0xbe9b7b, 0xe6c49b, 0xe65f50, 0x5c4a40,
            ),
        )
    }

    /// Shades of gray: grays only, light to dark — killi's `shades-of-gray` skin. It keeps the
    /// default red for failures, as the skin does.
    fn shades_of_gray() -> Self {
        Self::build(
            palette(
                [
                    0x262626, 0xff6f7d, 0x999999, 0xdddddd, 0xaaaaaa, 0xcccccc, 0x999999, 0xf5f5f5,
                    0x676767, 0xff8c97, 0xadadad, 0xe2e2e2, 0xbbbbbb, 0xd6d6d6, 0xadadad, 0xf6f6f6,
                ],
                0xe6e6e6,
                0x121212,
                0xeeeeee,
            ),
            &chrome(
                0x404040, 0x262626, 0xe6e6e6, 0x999999, 0xeeeeee, 0xff6f7d, 0x555555,
            ),
        )
    }

    /// Off white: warm whites and ivory on dark — killi's `off-white` skin.
    fn off_white() -> Self {
        Self::build(
            palette(
                [
                    0x34322e, 0xff6f7d, 0x9d9d98, 0xfff4d6, 0xdcdcd6, 0xf9f1f1, 0xc8c2ba, 0xf3e6e6,
                    0x71706d, 0xff8c97, 0xb1b1ad, 0xfff6dc, 0xe3e3de, 0xfaf4f4, 0xd3cec8, 0xf4e8e8,
                ],
                0xfffff2,
                0x161512,
                0xfffff2,
            ),
            &chrome(
                0x4a4640, 0x34322e, 0xfffff2, 0xc8c2ba, 0xfffff2, 0xff6f7d, 0x69645f,
            ),
        )
    }

    /// Red orange: amber through orange to red on dark — killi's `red-orange` skin.
    fn red_orange() -> Self {
        Self::build(
            palette(
                [
                    0x402010, 0xff4d3c, 0x9c907a, 0xffc100, 0xffae4d, 0xff7400, 0xe68c40, 0xffd250,
                    0x796358, 0xff7163, 0xb0a695, 0xffca26, 0xffbe71, 0xff9033, 0xeba366, 0xffd65e,
                ],
                0xffd250,
                0x180c06,
                0xff9a00,
            ),
            &chrome(
                0x5c3014, 0x402010, 0xffd250, 0xe68c40, 0xff9a00, 0xff4d3c, 0x663a22,
            ),
        )
    }

    /// Snow and ice: white and ice cyan on dark — killi's `snow-and-ice` skin.
    fn snow_and_ice() -> Self {
        Self::build(
            palette(
                [
                    0x123438, 0xff6f7d, 0x89a9a7, 0xffffff, 0x64cdd7, 0x86d6d8, 0x3fd0d4, 0xd2f4f5,
                    0x597174, 0xff8c97, 0xa1bab9, 0xffffff, 0x83d7df, 0x9edee0, 0x65d9dd, 0xd6f5f6,
                ],
                0xffffff,
                0x0c1a1c,
                0x3fd0d4,
            ),
            &chrome(
                0x184e54, 0x123438, 0xffffff, 0x6eafb4, 0x3fd0d4, 0xff6f7d, 0x32585c,
            ),
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
