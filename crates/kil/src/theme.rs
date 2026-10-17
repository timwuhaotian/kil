//! The one built-in dark theme (DESIGN.md §7).

use gpui::{Global, Hsla, Rgba as GpuiRgba, rgb, rgba};
use kil_term::{Palette, Rgba};

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
    pub divider: Hsla,
    pub inactive_dim: Hsla,
    pub banner_bg: Hsla,
}

impl Global for Theme {}

impl Theme {
    pub fn dark() -> Self {
        let terminal = Palette::default();
        Self {
            terminal_bg: hsla(terminal.background),
            terminal,
            selection_bg: rgba(0x3d_5a_80_cc).into(),
            sidebar_bg: rgba(0x16_18_1d_b8).into(),
            sidebar_text: rgb(0xe3_e6_eb).into(),
            sidebar_muted: rgb(0x7d_84_90).into(),
            sidebar_hover: rgba(0xff_ff_ff_0f).into(),
            sidebar_selected: rgba(0xff_ff_ff_1f).into(),
            accent: rgb(0x6c_a8_ff).into(),
            divider: rgb(0x26_29_30).into(),
            inactive_dim: rgba(0x0b_0c_0f_4d).into(),
            banner_bg: rgba(0x1d_20_26_f2).into(),
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
