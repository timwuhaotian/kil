//! UI-agnostic copy of the visible grid with colors resolved (DESIGN.md §5.3).

use alacritty_terminal::{
    event::EventListener,
    grid::Dimensions,
    term::{Term, cell::Flags, color::Colors},
    vte::ansi::{Color, CursorShape as VteCursorShape, NamedColor, Rgb},
};

/// An sRGB color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgba {
    /// Builds a color from `0xRRGGBB`.
    #[must_use]
    pub const fn hex(rgb: u32) -> Self {
        let [_, r, g, b] = rgb.to_be_bytes();
        Self { r, g, b }
    }

    fn dim(self) -> Self {
        let scale = |c: u8| u8::try_from(u16::from(c) * 2 / 3).unwrap_or(c);
        Self {
            r: scale(self.r),
            g: scale(self.g),
            b: scale(self.b),
        }
    }
}

impl From<Rgb> for Rgba {
    fn from(Rgb { r, g, b }: Rgb) -> Self {
        Self { r, g, b }
    }
}

impl From<Rgba> for Rgb {
    fn from(Rgba { r, g, b }: Rgba) -> Self {
        Self { r, g, b }
    }
}

/// Style bits of a [`RenderCell`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CellFlags(u16);

impl CellFlags {
    /// Bold weight.
    pub const BOLD: Self = Self(1);
    /// Italic style.
    pub const ITALIC: Self = Self(1 << 1);
    /// Any underline style.
    pub const UNDERLINE: Self = Self(1 << 2);
    /// Strike-through.
    pub const STRIKEOUT: Self = Self(1 << 3);
    /// First half of a double-width character.
    pub const WIDE: Self = Self(1 << 4);
    /// Placeholder second half of a double-width character; draw nothing.
    pub const WIDE_SPACER: Self = Self(1 << 5);
    /// Inside the selection.
    pub const SELECTED: Self = Self(1 << 6);

    /// Whether every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    fn set(&mut self, other: Self, on: bool) {
        if on {
            self.0 |= other.0;
        }
    }
}

impl std::ops::BitOr for CellFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// One visible cell, ready to paint: 12 bytes, `Copy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderCell {
    /// The character; `' '` for empty cells.
    pub ch: char,
    /// Foreground, after inverse/dim/hidden are applied.
    pub fg: Rgba,
    /// Background, after inverse is applied.
    pub bg: Rgba,
    /// Style bits.
    pub flags: CellFlags,
}

/// Cursor drawing style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    /// Filled block.
    Block,
    /// Underscore.
    Underline,
    /// Vertical bar.
    Beam,
    /// Outlined block (unfocused).
    HollowBlock,
}

/// Where and how to draw the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorSnapshot {
    /// Viewport row.
    pub row: usize,
    /// Column.
    pub col: usize,
    /// Style.
    pub shape: CursorShape,
}

/// Default colors; programs may override entries at runtime (OSC 4/10/11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The 16 ANSI colors: 8 normal then 8 bright.
    pub ansi: [Rgba; 16],
    /// Default foreground.
    pub foreground: Rgba,
    /// Default background.
    pub background: Rgba,
    /// Cursor color.
    pub cursor: Rgba,
}

#[expect(clippy::unreadable_literal, reason = "hex colors read as #RRGGBB")]
impl Default for Palette {
    fn default() -> Self {
        Self {
            ansi: [
                Rgba::hex(0x1d1f24),
                Rgba::hex(0xe06c75),
                Rgba::hex(0x98c379),
                Rgba::hex(0xe5c07b),
                Rgba::hex(0x61afef),
                Rgba::hex(0xc678dd),
                Rgba::hex(0x56b6c2),
                Rgba::hex(0xabb2bf),
                Rgba::hex(0x5c6370),
                Rgba::hex(0xef7b85),
                Rgba::hex(0xa8d38a),
                Rgba::hex(0xf0cd8a),
                Rgba::hex(0x74bdf7),
                Rgba::hex(0xd48ae8),
                Rgba::hex(0x67c7d2),
                Rgba::hex(0xe6e9ef),
            ],
            foreground: Rgba::hex(0xd7dae0),
            background: Rgba::hex(0x111316),
            cursor: Rgba::hex(0xf2f3f5),
        }
    }
}

impl Palette {
    /// Color for xterm-256 index `index`: ANSI, then the 6×6×6 cube, then grays.
    #[must_use]
    pub fn indexed(&self, index: u8) -> Rgba {
        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let i = index - 16;
                let c = |n: u8| CUBE[usize::from(n % 6)];
                Rgba {
                    r: c(i / 36),
                    g: c(i / 6),
                    b: c(i),
                }
            }
            _ => {
                let level = 8 + (index - 232) * 10;
                Rgba {
                    r: level,
                    g: level,
                    b: level,
                }
            }
        }
    }

    /// Default for a slot of alacritty's 269-entry color table.
    pub(crate) fn slot(&self, slot: usize) -> Rgba {
        const DIM_BLACK: usize = NamedColor::DimBlack as usize;
        const DIM_WHITE: usize = NamedColor::DimWhite as usize;
        match slot {
            0..=255 => self.indexed(u8::try_from(slot).unwrap_or_default()),
            s if s == NamedColor::Background as usize => self.background,
            s if s == NamedColor::Cursor as usize => self.cursor,
            s if s == NamedColor::DimForeground as usize => self.foreground.dim(),
            DIM_BLACK..=DIM_WHITE => self.ansi[slot - DIM_BLACK].dim(),
            _ => self.foreground,
        }
    }
}

/// The visible grid of a session, reused frame to frame to avoid allocation.
#[derive(Debug, Default)]
pub struct Frame {
    /// Columns per row.
    pub cols: usize,
    /// Visible rows.
    pub rows: usize,
    /// `rows × cols` cells, row-major.
    pub cells: Vec<RenderCell>,
    /// Cursor, if visible in the viewport.
    pub cursor: Option<CursorSnapshot>,
    /// Lines scrolled back from the bottom.
    pub display_offset: usize,
    /// Lines of scrollback available.
    pub history: usize,
}

impl Frame {
    /// The cells of viewport row `row`.
    #[must_use]
    pub fn row(&self, row: usize) -> &[RenderCell] {
        let start = (row * self.cols).min(self.cells.len());
        &self.cells[start..(start + self.cols).min(self.cells.len())]
    }

    /// Text of viewport row `row`, without trailing blanks or wide-char spacers.
    #[must_use]
    pub fn row_text(&self, row: usize) -> String {
        let text: String = self
            .row(row)
            .iter()
            .filter(|cell| !cell.flags.contains(CellFlags::WIDE_SPACER))
            .map(|cell| cell.ch)
            .collect();
        text.trim_end().to_owned()
    }

    /// Refills from `term`. Call with the terminal lock held; does not allocate in steady state.
    pub(crate) fn fill<T: EventListener>(&mut self, term: &Term<T>, palette: &Palette) {
        let content = term.renderable_content();
        let (rows, cols) = (term.screen_lines(), term.columns());
        let needed = rows * cols;
        self.cells.clear();
        if self.cells.capacity() > needed * 2 {
            // Don't pin the memory of a window that was briefly much larger.
            self.cells.shrink_to(needed);
        }
        self.cells.reserve(needed);
        self.rows = rows;
        self.cols = cols;
        self.display_offset = content.display_offset;
        self.history = term.history_size();

        let offset = i32::try_from(content.display_offset).unwrap_or(i32::MAX);
        let colors = content.colors;
        for indexed in content.display_iter {
            let cell = indexed.cell;
            let mut flags = CellFlags::default();
            flags.set(CellFlags::BOLD, cell.flags.intersects(Flags::BOLD));
            flags.set(CellFlags::ITALIC, cell.flags.intersects(Flags::ITALIC));
            flags.set(
                CellFlags::UNDERLINE,
                cell.flags.intersects(Flags::ALL_UNDERLINES),
            );
            flags.set(CellFlags::STRIKEOUT, cell.flags.contains(Flags::STRIKEOUT));
            flags.set(CellFlags::WIDE, cell.flags.contains(Flags::WIDE_CHAR));
            flags.set(
                CellFlags::WIDE_SPACER,
                cell.flags.contains(Flags::WIDE_CHAR_SPACER),
            );
            let selected = content
                .selection
                .is_some_and(|range| range.contains(indexed.point));
            flags.set(CellFlags::SELECTED, selected);

            let mut fg = resolve(cell.fg, colors, palette);
            let mut bg = resolve(cell.bg, colors, palette);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = fg.dim();
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            self.cells.push(RenderCell {
                ch: cell.c,
                fg,
                bg,
                flags,
            });
        }

        let cursor = content.cursor;
        let row = cursor.point.line.0 + offset;
        self.cursor = usize::try_from(row)
            .ok()
            .filter(|&row| row < rows)
            .and_then(|row| {
                let shape = match cursor.shape {
                    VteCursorShape::Hidden => return None,
                    VteCursorShape::Block => CursorShape::Block,
                    VteCursorShape::Underline => CursorShape::Underline,
                    VteCursorShape::Beam => CursorShape::Beam,
                    VteCursorShape::HollowBlock => CursorShape::HollowBlock,
                };
                Some(CursorSnapshot {
                    row,
                    col: cursor.point.column.0,
                    shape,
                })
            });
    }
}

fn resolve(color: Color, colors: &Colors, palette: &Palette) -> Rgba {
    match color {
        Color::Spec(rgb) => rgb.into(),
        Color::Named(name) => slot(name as usize, colors, palette),
        Color::Indexed(index) => slot(usize::from(index), colors, palette),
    }
}

fn slot(slot: usize, colors: &Colors, palette: &Palette) -> Rgba {
    colors[slot].map_or_else(|| palette.slot(slot), Rgba::from)
}

#[cfg(test)]
#[expect(clippy::unreadable_literal, reason = "hex colors read as #RRGGBB")]
mod tests {
    use super::*;

    #[test]
    fn indexed_should_map_cube_corner_to_white() {
        assert_eq!(Palette::default().indexed(231), Rgba::hex(0xffffff));
    }

    #[test]
    fn indexed_should_map_first_gray_to_level_8() {
        assert_eq!(Palette::default().indexed(232), Rgba::hex(0x080808));
    }

    #[test]
    fn indexed_should_use_palette_for_ansi_range() {
        let palette = Palette::default();
        assert_eq!(palette.indexed(9), palette.ansi[9]);
    }

    #[test]
    fn render_cell_should_stay_small() {
        assert!(std::mem::size_of::<RenderCell>() <= 16);
    }
}
