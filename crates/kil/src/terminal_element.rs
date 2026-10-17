//! Paints a pane's grid: merged backgrounds, grid-locked glyph runs, cursor, IME text
//! (DESIGN.md §6.2).

use gpui::{
    App, BorderStyle, Bounds, Element, ElementId, Entity, Focusable, Font, FontStyle, FontWeight,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, StrikethroughStyle, Style, TextRun, UnderlineStyle, Window, fill,
    outline, point, px, relative, size,
};
use kil_term::{CellFlags, CursorShape, Frame, GridSize, RenderCell, Rgba};

use crate::{
    settings::Settings,
    terminal_pane::{Geometry, TerminalInput, TerminalPane},
    theme::{Theme, hsla},
};

const PADDING_X: Pixels = px(10.);
const PADDING_Y: Pixels = px(6.);
const LINE_HEIGHT: f32 = 1.25;

pub struct TerminalElement {
    pane: Entity<TerminalPane>,
    focused: bool,
}

impl TerminalElement {
    pub fn new(pane: Entity<TerminalPane>, focused: bool) -> Self {
        Self { pane, focused }
    }
}

pub struct Layout {
    line_height: Pixels,
    backgrounds: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<PaintQuad>,
    overlays: Vec<(Point<Pixels>, ShapedLine)>,
}

/// Colors the element needs, copied out of the theme so `cx` is free for the pane update.
struct Colors {
    default_bg: Rgba,
    cursor: Hsla,
    selection: Hsla,
}

/// Style that forces a new glyph run when it changes.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RunStyle {
    fg: Rgba,
    flags: CellFlags,
}

impl RunStyle {
    const DECORATIONS: [CellFlags; 2] = [CellFlags::UNDERLINE, CellFlags::STRIKEOUT];

    fn of(cell: &RenderCell) -> Self {
        let keep = [
            CellFlags::BOLD,
            CellFlags::ITALIC,
            CellFlags::UNDERLINE,
            CellFlags::STRIKEOUT,
        ];
        let flags = keep
            .into_iter()
            .filter(|flag| cell.flags.contains(*flag))
            .fold(CellFlags::default(), |acc, flag| acc | flag);
        Self { fg: cell.fg, flags }
    }

    fn decorated(self) -> bool {
        Self::DECORATIONS
            .iter()
            .any(|flag| self.flags.contains(*flag))
    }

    fn run(self, len: usize, base: &Font) -> TextRun {
        let color = hsla(self.fg);
        let mut font = base.clone();
        if self.flags.contains(CellFlags::BOLD) {
            font.weight = FontWeight::BOLD;
        }
        if self.flags.contains(CellFlags::ITALIC) {
            font.style = FontStyle::Italic;
        }
        TextRun {
            len,
            font,
            color,
            background_color: None,
            underline: self
                .flags
                .contains(CellFlags::UNDERLINE)
                .then(|| UnderlineStyle {
                    color: Some(color),
                    thickness: px(1.),
                    wavy: false,
                }),
            strikethrough: self
                .flags
                .contains(CellFlags::STRIKEOUT)
                .then(|| StrikethroughStyle {
                    color: Some(color),
                    thickness: px(1.),
                }),
        }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Layout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        &mut (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let settings = cx.global::<Settings>();
        let font = gpui::font(settings.font_family.clone());
        let font_size = settings.font_size;
        let theme = cx.global::<Theme>();
        let palette = theme.terminal.clone();
        let colors = Colors {
            default_bg: palette.background,
            cursor: hsla(palette.cursor),
            selection: theme.selection_bg,
        };

        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map_or(font_size * 0.6, |advance| advance.width);
        let line_height = (font_size * LINE_HEIGHT).round();
        let geometry = Geometry {
            origin: point(bounds.origin.x + PADDING_X, bounds.origin.y + PADDING_Y),
            cell: size(cell_width, line_height),
        };
        let grid = GridSize {
            rows: count(bounds.size.height - PADDING_Y * 2., line_height),
            cols: count(bounds.size.width - PADDING_X * 2., cell_width),
            cell_width: pixels_u16(cell_width),
            cell_height: pixels_u16(line_height),
        };
        let focused = self.focused;
        self.pane.update(cx, |pane, _| {
            pane.layout(grid, geometry, &palette);
            let mut layout = build(&pane.frame, geometry, &font, font_size, &colors, window);
            layout.cursor = cursor(&pane.frame, geometry, &colors, focused, pane.cursor_visible);
            if let Some(text) = &pane.marked_text {
                layout.overlays.extend(marked(
                    &pane.frame,
                    geometry,
                    text,
                    &font,
                    font_size,
                    window,
                ));
            }
            layout
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        &mut (): &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for quad in layout.backgrounds.drain(..) {
            window.paint_quad(quad);
        }
        for (origin, line) in &layout.lines {
            if let Err(err) = line.paint(*origin, layout.line_height, window, cx) {
                log::warn!("glyph paint failed: {err}");
            }
        }
        if let Some(cursor) = layout.cursor.take() {
            window.paint_quad(cursor);
        }
        for (origin, line) in &layout.overlays {
            let _ = line.paint(*origin, layout.line_height, window, cx);
        }
        let focus = self.pane.read(cx).focus_handle(cx);
        window.handle_input(
            &focus,
            TerminalInput {
                pane: self.pane.clone(),
            },
            cx,
        );
    }
}

fn build(
    frame: &Frame,
    geometry: Geometry,
    font: &Font,
    font_size: Pixels,
    colors: &Colors,
    window: &Window,
) -> Layout {
    let Geometry { origin, cell } = geometry;
    let mut backgrounds = Vec::new();
    let mut lines = Vec::new();
    let mut text = String::new();
    for row in 0..frame.rows {
        let cells_in_row = frame.row(row);
        let y = origin.y + cell.height * cells(row);

        let background = |cell: &RenderCell| {
            if cell.flags.contains(CellFlags::SELECTED) {
                Some(colors.selection)
            } else {
                (cell.bg != colors.default_bg).then(|| hsla(cell.bg))
            }
        };
        let mut col = 0;
        while col < cells_in_row.len() {
            let bg = background(&cells_in_row[col]);
            let start = col;
            while col < cells_in_row.len() && background(&cells_in_row[col]) == bg {
                col += 1;
            }
            if let Some(bg) = bg {
                let bounds = Bounds::new(
                    point(origin.x + cell.width * cells(start), y),
                    size(cell.width * cells(col - start), cell.height),
                );
                backgrounds.push(fill(bounds, bg));
            }
        }

        let mut col = 0;
        while col < cells_in_row.len() {
            let first = cells_in_row[col];
            if first.flags.contains(CellFlags::WIDE_SPACER) {
                col += 1;
                continue;
            }
            // A run never mixes wide and narrow glyphs, so every glyph can be forced onto the
            // grid: fallback fonts (CJK, symbols) would otherwise drift the columns after them.
            let wide = first.flags.contains(CellFlags::WIDE);
            let style = RunStyle::of(&first);
            let start = col;
            text.clear();
            while let Some(next) = cells_in_row.get(col) {
                if next.flags.contains(CellFlags::WIDE_SPACER) {
                    col += 1;
                    continue;
                }
                if next.flags.contains(CellFlags::WIDE) != wide || RunStyle::of(next) != style {
                    break;
                }
                text.push(next.ch);
                col += 1;
            }
            if text.chars().all(|ch| ch == ' ') && !style.decorated() {
                continue;
            }
            let advance = if wide { cell.width * 2. } else { cell.width };
            let run = style.run(text.len(), font);
            let line = window.text_system().shape_line(
                SharedString::from(text.clone()),
                font_size,
                &[run],
                Some(advance),
            );
            lines.push((point(origin.x + cell.width * cells(start), y), line));
        }
    }
    Layout {
        line_height: cell.height,
        backgrounds,
        lines,
        cursor: None,
        overlays: Vec::new(),
    }
}

fn cursor(
    frame: &Frame,
    geometry: Geometry,
    colors: &Colors,
    focused: bool,
    visible: bool,
) -> Option<PaintQuad> {
    let cursor = frame.cursor?;
    let Geometry { origin, cell } = geometry;
    let wide = frame
        .row(cursor.row)
        .get(cursor.col)
        .is_some_and(|c| c.flags.contains(CellFlags::WIDE));
    let width = if wide { cell.width * 2. } else { cell.width };
    let at = point(
        origin.x + cell.width * cells(cursor.col),
        origin.y + cell.height * cells(cursor.row),
    );
    let block = Bounds::new(at, size(width, cell.height));
    if !focused || cursor.shape == CursorShape::HollowBlock {
        return Some(outline(block, colors.cursor, BorderStyle::Solid));
    }
    if !visible {
        return None;
    }
    let bar = px(2.);
    Some(match cursor.shape {
        CursorShape::Beam => fill(Bounds::new(at, size(bar, cell.height)), colors.cursor),
        CursorShape::Underline => fill(
            Bounds::new(point(at.x, at.y + cell.height - bar), size(width, bar)),
            colors.cursor,
        ),
        // Translucent so the glyph underneath stays readable without a second text pass.
        CursorShape::Block | CursorShape::HollowBlock => fill(block, colors.cursor.opacity(0.7)),
    })
}

/// IME composition text drawn over the cursor cell, underlined.
fn marked(
    frame: &Frame,
    geometry: Geometry,
    text: &str,
    font: &Font,
    font_size: Pixels,
    window: &Window,
) -> Option<(Point<Pixels>, ShapedLine)> {
    let cursor = frame.cursor?;
    let Geometry { origin, cell } = geometry;
    let color = hsla(Rgba::hex(0xf2_f3_f5));
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: Some(hsla(Rgba::hex(0x2a_2e_36))),
        underline: Some(UnderlineStyle {
            color: Some(color),
            thickness: px(1.),
            wavy: false,
        }),
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from(text.to_owned()),
        font_size,
        &[run],
        None,
    );
    let at = point(
        origin.x + cell.width * cells(cursor.col),
        origin.y + cell.height * cells(cursor.row),
    );
    Some((at, line))
}

/// A grid index as a multiplier for pixel math.
#[expect(
    clippy::cast_precision_loss,
    reason = "grid indices are far below 2^24"
)]
pub fn cells(n: usize) -> f32 {
    n as f32
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 1..=u16::MAX first"
)]
fn count(available: Pixels, cell: Pixels) -> u16 {
    (available / cell).floor().clamp(1., f32::from(u16::MAX)) as u16
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=u16::MAX first"
)]
fn pixels_u16(pixels: Pixels) -> u16 {
    f32::from(pixels).round().clamp(0., f32::from(u16::MAX)) as u16
}
