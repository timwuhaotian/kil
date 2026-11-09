//! A one-line text field with IME, for tab names and the Settings window.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    InspectorElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, Subscription, TextRun,
    UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative,
    size,
};

use crate::theme::Theme;

actions!(
    text_field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Home,
        End,
        SelectLeft,
        SelectRight,
        SelectHome,
        SelectEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Confirm,
        Cancel
    ]
);

/// Editing keys; fixed, and live only while a field has focus.
pub fn bindings() -> Vec<KeyBinding> {
    let cx = Some("TextField");
    vec![
        KeyBinding::new("backspace", Backspace, cx),
        KeyBinding::new("delete", Delete, cx),
        KeyBinding::new("left", Left, cx),
        KeyBinding::new("right", Right, cx),
        KeyBinding::new("home", Home, cx),
        KeyBinding::new("cmd-left", Home, cx),
        KeyBinding::new("end", End, cx),
        KeyBinding::new("cmd-right", End, cx),
        KeyBinding::new("shift-left", SelectLeft, cx),
        KeyBinding::new("shift-right", SelectRight, cx),
        KeyBinding::new("shift-home", SelectHome, cx),
        KeyBinding::new("shift-end", SelectEnd, cx),
        KeyBinding::new("shift-cmd-left", SelectHome, cx),
        KeyBinding::new("shift-cmd-right", SelectEnd, cx),
        KeyBinding::new("cmd-a", SelectAll, cx),
        KeyBinding::new("cmd-c", Copy, cx),
        KeyBinding::new("cmd-x", Cut, cx),
        KeyBinding::new("cmd-v", Paste, cx),
        KeyBinding::new("enter", Confirm, cx),
        KeyBinding::new("escape", Cancel, cx),
    ]
}

/// What a field tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldEvent {
    /// Enter, or focus left after an edit.
    Confirmed(SharedString),
    /// Escape (the edit is reverted), or focus left without an edit.
    Dismissed,
}

/// Text and selection in UTF-8 byte offsets on char boundaries. No GPUI types, so it is
/// unit-tested; the IME speaks UTF-16, converted at the edge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Buffer {
    text: String,
    /// Empty means a caret. Always ordered; [`Self::anchor`] says which end is fixed.
    selection: Range<usize>,
    /// The end shift-select and mouse-drag extend from. `None` when collapsed.
    anchor: Option<usize>,
    /// IME preedit.
    marked: Option<Range<usize>>,
}

impl Buffer {
    /// All of `text`, selected.
    fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            selection: 0..text.len(),
            anchor: None,
            marked: None,
        }
    }

    /// Where the next typed char lands: the free end of the selection.
    fn caret(&self) -> usize {
        match self.anchor {
            Some(fixed) if fixed == self.selection.start => self.selection.end,
            Some(_) => self.selection.start,
            None => self.selection.end,
        }
    }

    fn prev(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next(&self, offset: usize) -> usize {
        self.text[offset..]
            .chars()
            .next()
            .map_or(self.text.len(), |ch| offset + ch.len_utf8())
    }

    fn move_to(&mut self, offset: usize) {
        self.selection = offset..offset;
        self.anchor = None;
    }

    /// Moves the caret to `offset`, selecting from the anchor (shift-select, mouse-drag).
    /// From a caret the anchor is the caret; from a selection made without one (select-all)
    /// it is the selection's start.
    fn shift_to(&mut self, offset: usize) {
        let anchor = self.anchor.unwrap_or(self.selection.start);
        self.anchor = (anchor != offset).then_some(anchor);
        self.selection = anchor.min(offset)..anchor.max(offset);
    }

    /// Selects the word (letters, digits, `_`) at `offset`; elsewhere just a caret.
    fn select_word(&mut self, offset: usize) {
        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let at = |text: &str, index: usize| text[index..].chars().next().is_some_and(word);
        let on_word = (offset < self.text.len() && at(&self.text, offset))
            || (offset > 0 && at(&self.text, self.prev(offset)));
        if !on_word {
            self.move_to(offset);
            return;
        }
        let mut start = offset;
        while start > 0 && at(&self.text, self.prev(start)) {
            start = self.prev(start);
        }
        let mut end = offset;
        while end < self.text.len() && at(&self.text, end) {
            end = self.next(end);
        }
        self.anchor = Some(start);
        self.selection = start..end;
    }

    fn left(&mut self) {
        let to = if self.selection.is_empty() {
            self.prev(self.caret())
        } else {
            self.selection.start
        };
        self.move_to(to);
    }

    fn right(&mut self) {
        let to = if self.selection.is_empty() {
            self.next(self.caret())
        } else {
            self.selection.end
        };
        self.move_to(to);
    }

    fn backspace(&mut self) {
        if self.selection.is_empty() {
            self.selection = self.prev(self.caret())..self.caret();
        }
        self.replace(None, "");
    }

    fn delete(&mut self) {
        if self.selection.is_empty() {
            self.selection = self.caret()..self.next(self.caret());
        }
        self.replace(None, "");
    }

    /// Replaces `range` (else the preedit, else the selection); the caret lands after it.
    fn replace(&mut self, range: Option<Range<usize>>, new: &str) {
        let range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        self.text.replace_range(range.clone(), new);
        self.move_to(range.start + new.len());
        self.marked = None;
    }

    /// IME preedit: replaces like [`Self::replace`], marks `new`, and selects `selected`
    /// (byte offsets within `new`).
    fn mark(&mut self, range: Option<Range<usize>>, new: &str, selected: Option<Range<usize>>) {
        let range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        self.text.replace_range(range.clone(), new);
        let start = range.start;
        self.anchor = None;
        self.marked = (!new.is_empty()).then(|| start..start + new.len());
        self.selection = selected.map_or(start + new.len()..start + new.len(), |sel| {
            start + sel.start..start + sel.end
        });
    }

    fn to_utf16(&self, offset: usize) -> usize {
        self.text
            .get(..offset)
            .unwrap_or(&self.text)
            .encode_utf16()
            .count()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.to_utf16(range.start)..self.to_utf16(range.end)
    }

    /// Clamped and ordered: the IME's ranges are not trusted to fit.
    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        let (a, b) = (
            byte_offset(&self.text, range.start),
            byte_offset(&self.text, range.end),
        );
        a.min(b)..a.max(b)
    }
}

/// UTF-8 offset of the char at UTF-16 offset `utf16` in `text`, clamped to its end.
fn byte_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        if units >= utf16 {
            return index;
        }
        units += ch.len_utf16();
    }
    text.len()
}

/// Where the text was last painted, for clicks and the IME candidate window.
struct Layout {
    line: ShapedLine,
    bounds: Bounds<Pixels>,
    /// Horizontal scroll that keeps the caret visible.
    scroll: Pixels,
}

pub struct TextField {
    focus_handle: FocusHandle,
    buffer: Buffer,
    /// Text as last confirmed (or set); Escape returns to it.
    confirmed: String,
    layout: Option<Layout>,
    /// A left-drag extends the selection until the button comes up.
    dragging: bool,
    _blur: Subscription,
}

impl EventEmitter<FieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TextField {
    /// A field holding `text`, all selected.
    pub fn new(text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let blur = cx.on_blur(&focus_handle, window, |this, _, cx| this.blurred(cx));
        Self {
            focus_handle,
            buffer: Buffer::new(text),
            confirmed: text.to_owned(),
            layout: None,
            dragging: false,
            _blur: blur,
        }
    }

    /// Replaces the text without emitting, e.g. after the owner rejected a value.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer = Buffer::new(text);
        text.clone_into(&mut self.confirmed);
        cx.notify();
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Buffer)) {
        f(&mut self.buffer);
        cx.notify();
    }

    fn blurred(&mut self, cx: &mut Context<Self>) {
        // A blur cancels the composition (macOS): the preedit goes, typed text stays.
        if let Some(marked) = self.buffer.marked.take() {
            self.buffer.text.replace_range(marked.clone(), "");
            self.buffer.move_to(marked.start);
        }
        if self.buffer.text == self.confirmed {
            cx.emit(FieldEvent::Dismissed);
        } else {
            self.confirm(&Confirm, cx);
        }
        cx.notify();
    }

    fn confirm(&mut self, _: &Confirm, cx: &mut Context<Self>) {
        self.confirmed.clone_from(&self.buffer.text);
        cx.emit(FieldEvent::Confirmed(self.confirmed.clone().into()));
    }

    fn cancel(&mut self, _: &Cancel, cx: &mut Context<Self>) {
        self.buffer = Buffer::new(&self.confirmed);
        cx.emit(FieldEvent::Dismissed);
        cx.notify();
    }

    /// The selected text, if any.
    fn selected_text(&self) -> Option<String> {
        (!self.buffer.selection.is_empty())
            .then(|| self.buffer.text[self.buffer.selection.clone()].to_owned())
    }

    fn copy(&mut self, _: &Copy, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.edit(cx, |buffer| buffer.replace(None, ""));
        }
    }

    fn paste(&mut self, _: &Paste, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let line = text.replace(['\r', '\n'], " ");
            self.edit(cx, |buffer| buffer.replace(None, &line));
        }
    }

    /// The byte offset under `position`, for clicks and drags.
    fn offset_at(&self, position: Point<Pixels>) -> Option<usize> {
        self.layout.as_ref().map(|layout| {
            let x = position.x - layout.bounds.left() + layout.scroll;
            layout.line.closest_index_for_x(x)
        })
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keep the click from reaching the row or window beneath.
        cx.stop_propagation();
        window.focus(&self.focus_handle);
        let Some(offset) = self.offset_at(event.position) else {
            return;
        };
        if event.click_count >= 2 {
            self.dragging = false;
            self.edit(cx, |buffer| buffer.select_word(offset));
        } else {
            self.dragging = true;
            self.edit(cx, |buffer| buffer.move_to(offset));
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging
            && let Some(offset) = self.offset_at(event.position)
        {
            self.edit(cx, |buffer| buffer.shift_to(offset));
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.dragging = false;
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("TextField")
            .track_focus(&self.focus_handle)
            .w_full()
            .overflow_hidden()
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(|this, _: &Backspace, _, cx| this.edit(cx, Buffer::backspace)))
            .on_action(cx.listener(|this, _: &Delete, _, cx| this.edit(cx, Buffer::delete)))
            .on_action(cx.listener(|this, _: &Left, _, cx| this.edit(cx, Buffer::left)))
            .on_action(cx.listener(|this, _: &Right, _, cx| this.edit(cx, Buffer::right)))
            .on_action(cx.listener(|this, _: &Home, _, cx| this.edit(cx, |b| b.move_to(0))))
            .on_action(cx.listener(|this, _: &End, _, cx| {
                this.edit(cx, |b| b.move_to(b.text.len()));
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                this.edit(cx, |b| {
                    let to = b.prev(b.caret());
                    b.shift_to(to);
                });
            }))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.edit(cx, |b| {
                    let to = b.next(b.caret());
                    b.shift_to(to);
                });
            }))
            .on_action(cx.listener(|this, _: &SelectHome, _, cx| this.edit(cx, |b| b.shift_to(0))))
            .on_action(cx.listener(|this, _: &SelectEnd, _, cx| {
                this.edit(cx, |b| b.shift_to(b.text.len()));
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.edit(cx, |b| {
                    b.anchor = Some(0);
                    b.selection = 0..b.text.len();
                });
            }))
            .on_action(cx.listener(|this, action, _, cx| this.copy(action, cx)))
            .on_action(cx.listener(|this, action, _, cx| this.cut(action, cx)))
            .on_action(cx.listener(|this, action, _, cx| this.paste(action, cx)))
            .on_action(cx.listener(|this, action, _, cx| this.confirm(action, cx)))
            .on_action(cx.listener(|this, action, _, cx| this.cancel(action, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(FieldElement { field: cx.entity() })
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range);
        actual.replace(self.buffer.range_to_utf16(&range));
        Some(self.buffer.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.buffer.selection),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked
            .as_ref()
            .map(|range| self.buffer.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.buffer.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|range| self.buffer.range_from_utf16(&range));
        self.edit(cx, |buffer| buffer.replace(range, text));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|range| self.buffer.range_from_utf16(&range));
        let selected = selected.map(|sel| byte_offset(text, sel.start)..byte_offset(text, sel.end));
        self.edit(cx, |buffer| buffer.mark(range, text, selected));
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.buffer.range_from_utf16(&range);
        let left = layout.bounds.left() - layout.scroll;
        Some(Bounds::from_corners(
            point(
                left + layout.line.x_for_index(range.start),
                layout.bounds.top(),
            ),
            point(
                left + layout.line.x_for_index(range.end),
                layout.bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let x = position.x - layout.bounds.left() + layout.scroll;
        let index = layout.line.index_for_x(x)?;
        Some(self.buffer.to_utf16(index))
    }
}

/// Shapes and paints the text, preedit underline, selection and caret, and registers the
/// field as the window's input handler while it has focus.
struct FieldElement {
    field: Entity<TextField>,
}

struct Prepainted {
    line: ShapedLine,
    scroll: Pixels,
    selection: Option<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl IntoElement for FieldElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FieldElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;

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
        let style = Style {
            size: size(relative(1.).into(), window.line_height().into()),
            ..Style::default()
        };
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
        let theme = cx.global::<Theme>();
        let buffer = &self.field.read(cx).buffer;
        let style = window.text_style();
        let run = TextRun {
            len: buffer.text.len(),
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs: Vec<TextRun> = match &buffer.marked {
            None => vec![run],
            Some(marked) => [
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.len(),
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: buffer.text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line =
            window
                .text_system()
                .shape_line(buffer.text.clone().into(), font_size, &runs, None);

        let caret_x = line.x_for_index(buffer.caret());
        let scroll = (caret_x - bounds.size.width + px(2.)).max(px(0.));
        let x = |index| bounds.left() + line.x_for_index(index) - scroll;
        let selection = (!buffer.selection.is_empty()).then(|| {
            fill(
                Bounds::from_corners(
                    point(x(buffer.selection.start), bounds.top()),
                    point(x(buffer.selection.end), bounds.bottom()),
                ),
                theme.selection_bg,
            )
        });
        let caret = buffer.selection.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(x(buffer.caret()), bounds.top()),
                    size(px(1.5), bounds.size.height),
                ),
                theme.accent,
            )
        });
        Prepainted {
            line,
            scroll,
            selection,
            caret,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        &mut (): &mut Self::RequestLayoutState,
        prepainted: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.field.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        let focused = focus.is_focused(window);
        if focused && let Some(selection) = prepainted.selection.take() {
            window.paint_quad(selection);
        }
        let origin = point(bounds.left() - prepainted.scroll, bounds.top());
        let _ = prepainted
            .line
            .paint(origin, window.line_height(), window, cx);
        if focused && let Some(caret) = prepainted.caret.take() {
            window.paint_quad(caret);
        }
        let layout = Layout {
            line: prepainted.line.clone(),
            bounds,
            scroll: prepainted.scroll,
        };
        self.field
            .update(cx, |field, _| field.layout = Some(layout));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret_at(text: &str, offset: usize) -> Buffer {
        let mut buffer = Buffer::new(text);
        buffer.move_to(offset);
        buffer
    }

    #[test]
    fn new_should_select_all() {
        assert_eq!(Buffer::new("标签").selection, 0..6);
    }

    #[test]
    fn typing_should_replace_the_selection() {
        let mut buffer = Buffer::new("shell");
        buffer.replace(None, "build");
        assert_eq!((buffer.text.as_str(), buffer.selection), ("build", 5..5));
    }

    #[test]
    fn backspace_should_remove_one_wide_char() {
        let mut buffer = caret_at("构建", 6);
        buffer.backspace();
        assert_eq!((buffer.text.as_str(), buffer.caret()), ("构", 3));
    }

    #[test]
    fn delete_at_end_should_do_nothing() {
        let mut buffer = caret_at("ab", 2);
        buffer.delete();
        assert_eq!(buffer.text, "ab");
    }

    #[test]
    fn utf16_offsets_should_map_to_char_boundaries() {
        // "a" is 1 unit, "😀" is 2, "中" is 1.
        let buffer = Buffer::new("a😀中");
        assert_eq!(buffer.range_from_utf16(&(1..3)), 1..5);
        assert_eq!(buffer.to_utf16(5), 3);
    }

    #[test]
    fn utf16_ranges_from_the_ime_should_be_clamped_and_ordered() {
        let reversed = Range { start: 9, end: 1 };
        assert_eq!(Buffer::new("ab").range_from_utf16(&reversed), 1..2);
    }

    #[test]
    fn bindings_should_parse_every_editing_key() {
        // `KeyBinding::new` panics on a bad keystroke, so building is the check.
        assert!(bindings().len() >= 18);
    }

    #[test]
    fn shift_moves_should_extend_from_the_anchor_then_collapse_back() {
        let mut buffer = caret_at("shell", 5);
        buffer.shift_to(buffer.prev(buffer.caret()));
        buffer.shift_to(buffer.prev(buffer.caret()));
        assert_eq!((buffer.selection.clone(), buffer.caret()), (3..5, 3));
        buffer.shift_to(buffer.next(buffer.caret()));
        buffer.shift_to(buffer.next(buffer.caret()));
        assert_eq!((buffer.selection.clone(), buffer.caret()), (5..5, 5));
        buffer.left();
        assert_eq!(buffer.caret(), 4);
    }

    #[test]
    fn shift_moves_should_shrink_a_selection_from_its_free_end() {
        let mut buffer = Buffer::new("shell");
        let to = buffer.prev(buffer.caret());
        buffer.shift_to(to);
        assert_eq!(buffer.selection, 0..4);
    }

    #[test]
    fn double_click_should_select_the_word_when_on_one() {
        let mut buffer = caret_at("foo bar-baz", 5);
        buffer.select_word(5);
        assert_eq!(&buffer.text[buffer.selection.clone()], "bar");
        let mut buffer = caret_at("a  b", 2);
        buffer.select_word(2);
        assert_eq!(buffer.selection, 2..2);
    }

    #[test]
    fn preedit_should_be_marked_then_committed() {
        let mut buffer = caret_at("x", 1);
        buffer.mark(None, "zhong", None);
        assert_eq!(buffer.marked, Some(1..6));
        buffer.replace(None, "中");
        assert_eq!((buffer.text.as_str(), buffer.marked), ("x中", None));
    }

    #[gpui::test]
    fn blur_should_revert_preedit_when_marked(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (field, cx) = cx.add_window_view(|window, cx| TextField::new("ab", window, cx));
        field.update(cx, |field, cx| {
            field.buffer.move_to(2);
            field.buffer.replace(None, "c");
            field.buffer.mark(None, "zhong", None);
            assert_eq!(field.buffer.text, "abczhong");
            field.blurred(cx);
            assert_eq!(field.buffer.text, "abc");
            assert_eq!(field.buffer.marked, None);
        });
    }
}
