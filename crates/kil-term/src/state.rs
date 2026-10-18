//! A whole terminal as bytes that a fresh one, of the same size, parses into the same state:
//! what a client gets when it attaches to a session in the server (DESIGN.md §14.5).

use alacritty_terminal::{
    event::EventListener,
    grid::{Dimensions, Grid, Row},
    index::Line,
    term::{Term, TermMode, cell::Cell, color::COUNT},
    vte::ansi::{CursorShape, NamedColor},
};

use crate::transcript::Painter;

/// Renders `term`, and leaves it as it was.
///
/// alacritty exposes the primary screen only while it is active, so with a full-screen
/// program up this swaps screens and back. Entering the alternate screen resets it, so its
/// rows and cursors are copied first and put back exactly.
pub(crate) fn render<T: EventListener>(term: &mut Term<T>, title: Option<&str>) -> Vec<u8> {
    let grid = term.grid();
    let mut paint =
        Painter::with_capacity((grid.history_size() + grid.screen_lines()) * grid.columns());
    let alt = term.mode().contains(TermMode::ALT_SCREEN).then(|| {
        let grid = term.grid();
        let rows: Vec<Row<Cell>> = (0..grid.screen_lines())
            .map(|line| grid[line_at(line)].clone())
            .collect();
        (rows, grid.cursor.clone(), grid.saved_cursor.clone())
    });
    if alt.is_some() {
        // Leaving only swaps the grids.
        term.swap_alt();
    }
    primary(&mut paint, term.grid());
    cursor(&mut paint, term.grid());
    if let Some((rows, cursor_then, saved)) = alt {
        term.swap_alt();
        let grid = term.grid_mut();
        for (line, row) in rows.into_iter().enumerate() {
            grid[line_at(line)] = row;
        }
        grid.cursor = cursor_then;
        grid.saved_cursor = saved;
        paint.reset();
        paint.raw(b"\x1b[?1049h");
        alternate(&mut paint, term.grid());
        cursor(&mut paint, term.grid());
    }
    modes(&mut paint, term);
    colors(&mut paint, term);
    if let Some(title) = title {
        paint.raw(b"\x1b]2;");
        let clean: String = title.chars().filter(|c| !c.is_control()).collect();
        paint.raw(clean.as_bytes());
        paint.raw(b"\x07");
    }
    paint.out
}

fn line_at(line: usize) -> Line {
    Line(i32::try_from(line).unwrap_or(i32::MAX))
}

/// Scrollback and the whole screen, so the history and the screen split where they did.
fn primary(paint: &mut Painter, grid: &Grid<Cell>) {
    let mut line = grid.topmost_line();
    let bottom = grid.bottommost_line();
    while line <= bottom {
        let wrapped = paint.row(&grid[line]);
        if !wrapped && line < bottom {
            paint.reset();
            paint.raw(b"\r\n");
        }
        line += 1;
    }
}

/// Each row at its own position: a full-screen program's screen has no flow to keep.
fn alternate(paint: &mut Painter, grid: &Grid<Cell>) {
    for line in 0..grid.screen_lines() {
        paint.raw(format!("\x1b[{};1H", line + 1).as_bytes());
        paint.row(&grid[line_at(line)]);
    }
}

/// The cursor's position, pending wrap and pen.
fn cursor(paint: &mut Painter, grid: &Grid<Cell>) {
    let cursor = &grid.cursor;
    let (line, col) = (cursor.point.line.0 + 1, cursor.point.column.0 + 1);
    paint.raw(format!("\x1b[{line};{col}H").as_bytes());
    if cursor.input_needs_wrap {
        // Printing the last cell again leaves the next character to wrap, as it would have.
        paint.cell(&grid[cursor.point.line][cursor.point.column]);
    }
    paint.reset();
    paint.pen(&cursor.template);
}

fn modes<T: EventListener>(paint: &mut Painter, term: &Term<T>) {
    let mode = *term.mode();
    let set = |paint: &mut Painter, on: bool, code: &str| {
        paint.raw(format!("\x1b[{code}{}", if on { 'h' } else { 'l' }).as_bytes());
    };
    // Each is set or reset explicitly: the new terminal's defaults may differ.
    set(paint, mode.contains(TermMode::APP_CURSOR), "?1");
    set(paint, mode.contains(TermMode::BRACKETED_PASTE), "?2004");
    set(paint, mode.contains(TermMode::FOCUS_IN_OUT), "?1004");
    set(paint, mode.contains(TermMode::ALTERNATE_SCROLL), "?1007");
    set(paint, mode.contains(TermMode::LINE_WRAP), "?7");
    set(paint, mode.contains(TermMode::SHOW_CURSOR), "?25");
    set(paint, mode.contains(TermMode::INSERT), "4");
    set(paint, mode.contains(TermMode::LINE_FEED_NEW_LINE), "20");
    // The mouse reporting modes exclude each other; setting one clears the rest.
    paint.raw(b"\x1b[?1000l");
    for (flag, code) in [
        (TermMode::MOUSE_REPORT_CLICK, "?1000"),
        (TermMode::MOUSE_DRAG, "?1002"),
        (TermMode::MOUSE_MOTION, "?1003"),
    ] {
        if mode.contains(flag) {
            set(paint, true, code);
        }
    }
    set(paint, mode.contains(TermMode::SGR_MOUSE), "?1006");
    set(paint, mode.contains(TermMode::UTF8_MOUSE), "?1005");
    paint.raw(if mode.contains(TermMode::APP_KEYPAD) {
        b"\x1b="
    } else {
        b"\x1b>"
    });
    let style = term.cursor_style();
    let shape = match style.shape {
        CursorShape::Block | CursorShape::HollowBlock | CursorShape::Hidden => 1,
        CursorShape::Underline => 3,
        CursorShape::Beam => 5,
    };
    let code = if style.blinking { shape } else { shape + 1 };
    paint.raw(format!("\x1b[{code} q").as_bytes());
}

/// Colors the program redefined (OSC 4, 10, 11, 12).
fn colors<T: EventListener>(paint: &mut Painter, term: &Term<T>) {
    let named = [
        (NamedColor::Foreground as usize, "10"),
        (NamedColor::Background as usize, "11"),
        (NamedColor::Cursor as usize, "12"),
    ];
    for index in 0..COUNT {
        let Some(rgb) = term.colors()[index] else {
            continue;
        };
        let spec = format!("rgb:{:02x}/{:02x}/{:02x}", rgb.r, rgb.g, rgb.b);
        if index < 256 {
            paint.raw(format!("\x1b]4;{index};{spec}\x07").as_bytes());
        } else if let Some((_, code)) = named.iter().find(|(slot, _)| *slot == index) {
            paint.raw(format!("\x1b]{code};{spec}\x07").as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::{event::VoidListener, index::Column, term::Config};

    use super::*;
    use crate::{session::GridSize, transcript::replay};

    const SIZE: GridSize = GridSize {
        rows: 4,
        cols: 12,
        cell_width: 8,
        cell_height: 16,
    };

    fn term_with(bytes: &[u8]) -> Term<VoidListener> {
        let config = Config {
            scrolling_history: 100,
            ..Config::default()
        };
        let mut term = Term::new(config, &SIZE, VoidListener);
        replay(&mut term, bytes);
        term
    }

    fn text(grid: &Grid<Cell>) -> Vec<String> {
        let mut line = grid.topmost_line();
        let mut rows = Vec::new();
        while line <= grid.bottommost_line() {
            let row: String = (0..grid.columns())
                .map(|col| grid[line][Column(col)].c)
                .collect();
            rows.push(row.trim_end().to_owned());
            line += 1;
        }
        rows
    }

    /// A copy made from `render`, plus `render` having left the original as it was.
    fn copy_of(original: &mut Term<VoidListener>) -> Term<VoidListener> {
        let before = (text(original.grid()), original.grid().cursor.point);
        let copy = term_with(&render(original, Some("t")));
        assert_eq!(
            (text(original.grid()), original.grid().cursor.point),
            before
        );
        copy
    }

    #[test]
    fn render_should_rebuild_scrollback_screen_and_cursor() {
        let mut original = term_with(b"1\r\n2\r\n3\r\n4\r\n5\r\n$ ab\x1b[1;31m");
        let copy = copy_of(&mut original);
        assert_eq!(text(copy.grid()), text(original.grid()));
        assert_eq!(copy.grid().history_size(), original.grid().history_size());
        assert_eq!(copy.grid().cursor.point, original.grid().cursor.point);
        assert_eq!(
            copy.grid().cursor.template.fg,
            original.grid().cursor.template.fg
        );
    }

    #[test]
    fn render_should_rebuild_the_alternate_screen_and_keep_the_primary() {
        let mut original =
            term_with(b"shell\r\n$ vim\r\n\x1b[?1049h\x1b[2;3Hvim text\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h");
        let mut copy = copy_of(&mut original);
        assert_eq!(text(copy.grid()), text(original.grid()));
        assert_eq!(copy.grid().cursor.point, original.grid().cursor.point);
        assert_eq!(*copy.mode(), *original.mode());
        // Leaving vim shows the shell again, in both.
        replay(&mut copy, b"\x1b[?1049l");
        replay(&mut original, b"\x1b[?1049l");
        assert_eq!(text(copy.grid()), text(original.grid()));
        assert_eq!(copy.grid().cursor.point, original.grid().cursor.point);
    }

    #[test]
    fn render_should_keep_a_pending_wrap() {
        let mut original = term_with(b"123456789012");
        let mut copy = copy_of(&mut original);
        replay(&mut copy, b"x");
        replay(&mut original, b"x");
        assert_eq!(text(copy.grid()), text(original.grid()));
    }

    #[test]
    fn render_should_keep_redefined_colors() {
        let mut original = term_with(b"\x1b]4;1;rgb:12/34/56\x07");
        let copy = copy_of(&mut original);
        assert_eq!(copy.colors()[1], original.colors()[1]);
    }
}
