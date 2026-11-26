//! Soft-keyboard bridge.
//!
//! android-activity (GameActivity) owns a text buffer that the IME edits. We
//! keep a *window* of the focused GPUI document mirrored into that buffer:
//! when GPUI's text or selection changes we push a window of it to Android,
//! and when Android reports an edit we diff it against the mirror and turn the
//! change into `replace_text_in_range` / `replace_and_mark_text_in_range`.

use android_activity::input::{TextInputState, TextSpan};
use gpui::PlatformInputHandler;
use std::ops::Range;

/// UTF-16 code units of context mirrored on each side of the selection.
const CONTEXT: usize = 128;

#[derive(Default)]
pub(crate) struct ImeMirror {
    /// Text window as last sent to / received from Android.
    pub text: String,
    /// UTF-16 offset of `text` within the GPUI document.
    pub window_start: usize,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Android reports spans as byte offsets into the UTF-8 text.
fn byte_to_utf16(s: &str, byte: usize) -> usize {
    let byte = byte.min(s.len());
    let byte = (0..=byte).rev().find(|b| s.is_char_boundary(*b)).unwrap_or(0);
    utf16_len(&s[..byte])
}

fn utf16_to_byte(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= utf16 {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

impl ImeMirror {
    /// Builds the Android-side state from the document around the selection.
    pub fn snapshot(&mut self, handler: &mut PlatformInputHandler) -> Option<TextInputState> {
        let selection = handler.selected_text_range(true)?;
        let start = selection.range.start.saturating_sub(CONTEXT);
        let end = selection.range.end + CONTEXT;
        let mut adjusted = None;
        let text = handler.text_for_range(start..end, &mut adjusted).unwrap_or_default();
        let window_start = adjusted.map_or(start, |r| r.start);
        let rel_start = selection.range.start.saturating_sub(window_start);
        let rel_end = selection.range.end.saturating_sub(window_start);
        let span = |utf16: usize| utf16_to_byte(&text, utf16);
        let compose_region = handler.marked_text_range().map(|m| TextSpan {
            start: span(m.start.saturating_sub(window_start)),
            end: span(m.end.saturating_sub(window_start)),
        });
        let state = TextInputState {
            selection: TextSpan { start: span(rel_start), end: span(rel_end) },
            compose_region,
            text: text.clone(),
        };
        self.text = text;
        self.window_start = window_start;
        Some(state)
    }

    /// Applies an edit made by the IME to the GPUI document.
    pub fn apply(&mut self, handler: &mut PlatformInputHandler, new: &TextInputState) {
        let old = self.text.as_str();
        let new_text = new.text.as_str();

        // Longest common prefix/suffix (on char boundaries) bounds the edit.
        let mut prefix = old
            .bytes()
            .zip(new_text.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while prefix > 0 && !(old.is_char_boundary(prefix) && new_text.is_char_boundary(prefix)) {
            prefix -= 1;
        }
        let max_suffix = old.len().min(new_text.len()) - prefix;
        let mut suffix = old
            .bytes()
            .rev()
            .zip(new_text.bytes().rev())
            .take(max_suffix)
            .take_while(|(a, b)| a == b)
            .count();
        while suffix > 0
            && !(old.is_char_boundary(old.len() - suffix)
                && new_text.is_char_boundary(new_text.len() - suffix))
        {
            suffix -= 1;
        }

        let removed = &old[prefix..old.len() - suffix];
        let inserted = &new_text[prefix..new_text.len() - suffix];
        let range_start = self.window_start + utf16_len(&old[..prefix]);
        let range: Range<usize> = range_start..range_start + utf16_len(removed);

        let composing = new.compose_region.filter(|c| c.end > c.start);
        if !removed.is_empty() || !inserted.is_empty() {
            if composing.is_some() {
                handler.replace_and_mark_text_in_range(Some(range), inserted, None);
            } else {
                handler.replace_text_in_range(Some(range), inserted);
            }
        } else if composing.is_none() && handler.marked_text_range().is_some() {
            handler.unmark_text();
        }
        self.text = new.text.clone();
        let _ = byte_to_utf16; // kept for selection-only updates (TODO)
    }
}
