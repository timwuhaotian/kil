//! A terminal's text and colors as bytes that paint them again when parsed: what a restored
//! pane shows of its last launch (DESIGN.md §4.9).

use std::sync::Arc;

use alacritty_terminal::{
    event::EventListener,
    grid::Dimensions,
    index::Column,
    sync::FairMutex,
    term::{
        Term, TermMode,
        cell::{Cell, Flags},
    },
    vte::ansi::{Color, NamedColor, Processor},
};

use crate::events::EventProxy;

/// Flags that change how a cell looks; the rest (wrap, wide-char bookkeeping) are layout.
const STYLE: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// A session's terminal, readable from another thread after the session is gone.
#[derive(Clone)]
pub struct Transcript {
    term: Arc<FairMutex<Term<EventProxy>>>,
}

impl Transcript {
    pub(crate) fn new(term: Arc<FairMutex<Term<EventProxy>>>) -> Self {
        Self { term }
    }

    /// Scrollback and screen up to the cursor line, as text with SGR attributes; wrapped
    /// lines stay joined, so they re-wrap at any width. `None` while a full-screen program
    /// shows the alternate screen: the primary one is hidden then, and unchanged.
    ///
    /// Takes the terminal lock for the whole copy, which the PTY thread parses under: call it
    /// off the UI thread, and not per frame.
    #[must_use]
    pub fn render(&self) -> Option<Vec<u8>> {
        encode(&self.term.lock())
    }
}

pub(crate) fn encode<T: EventListener>(term: &Term<T>) -> Option<Vec<u8>> {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return None;
    }
    let grid = term.grid();
    let columns = grid.columns();
    let mut out = Vec::with_capacity((grid.history_size() + grid.screen_lines()) * columns);
    let mut pen = Cell::default();
    let last = grid.cursor.point.line;
    let mut line = grid.topmost_line();
    while line <= last {
        let row = &grid[line];
        let wrapped = row[Column(columns - 1)].flags.contains(Flags::WRAPLINE);
        let end = if wrapped {
            columns
        } else {
            (0..columns)
                .rev()
                .find(|&col| !is_blank(&row[Column(col)]))
                .map_or(0, |col| col + 1)
        };
        for col in 0..end {
            let cell = &row[Column(col)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            if !same_style(cell, &pen) {
                push_sgr(&mut out, cell);
                pen.clone_from(cell);
            }
            push_char(&mut out, cell.c);
            for &c in cell.zerowidth().unwrap_or_default() {
                push_char(&mut out, c);
            }
        }
        if !wrapped {
            // Ends the line before the newline, so a background color doesn't bleed into it.
            if !same_style(&pen, &Cell::default()) {
                out.extend_from_slice(b"\x1b[0m");
                pen = Cell::default();
            }
            out.extend_from_slice(b"\r\n");
        }
        line += 1;
    }
    out.extend_from_slice(b"\x1b[0m");
    Some(out)
}

fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(STYLE)
        && cell.zerowidth().is_none()
}

fn same_style(a: &Cell, b: &Cell) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.flags & STYLE == b.flags & STYLE
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// A full reset plus `cell`'s attributes, so nothing carries over from the previous pen.
fn push_sgr(out: &mut Vec<u8>, cell: &Cell) {
    out.extend_from_slice(b"\x1b[0");
    let flags = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, ";1"),
        (Flags::DIM, ";2"),
        (Flags::ITALIC, ";3"),
        (Flags::UNDERLINE, ";4"),
        (Flags::DOUBLE_UNDERLINE, ";4:2"),
        (Flags::UNDERCURL, ";4:3"),
        (Flags::DOTTED_UNDERLINE, ";4:4"),
        (Flags::DASHED_UNDERLINE, ";4:5"),
        (Flags::INVERSE, ";7"),
        (Flags::HIDDEN, ";8"),
        (Flags::STRIKEOUT, ";9"),
    ] {
        if flags.contains(flag) {
            out.extend_from_slice(code.as_bytes());
        }
    }
    push_color(out, cell.fg, 30);
    push_color(out, cell.bg, 40);
    out.push(b'm');
}

/// `base` is 30 for the foreground and 40 for the background.
fn push_color(out: &mut Vec<u8>, color: Color, base: u8) {
    let code = match color {
        Color::Named(named) => match named as usize {
            index @ 0..8 => format!(";{}", usize::from(base) + index),
            index @ 8..16 => format!(";{}", usize::from(base) + 60 + index - 8),
            _ => match dim_index(named) {
                Some(index) => format!(";{}", usize::from(base) + index),
                // Foreground, Background and Cursor are the defaults.
                None => return,
            },
        },
        Color::Indexed(index) => format!(";{};5;{index}", base + 8),
        Color::Spec(rgb) => format!(";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b),
    };
    out.extend_from_slice(code.as_bytes());
}

fn dim_index(named: NamedColor) -> Option<usize> {
    let dims = [
        NamedColor::DimBlack,
        NamedColor::DimRed,
        NamedColor::DimGreen,
        NamedColor::DimYellow,
        NamedColor::DimBlue,
        NamedColor::DimMagenta,
        NamedColor::DimCyan,
        NamedColor::DimWhite,
    ];
    dims.iter().position(|&dim| dim == named)
}

/// Paints `bytes` into `term` as if a program had printed them.
pub(crate) fn replay<T: EventListener>(term: &mut Term<T>, bytes: &[u8]) {
    Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(term, bytes);
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::{event::VoidListener, index::Line, term::Config};

    use super::*;
    use crate::session::GridSize;

    const SIZE: GridSize = GridSize {
        rows: 4,
        cols: 10,
        cell_width: 8,
        cell_height: 16,
    };

    fn term_with(bytes: &[u8], size: GridSize) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &size, VoidListener);
        replay(&mut term, bytes);
        term
    }

    fn text(term: &Term<VoidListener>) -> Vec<String> {
        let grid = term.grid();
        let mut line = grid.topmost_line();
        let mut rows = Vec::new();
        while line <= grid.bottommost_line() {
            let row: String = (0..grid.columns())
                .map(|col| &grid[line][Column(col)])
                .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
                .map(|cell| cell.c)
                .collect();
            rows.push(row.trim_end().to_owned());
            line += 1;
        }
        rows
    }

    #[test]
    fn encode_should_round_trip_text_colors_and_wide_chars() {
        let original = term_with(
            b"\x1b[1;31mred\x1b[0m ok\r\n\xe4\xb8\xad\xe6\x96\x87\r\n$ ",
            SIZE,
        );
        let copy = term_with(&encode(&original).unwrap(), SIZE);
        assert_eq!(text(&copy)[..2], ["red ok", "中文"]);
        let red = &copy.grid()[Line(0)][Column(0)];
        assert_eq!(
            (red.fg, red.flags.contains(Flags::BOLD)),
            (Color::Named(NamedColor::Red), true)
        );
        assert_eq!(
            copy.grid()[Line(0)][Column(4)].fg,
            Color::Named(NamedColor::Foreground)
        );
    }

    #[test]
    fn encode_should_keep_a_wrapped_line_joined_so_it_reflows() {
        let original = term_with(b"0123456789abcdef\r\n", SIZE);
        let wide = GridSize { cols: 20, ..SIZE };
        let mut copy = term_with(&encode(&original).unwrap(), SIZE);
        copy.resize(wide);
        assert_eq!(text(&copy)[0], "0123456789abcdef");
    }

    #[test]
    fn encode_should_skip_the_alternate_screen() {
        assert_eq!(encode(&term_with(b"\x1b[?1049hvim", SIZE)), None);
    }
}
