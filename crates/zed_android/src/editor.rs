//! A touch-friendly multi-line code editor built on GPUI primitives.

use gpui::{prelude::*, *};
use std::{ops::Range, path::PathBuf};

pub const GUTTER_WIDTH: f32 = 48.0;
pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 20.0;
pub const MONO_FONT: &str = "Lilex";
const TAB: &str = "    ";

pub struct Editor {
    pub focus_handle: FocusHandle,
    pub text: String,
    /// Selection in bytes; the caret is `end` unless `reversed`.
    pub selection: Range<usize>,
    pub reversed: bool,
    pub marked: Option<Range<usize>>,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub ctrl_sticky: bool,
    scroll: ScrollHandle,
    anchor_bounds: Option<Bounds<Pixels>>,
    char_width: Pixels,
    goal_col: Option<usize>,
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Editor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            text: String::new(),
            selection: 0..0,
            reversed: false,
            marked: None,
            path: None,
            dirty: false,
            ctrl_sticky: false,
            scroll: ScrollHandle::new(),
            anchor_bounds: None,
            char_width: px(8.4),
            goal_col: None,
        }
    }

    pub fn set_contents(&mut self, text: String, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.text = text;
        self.path = path;
        self.selection = 0..0;
        self.reversed = false;
        self.marked = None;
        self.dirty = false;
        self.goal_col = None;
        cx.notify();
    }

    pub fn save(&mut self, cx: &mut Context<Self>) -> std::io::Result<()> {
        if let Some(path) = &self.path {
            std::fs::write(path, &self.text)?;
            self.dirty = false;
            cx.notify();
        }
        Ok(())
    }

    pub fn cursor(&self) -> usize {
        if self.reversed { self.selection.start } else { self.selection.end }
    }

    // ---- text geometry ----

    fn line_start(&self, offset: usize) -> usize {
        self.text[..offset].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.text[offset..].find('\n').map_or(self.text.len(), |i| offset + i)
    }

    fn line_col(&self, offset: usize) -> (usize, usize) {
        let before = &self.text[..offset];
        let line = before.bytes().filter(|b| *b == b'\n').count();
        let col = before[self.line_start(offset)..].chars().count();
        (line, col)
    }

    fn offset_for(&self, line: usize, col: usize) -> usize {
        let mut start = 0;
        for _ in 0..line {
            match self.text[start..].find('\n') {
                Some(i) => start += i + 1,
                None => return self.text.len(),
            }
        }
        let end = self.line_end(start);
        self.text[start..end]
            .char_indices()
            .nth(col)
            .map_or(end, |(i, _)| start + i)
    }

    fn line_count(&self) -> usize {
        self.text.bytes().filter(|b| *b == b'\n').count() + 1
    }

    fn prev_boundary(&self, offset: usize) -> usize {
        self.text[..offset].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text[offset..].chars().next().map_or(self.text.len(), |c| offset + c.len_utf8())
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())].encode_utf16().count()
    }

    fn offset_from_utf16(&self, target: usize) -> usize {
        let mut units = 0;
        for (i, c) in self.text.char_indices() {
            if units >= target {
                return i;
            }
            units += c.len_utf16();
        }
        self.text.len()
    }

    fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }

    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)
    }

    // ---- editing ----

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = true;
        self.goal_col = None;
        window.notify_text_input_changed(&self.focus_handle);
        self.reveal_cursor();
        cx.notify();
    }

    fn reveal_cursor(&self) {
        let (line, _) = self.line_col(self.cursor());
        self.scroll.scroll_to_item(line);
    }

    pub fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let range = self.selection.clone();
        self.text.replace_range(range.clone(), text);
        let at = range.start + text.len();
        self.selection = at..at;
        self.reversed = false;
        self.marked = None;
        self.changed(window, cx);
    }

    fn delete_range(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if range.is_empty() {
            return;
        }
        self.text.replace_range(range.clone(), "");
        self.selection = range.start..range.start;
        self.reversed = false;
        self.marked = None;
        self.changed(window, cx);
    }

    fn move_to(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        let offset = offset.min(self.text.len());
        if extend {
            let anchor = if self.reversed { self.selection.end } else { self.selection.start };
            self.reversed = offset < anchor;
            self.selection = anchor.min(offset)..anchor.max(offset);
        } else {
            self.selection = offset..offset;
            self.reversed = false;
        }
        self.marked = None;
        self.reveal_cursor();
        cx.notify();
    }

    /// Handles a named key (from hardware keys or the on-screen toolbar).
    /// Returns whether the key was consumed.
    pub fn handle_key(
        &mut self,
        key: &str,
        ctrl: bool,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let cursor = self.cursor();
        let keep_goal = matches!(key, "up" | "down" | "pageup" | "pagedown");
        let goal = self.goal_col;
        let handled = match (key, ctrl) {
            ("left", _) => {
                let to = if !self.selection.is_empty() && !shift {
                    self.selection.start
                } else {
                    self.prev_boundary(cursor)
                };
                self.move_to(to, shift, cx);
                true
            }
            ("right", _) => {
                let to = if !self.selection.is_empty() && !shift {
                    self.selection.end
                } else {
                    self.next_boundary(cursor)
                };
                self.move_to(to, shift, cx);
                true
            }
            ("up", _) | ("down", _) | ("pageup", _) | ("pagedown", _) => {
                let (line, col) = self.line_col(cursor);
                let col = goal.unwrap_or(col);
                let step = if key.starts_with("page") { 20 } else { 1 };
                let target = if key == "up" || key == "pageup" {
                    line.saturating_sub(step)
                } else {
                    (line + step).min(self.line_count() - 1)
                };
                let to = self.offset_for(target, col);
                self.move_to(to, shift, cx);
                self.goal_col = Some(col);
                true
            }
            ("home", _) => {
                let to = self.line_start(cursor);
                self.move_to(to, shift, cx);
                true
            }
            ("end", _) => {
                let to = self.line_end(cursor);
                self.move_to(to, shift, cx);
                true
            }
            ("enter", _) => {
                let start = self.line_start(self.selection.start);
                let line_to_cursor = &self.text[start..self.selection.start];
                let indent: String =
                    line_to_cursor.chars().take_while(|c| *c == ' ').collect();
                let extra = if line_to_cursor.trim_end().ends_with(['{', '(', '[']) { TAB } else { "" };
                let text = format!("\n{indent}{extra}");
                self.insert(&text, window, cx);
                true
            }
            ("backspace", _) => {
                let range = if self.selection.is_empty() {
                    // Delete a whole indent unit when only spaces precede the caret.
                    let start = self.line_start(cursor);
                    let before = &self.text[start..cursor];
                    if !before.is_empty() && before.chars().all(|c| c == ' ') && before.len() % 4 == 0 {
                        cursor - 4..cursor
                    } else {
                        self.prev_boundary(cursor)..cursor
                    }
                } else {
                    self.selection.clone()
                };
                self.delete_range(range, window, cx);
                true
            }
            ("delete", _) => {
                let range = if self.selection.is_empty() {
                    cursor..self.next_boundary(cursor)
                } else {
                    self.selection.clone()
                };
                self.delete_range(range, window, cx);
                true
            }
            ("tab", false) => {
                self.insert(TAB, window, cx);
                true
            }
            ("s", true) => {
                if let Err(error) = self.save(cx) {
                    log::error!("save failed: {error}");
                }
                true
            }
            ("a", true) => {
                self.selection = 0..self.text.len();
                self.reversed = false;
                cx.notify();
                true
            }
            ("c", true) | ("x", true) => {
                if !self.selection.is_empty() {
                    let selected = self.text[self.selection.clone()].to_string();
                    cx.write_to_clipboard(ClipboardItem::new_string(selected));
                    if key == "x" {
                        self.delete_range(self.selection.clone(), window, cx);
                    }
                }
                true
            }
            ("v", true) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.insert(&text, window, cx);
                }
                true
            }
            _ => false,
        };
        if handled && !keep_goal {
            self.goal_col = None;
        }
        handled
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let ctrl = ks.modifiers.control || self.ctrl_sticky;
        if self.handle_key(ks.key.as_str(), ctrl, ks.modifiers.shift, window, cx) {
            self.ctrl_sticky = false;
            cx.stop_propagation();
        }
    }

    fn tap(&mut self, line: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let left = self.anchor_bounds.map_or(Pixels::ZERO, |b| b.left());
        let x = f32::from(position.x - left) - GUTTER_WIDTH;
        let col = (x / f32::from(self.char_width)).round().max(0.0) as usize;
        let to = self.offset_for(line, col);
        self.move_to(to, false, cx);
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.text[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selection), reversed: self.reversed })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        self.selection = range;
        self.insert(new_text, window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        self.text.replace_range(range.clone(), new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        let start_utf16 = self.offset_to_utf16(range.start);
        let inserted_utf16 = new_text.encode_utf16().count();
        self.selection = new_selected_range_utf16
            .map(|s| {
                self.range_from_utf16(
                    &(start_utf16 + s.start.min(inserted_utf16)..start_utf16 + s.end.min(inserted_utf16)),
                )
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.reversed = false;
        self.changed(window, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let offset = self.offset_from_utf16(range_utf16.start);
        let (line, col) = self.line_col(offset);
        Some(Bounds::new(
            point(
                element_bounds.left() + px(GUTTER_WIDTH) + self.char_width * col as f32,
                element_bounds.top() + px(LINE_HEIGHT) * line as f32,
            ),
            size(self.char_width, px(LINE_HEIGHT)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// Registers the editor as the window's text input target while painting.
struct ImeAnchor {
    editor: Entity<Editor>,
}

impl Element for ImeAnchor {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.position = Position::Absolute;
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        style.inset.top = px(0.).into();
        style.inset.left = px(0.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.editor.clone()), cx);
        self.editor.update(cx, |editor, _| editor.anchor_bounds = Some(bounds));
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font = gpui::font(MONO_FONT);
        let font_id = window.text_system().resolve_font(&font);
        if let Ok(advance) = window.text_system().advance(font_id, px(FONT_SIZE), 'm') {
            self.char_width = advance.width;
        }
        let cw = self.char_width;
        let focused = self.focus_handle.is_focused(window);
        let (cursor_line, cursor_col) = self.line_col(self.cursor());
        let selection_lines = (self.line_col(self.selection.start).0, self.line_col(self.selection.end).0);
        let has_selection = !self.selection.is_empty();

        let rows: Vec<_> = self
            .text
            .split('\n')
            .enumerate()
            .map(|(i, line)| {
                let in_selection = has_selection && i >= selection_lines.0 && i <= selection_lines.1;
                div()
                    .id(("line", i))
                    .h(px(LINE_HEIGHT))
                    .w_full()
                    .flex()
                    .flex_row()
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.tap(i, event.position(), window, cx)
                    }))
                    .child(
                        div()
                            .w(px(GUTTER_WIDTH))
                            .flex_none()
                            .pr_2()
                            .text_right()
                            .text_color(rgb(0x6b7280))
                            .child(SharedString::from((i + 1).to_string())),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .whitespace_nowrap()
                            .when(in_selection, |d| d.bg(rgba(0x3b82f655)))
                            .child(SharedString::from(line.replace('\t', TAB)))
                            .when(focused && i == cursor_line, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left(cw * cursor_col as f32)
                                        .w(px(2.))
                                        .h(px(LINE_HEIGHT))
                                        .bg(rgb(0x60a5fa)),
                                )
                            }),
                    )
            })
            .collect();

        div()
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .size_full()
            .font_family(MONO_FONT)
            .text_size(px(FONT_SIZE))
            .text_color(rgb(0xe5e7eb))
            .bg(rgb(0x111827))
            .child(
                div()
                    .id("editor-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .children(rows),
            )
            .child(ImeAnchor { editor: cx.entity() })
    }
}
