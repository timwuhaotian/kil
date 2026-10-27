//! Paints a pane's grid: merged backgrounds, grid-locked glyph runs and IME text; and, as a
//! view of its own, the pane's cursor (DESIGN.md §6.2).

use gpui::{
    App, BorderStyle, Bounds, Context, DispatchPhase, Element, ElementId, Entity, Focusable, Font,
    FontStyle, FontWeight, Global, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render,
    ShapedLine, SharedString, StrikethroughStyle, Style, Subscription, TextRun, UnderlineStyle,
    WeakEntity, Window, fill, outline, point, px, relative, size,
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
}

impl TerminalElement {
    pub fn new(pane: Entity<TerminalPane>) -> Self {
        Self { pane }
    }
}

pub struct Layout {
    line_height: Pixels,
    backgrounds: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    overlays: Vec<(Point<Pixels>, ShapedLine)>,
}

/// Colors the element needs, copied out of the theme so `cx` is free for the pane update.
struct Colors {
    default_bg: Rgba,
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
            selection: theme.selection_bg,
        };

        let cell_width = cell_width(&font, font_size, window, cx);
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
        self.pane.update(cx, |pane, _| {
            pane.layout(grid, geometry, &palette);
            let mut layout = build(&pane.frame, geometry, &font, font_size, &colors, window);
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
        for (origin, line) in &layout.overlays {
            let _ = line.paint(*origin, layout.line_height, window, cx);
        }
        // While a drag belongs to the program, the pointer may leave the pane, where the pane's
        // own handlers stop firing: moves and the release are caught at window level instead
        // (capture phase, once per event), so the program's edge autoscroll doesn't stall and
        // the release always arrives. The listeners live one frame; each paint while forwarding
        // registers them again.
        if self.pane.read(cx).forwarding {
            let pane = self.pane.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture && event.dragging() {
                    pane.update(cx, |pane, _| {
                        pane.forward_drag(event.position, event.modifiers);
                    });
                }
            });
            let pane = self.pane.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    pane.update(cx, |pane, _| {
                        pane.forward_up(event.position, event.modifiers);
                    });
                }
            });
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

/// Width of one cell for `font` at `size`, remembered across frames.
#[derive(Default)]
struct CellWidth {
    key: Option<(SharedString, Pixels)>,
    width: Pixels,
}

impl Global for CellWidth {}

/// Asking the text system costs a CoreText glyph lookup (measured at ~15% of the UI thread
/// while output streams), so it runs only when the font or size changes.
fn cell_width(font: &Font, size: Pixels, window: &Window, cx: &mut App) -> Pixels {
    let cached = cx.default_global::<CellWidth>();
    if let Some((family, at)) = &cached.key
        && *family == font.family
        && *at == size
    {
        return cached.width;
    }
    let text_system = window.text_system();
    let width = text_system
        .advance(text_system.resolve_font(font), size, 'm')
        .map_or(size * 0.6, |advance| advance.width);
    cx.set_global(CellWidth {
        key: Some((font.family.clone(), size)),
        width,
    });
    width
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
        overlays: Vec::new(),
    }
}

/// A pane's cursor, drawn over the pane as a view of its own. A blink then redraws this one
/// quad: when the pane drew it, every blink laid out and painted every glyph of the grid
/// again (~2.9 ms twice a second for a screen of colored text, measured).
///
/// It follows the pane's notifications, since output, scrolling or a resize move the cursor;
/// the blink notifies only this.
pub struct TerminalCursor {
    pane: WeakEntity<TerminalPane>,
    /// The blink's phase.
    pub(crate) visible: bool,
    _pane_changed: Subscription,
}

impl TerminalCursor {
    pub fn new(pane: &Entity<TerminalPane>, cx: &mut Context<Self>) -> Self {
        Self {
            pane: pane.downgrade(),
            visible: true,
            _pane_changed: cx.observe(pane, |_, _, cx| cx.notify()),
        }
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            cx.notify();
        }
    }
}

impl Render for TerminalCursor {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        CursorElement {
            pane: self.pane.clone(),
            visible: self.visible,
        }
    }
}

struct CursorElement {
    pane: WeakEntity<TerminalPane>,
    visible: bool,
}

impl IntoElement for CursorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for CursorElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<PaintQuad>;

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
        _: Bounds<Pixels>,
        &mut (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let color = hsla(cx.global::<Theme>().terminal.cursor);
        let pane = self.pane.upgrade()?;
        let focused = pane.focus_handle(cx).is_focused(window);
        // The pane's own prepaint, earlier in this frame, laid out the grid and took the
        // frame this reads.
        let pane = pane.read(cx);
        cursor(&pane.frame, pane.geometry?, color, focused, self.visible)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        &mut (): &mut Self::RequestLayoutState,
        cursor: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        if let Some(cursor) = cursor.take() {
            window.paint_quad(cursor);
        }
    }
}

fn cursor(
    frame: &Frame,
    geometry: Geometry,
    color: Hsla,
    focused: bool,
    visible: bool,
) -> Option<PaintQuad> {
    let cursor = frame
        .cursor
        .filter(|cursor| cursor.shape != CursorShape::Hidden)?;
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
        return Some(outline(block, color, BorderStyle::Solid));
    }
    if !visible {
        return None;
    }
    let bar = px(2.);
    Some(match cursor.shape {
        CursorShape::Beam => fill(Bounds::new(at, size(bar, cell.height)), color),
        CursorShape::Underline => fill(
            Bounds::new(point(at.x, at.y + cell.height - bar), size(width, bar)),
            color,
        ),
        // Translucent so the glyph underneath stays readable without a second text pass.
        CursorShape::Block | CursorShape::HollowBlock => fill(block, color.opacity(0.7)),
        CursorShape::Hidden => return None,
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
