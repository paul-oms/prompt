//! The composer's text engine.
//!
//! This is deliberately UI-agnostic: it deals in byte offsets that always land
//! on grapheme-cluster boundaries and exposes the operations a text editor
//! needs (selection, word motion, undo, click positioning). The renderer maps
//! byte offsets to pixels.

use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

const TAB_WIDTH: usize = 4;
const UNDO_LIMIT: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Other,
}

#[derive(Clone, Debug)]
struct Snapshot {
    text: String,
    caret: usize,
    anchor: Option<usize>,
}

#[derive(Default)]
pub struct Editor {
    text: String,
    caret: usize,
    anchor: Option<usize>,
    preferred_col: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<(Instant, EditKind)>,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    #[allow(dead_code)]
    pub fn anchor(&self) -> Option<usize> {
        self.anchor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn clear(&mut self) {
        if self.text.is_empty() {
            self.anchor = None;
            return;
        }
        self.begin_edit(EditKind::Other);
        self.text.clear();
        self.caret = 0;
        self.anchor = None;
        self.preferred_col = None;
    }

    pub fn set_text(&mut self, text: &str) {
        self.begin_edit(EditKind::Other);
        self.text = text.to_string();
        self.caret = self.text.len();
        self.anchor = None;
        self.preferred_col = None;
    }

    pub fn set_caret(&mut self, caret: usize) {
        self.caret = self.snap(caret);
        self.anchor = None;
        self.preferred_col = None;
    }

    /// Replace a byte range with `text` (used by completion). The range must
    /// land on grapheme boundaries; `snap` clamps it defensively.
    pub fn replace_range(&mut self, start: usize, end: usize, text: &str) {
        let start = self.snap(start);
        let end = self.snap(end).max(start);
        self.begin_edit(EditKind::Other);
        self.text.replace_range(start..end, text);
        self.caret = start + text.len();
        self.anchor = None;
        self.preferred_col = None;
    }

    // ---- selection -----------------------------------------------------

    /// An ordered byte range, or None when nothing is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.caret {
            return None;
        }
        Some(if anchor < self.caret {
            (anchor, self.caret)
        } else {
            (self.caret, anchor)
        })
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    pub fn selected_text(&self) -> Option<&str> {
        let (s, e) = self.selection()?;
        Some(&self.text[s..e])
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.len();
        self.preferred_col = None;
    }

    #[allow(dead_code)]
    pub fn select_range(&mut self, start: usize, end: usize) {
        let start = self.snap(start);
        let end = self.snap(end);
        self.anchor = Some(start);
        self.caret = end;
        self.preferred_col = None;
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    // ---- editing -------------------------------------------------------

    /// Insert text at the caret (replacing the selection). Newlines are
    /// supported; callers normalise CRLF first.
    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.begin_edit(EditKind::Insert);
        self.delete_selection_inner();
        self.text.insert_str(self.caret, text);
        self.caret += text.len();
        self.anchor = None;
        self.preferred_col = None;
    }

    pub fn newline(&mut self) {
        self.insert("\n");
    }

    pub fn backspace(&mut self) {
        if self.has_selection() {
            self.begin_edit(EditKind::Delete);
            self.delete_selection_inner();
            self.preferred_col = None;
            return;
        }
        if self.caret == 0 {
            return;
        }
        let prev = self.prev_boundary(self.caret);
        self.begin_edit(EditKind::Delete);
        self.text.replace_range(prev..self.caret, "");
        self.caret = prev;
        self.preferred_col = None;
    }

    pub fn delete_forward(&mut self) {
        if self.has_selection() {
            self.begin_edit(EditKind::Delete);
            self.delete_selection_inner();
            self.preferred_col = None;
            return;
        }
        if self.caret >= self.text.len() {
            return;
        }
        let next = self.next_boundary(self.caret);
        self.begin_edit(EditKind::Delete);
        self.text.replace_range(self.caret..next, "");
        self.preferred_col = None;
    }

    pub fn delete_word_back(&mut self) {
        if self.has_selection() {
            self.begin_edit(EditKind::Delete);
            self.delete_selection_inner();
            return;
        }
        let start = self.word_left(self.caret);
        if start == self.caret {
            return;
        }
        self.begin_edit(EditKind::Delete);
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        self.preferred_col = None;
    }

    pub fn delete_word_forward(&mut self) {
        if self.has_selection() {
            self.begin_edit(EditKind::Delete);
            self.delete_selection_inner();
            return;
        }
        let end = self.word_right(self.caret);
        if end == self.caret {
            return;
        }
        self.begin_edit(EditKind::Delete);
        self.text.replace_range(self.caret..end, "");
        self.preferred_col = None;
    }

    /// Delete from the caret to the start/end of the current logical line,
    /// like Cmd+Backspace / Cmd+Delete in an editor (Ctrl on Linux is mapped by
    /// the app to word deletes, so this is used for the line variants).
    pub fn delete_to_line_start(&mut self) {
        if self.has_selection() {
            self.begin_edit(EditKind::Delete);
            self.delete_selection_inner();
            return;
        }
        let start = self.line_start(self.caret);
        if start == self.caret {
            return;
        }
        self.begin_edit(EditKind::Delete);
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        self.preferred_col = None;
    }

    pub fn delete_to_line_end(&mut self) {
        if self.has_selection() {
            self.begin_edit(EditKind::Delete);
            self.delete_selection_inner();
            return;
        }
        let end = self.line_end(self.caret);
        if end == self.caret {
            return;
        }
        self.begin_edit(EditKind::Delete);
        self.text.replace_range(self.caret..end, "");
        self.preferred_col = None;
    }

    fn delete_selection_inner(&mut self) {
        if let Some((s, e)) = self.selection() {
            self.text.replace_range(s..e, "");
            self.caret = s;
            self.anchor = None;
        }
    }

    // ---- undo ----------------------------------------------------------

    fn begin_edit(&mut self, kind: EditKind) {
        let now = Instant::now();
        let coalesce = matches!(kind, EditKind::Insert | EditKind::Delete)
            && self
                .last_edit
                .map(|(at, prev)| {
                    prev == kind && now.duration_since(at) < Duration::from_millis(600)
                })
                .unwrap_or(false);
        if !coalesce {
            self.undo.push(Snapshot {
                text: self.text.clone(),
                caret: self.caret,
                anchor: self.anchor,
            });
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last_edit = Some((now, kind));
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(Snapshot {
            text: self.text.clone(),
            caret: self.caret,
            anchor: self.anchor,
        });
        self.restore(prev);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(Snapshot {
            text: self.text.clone(),
            caret: self.caret,
            anchor: self.anchor,
        });
        self.restore(next);
        true
    }

    fn restore(&mut self, snap: Snapshot) {
        self.text = snap.text;
        self.caret = self.snap(snap.caret);
        self.anchor = snap.anchor.map(|a| self.snap(a));
        self.preferred_col = None;
        self.last_edit = None;
    }

    #[allow(dead_code)]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // ---- motion --------------------------------------------------------

    pub fn move_left(&mut self, extend: bool) {
        if !extend && self.has_selection() {
            self.caret = self.selection().unwrap().0;
            self.anchor = None;
        } else {
            self.set_caret_extend(self.prev_boundary(self.caret), extend);
        }
        self.preferred_col = None;
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend && self.has_selection() {
            self.caret = self.selection().unwrap().1;
            self.anchor = None;
        } else {
            self.set_caret_extend(self.next_boundary(self.caret), extend);
        }
        self.preferred_col = None;
    }

    pub fn move_word_left(&mut self, extend: bool) {
        let target = self.word_left(self.caret);
        self.set_caret_extend(target, extend);
        self.preferred_col = None;
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let target = self.word_right(self.caret);
        self.set_caret_extend(target, extend);
        self.preferred_col = None;
    }

    pub fn move_up(&mut self, extend: bool) {
        let col = self
            .preferred_col
            .unwrap_or_else(|| self.display_col_at(self.caret));
        let Some((line, _)) = self.line_of(self.caret) else {
            return;
        };
        if line == 0 {
            self.set_caret_extend(0, extend);
            self.preferred_col = Some(col);
            return;
        }
        let start = self.line_start_of(line - 1);
        let end = self.line_end_of(line - 1);
        let target = self.index_at_col(start, end, col);
        self.set_caret_extend(target, extend);
        self.preferred_col = Some(col);
    }

    pub fn move_down(&mut self, extend: bool) {
        let col = self
            .preferred_col
            .unwrap_or_else(|| self.display_col_at(self.caret));
        let Some((line, _)) = self.line_of(self.caret) else {
            return;
        };
        if line + 1 >= self.line_count() {
            let end = self.text.len();
            self.set_caret_extend(end, extend);
            self.preferred_col = Some(col);
            return;
        }
        let start = self.line_start_of(line + 1);
        let end = self.line_end_of(line + 1);
        let target = self.index_at_col(start, end, col);
        self.set_caret_extend(target, extend);
        self.preferred_col = Some(col);
    }

    /// Smart Home: first press goes just after indentation, second to column 0.
    pub fn move_line_start(&mut self, extend: bool) {
        let start = self.line_start(self.caret);
        let end = self.line_end(self.caret);
        let mut first_non_ws = start;
        for (i, g) in self.text[start..end].grapheme_indices(true) {
            if g.chars().all(char::is_whitespace) {
                first_non_ws = start + i + g.len();
            } else {
                break;
            }
        }
        let target = if self.caret > first_non_ws {
            first_non_ws
        } else if self.caret == start && first_non_ws != start {
            first_non_ws
        } else {
            start
        };
        self.set_caret_extend(target, extend);
        self.preferred_col = None;
    }

    pub fn move_line_end(&mut self, extend: bool) {
        let end = self.line_end(self.caret);
        self.set_caret_extend(end, extend);
        self.preferred_col = None;
    }

    pub fn move_buffer_start(&mut self, extend: bool) {
        self.set_caret_extend(0, extend);
        self.preferred_col = None;
    }

    pub fn move_buffer_end(&mut self, extend: bool) {
        let end = self.text.len();
        self.set_caret_extend(end, extend);
        self.preferred_col = None;
    }

    fn set_caret_extend(&mut self, target: usize, extend: bool) {
        let target = self.snap(target);
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
            self.caret = target;
            if self.anchor == Some(self.caret) {
                self.anchor = None;
            }
        } else {
            self.caret = target;
            self.anchor = None;
        }
    }

    // ---- click / word selection ---------------------------------------

    pub fn select_word_at(&mut self, index: usize) {
        let index = self.snap(index);
        let mut start = index;
        let mut end = index;
        if index < self.text.len() {
            let class = self.class_at(index);
            while start > 0 && self.class_before(start) == class {
                start = self.prev_boundary(start);
            }
            while end < self.text.len() && self.class_at(end) == class {
                end = self.next_boundary(end);
            }
        }
        self.anchor = Some(start);
        self.caret = end;
        self.preferred_col = None;
    }

    pub fn select_line_at(&mut self, index: usize) {
        let index = self.snap(index);
        let start = self.line_start(index);
        let mut end = self.line_end(index);
        if end < self.text.len() && self.text[end..].starts_with('\n') {
            end += 1;
        }
        self.anchor = Some(start);
        self.caret = end;
    }

    /// Byte index for a click at `col` display cells into logical line `line`.
    pub fn index_at_line_col(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.line_count().saturating_sub(1));
        let start = self.line_start_of(line);
        let end = self.line_end_of(line);
        self.index_at_col(start, end, col)
    }

    pub fn index_for_line_col(&mut self, line: usize, col: usize, extend: bool) {
        let index = self.index_at_line_col(line, col);
        self.set_caret_extend(index, extend);
        self.preferred_col = None;
    }

    pub fn drag_to_index(&mut self, index: usize) {
        if self.anchor.is_none() {
            self.anchor = Some(self.caret);
        }
        self.caret = self.snap(index);
    }

    // ---- geometry ------------------------------------------------------

    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    pub fn line_of(&self, index: usize) -> Option<(usize, usize)> {
        let index = index.min(self.text.len());
        let start = self.line_start(index);
        Some((self.text[..start].matches('\n').count(), start))
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_start_of(line);
        let end = self.line_end_of(line);
        &self.text[start..end]
    }

    pub fn line_start_of(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        self.text
            .match_indices('\n')
            .nth(line - 1)
            .map(|(i, _)| i + 1)
            .unwrap_or(self.text.len())
    }

    pub fn line_end_of(&self, line: usize) -> usize {
        let start = self.line_start_of(line);
        self.text[start..]
            .find('\n')
            .map(|i| start + i)
            .unwrap_or(self.text.len())
    }

    pub fn line_start(&self, index: usize) -> usize {
        self.text[..index.min(self.text.len())]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    pub fn line_end(&self, index: usize) -> usize {
        let index = index.min(self.text.len());
        self.text[index..]
            .find('\n')
            .map(|i| index + i)
            .unwrap_or(self.text.len())
    }

    /// (line index, display column) of the caret.
    pub fn caret_line_col(&self) -> (usize, usize) {
        let start = self.line_start(self.caret);
        let line = self.text[..start].matches('\n').count();
        (line, self.display_width(&self.text[start..self.caret]))
    }

    pub fn display_width(&self, text: &str) -> usize {
        text.graphemes(true).map(grapheme_width).sum()
    }

    fn display_col_at(&self, index: usize) -> usize {
        let start = self.line_start(index);
        self.display_width(&self.text[start..index])
    }

    fn index_at_col(&self, start: usize, end: usize, col: usize) -> usize {
        let mut used = 0;
        for (i, g) in self.text[start..end].grapheme_indices(true) {
            let w = grapheme_width(g);
            if used + w > col {
                return start + i;
            }
            used += w;
            if used == col {
                // If the next grapheme is zero-width (combining), stay before
                // it so the caret is never inside a cluster.
                return start + i + g.len();
            }
        }
        end
    }

    // ---- helpers -------------------------------------------------------

    fn snap(&self, mut index: usize) -> usize {
        index = index.min(self.text.len());
        while index > 0 && !self.text.is_char_boundary(index) {
            index -= 1;
        }
        index
    }

    fn prev_boundary(&self, index: usize) -> usize {
        let index = self.snap(index);
        if index == 0 {
            return 0;
        }
        self.text[..index]
            .grapheme_indices(true)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_boundary(&self, index: usize) -> usize {
        let index = self.snap(index);
        if index >= self.text.len() {
            return self.text.len();
        }
        self.text[index..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| index + i)
            .unwrap_or(self.text.len())
    }

    fn word_left(&self, index: usize) -> usize {
        let mut i = self.snap(index);
        while i > 0 && self.class_before(i) == CharClass::Space {
            i = self.prev_boundary(i);
        }
        if i == 0 {
            return 0;
        }
        let class = self.class_before(i);
        while i > 0 && self.class_before(i) == class {
            i = self.prev_boundary(i);
        }
        i
    }

    fn word_right(&self, index: usize) -> usize {
        let mut i = self.snap(index);
        let len = self.text.len();
        while i < len && self.class_at(i) == CharClass::Space {
            i = self.next_boundary(i);
        }
        if i >= len {
            return len;
        }
        let class = self.class_at(i);
        while i < len && self.class_at(i) == class {
            i = self.next_boundary(i);
        }
        i
    }

    fn class_before(&self, index: usize) -> CharClass {
        if index == 0 {
            return CharClass::Space;
        }
        self.class_at(self.prev_boundary(index))
    }

    fn class_at(&self, index: usize) -> CharClass {
        if index >= self.text.len() {
            return CharClass::Space;
        }
        let g = self.text[index..].graphemes(true).next().unwrap_or("");
        char_class(g)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Symbol,
}

fn char_class(grapheme: &str) -> CharClass {
    let mut chars = grapheme.chars();
    let Some(c) = chars.next() else {
        return CharClass::Space;
    };
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~') {
        CharClass::Word
    } else {
        CharClass::Symbol
    }
}

fn grapheme_width(g: &str) -> usize {
    let mut width = 0;
    for c in g.chars() {
        if c == '\t' {
            width += TAB_WIDTH;
        } else {
            width += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    width.max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut e = Editor::default();
        e.set_text(text);
        e
    }

    #[test]
    fn insert_and_move() {
        let mut e = Editor::default();
        e.insert("hello");
        e.insert(" world");
        assert_eq!(e.text(), "hello world");
        e.move_left(false);
        assert_eq!(e.caret(), 10);
        e.insert("!");
        assert_eq!(e.text(), "hello worl!d");
    }

    #[test]
    fn unicode_graphemes_are_atomic() {
        let emoji = "👩‍💻";
        let mut e = editor(&format!("a{emoji}b"));
        e.move_buffer_start(false);
        e.move_right(false);
        assert_eq!(e.caret(), 1);
        e.move_right(false);
        assert_eq!(e.caret(), 1 + emoji.len());
        e.backspace();
        assert_eq!(e.text(), "ab");
    }

    #[test]
    fn shift_selection_and_replace() {
        let mut e = editor("hello world");
        e.move_buffer_start(false);
        for _ in 0..5 {
            e.move_right(true);
        }
        assert_eq!(e.selected_text(), Some("hello"));
        e.insert("goodbye");
        assert_eq!(e.text(), "goodbye world");
        assert!(!e.has_selection());
    }

    #[test]
    fn selection_collapse_left_and_right() {
        let mut e = editor("abcd");
        e.move_buffer_start(false);
        e.move_right(true);
        e.move_right(true);
        assert_eq!(e.selected_text(), Some("ab"));
        e.move_right(false);
        assert_eq!(e.caret(), 2);
        assert_eq!(e.text(), "abcd");

        e.select_range(1, 3);
        e.move_left(false);
        assert_eq!(e.caret(), 1);
        assert!(!e.has_selection());
    }

    #[test]
    fn word_motion() {
        let mut e = editor("git commit -m 'hi there'");
        e.move_buffer_start(false);
        e.move_word_right(false);
        assert_eq!(e.caret(), 3);
        e.move_word_right(false);
        assert_eq!(e.caret(), 10);
        e.move_word_right(false);
        assert_eq!(e.caret(), 13);
        e.move_word_left(false);
        assert_eq!(e.caret(), 11);
        e.move_word_left(false);
        assert_eq!(e.caret(), 4);
    }

    #[test]
    fn delete_word_back() {
        let mut e = editor("cargo build --release");
        e.delete_word_back();
        assert_eq!(e.text(), "cargo build ");
        e.delete_word_back();
        assert_eq!(e.text(), "cargo ");
        e.delete_word_back();
        assert_eq!(e.text(), "");
    }

    #[test]
    fn line_navigation_and_multiline() {
        let mut e = editor("first line\nsecond line\nthird");
        e.move_buffer_start(false);
        e.move_down(false);
        assert_eq!(e.caret(), 11);
        e.move_line_end(false);
        assert_eq!(e.caret(), 22);
        e.move_line_start(false);
        assert_eq!(e.caret(), 11);
        e.move_up(false);
        assert_eq!(e.caret(), 0);
        e.move_down(false);
        assert_eq!(e.caret(), 11);
        e.move_buffer_end(false);
        e.move_up(false);
        assert_eq!(e.caret(), 16);
    }

    #[test]
    fn vertical_motion_keeps_preferred_column() {
        let mut e = editor("abcdef\nx\nabcdef");
        e.move_buffer_start(false);
        for _ in 0..4 {
            e.move_right(false);
        }
        e.move_down(false);
        assert_eq!(e.caret(), 8); // end of short line
        e.move_down(false);
        assert_eq!(e.caret(), 13); // column 4 restored
    }

    #[test]
    fn newline_and_line_bounds() {
        let mut e = editor("ab");
        e.move_buffer_start(false);
        e.move_right(false);
        e.newline();
        assert_eq!(e.text(), "a\nb");
        assert_eq!(e.line_count(), 2);
        assert_eq!(e.line_text(0), "a");
        assert_eq!(e.line_text(1), "b");
        assert_eq!(e.caret_line_col(), (1, 0));
    }

    #[test]
    fn trailing_newline_is_a_line() {
        let e = editor("a\n");
        assert_eq!(e.line_count(), 2);
        assert_eq!(e.line_text(1), "");
    }

    #[test]
    fn undo_redo_coalesces_typing() {
        let mut e = Editor::default();
        e.insert("a");
        e.insert("b");
        e.insert("c");
        assert_eq!(e.text(), "abc");
        assert!(e.undo());
        assert_eq!(e.text(), "");
        assert!(e.redo());
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn undo_cursor_position() {
        let mut e = editor("one");
        e.move_buffer_end(false);
        e.insert(" two");
        assert_eq!(e.caret(), 7);
        e.undo();
        assert_eq!(e.caret(), 3);
        assert_eq!(e.text(), "one");
    }

    #[test]
    fn smart_home() {
        let mut e = editor("    cargo test");
        e.move_buffer_end(false);
        e.move_line_start(false);
        assert_eq!(e.caret(), 4);
        e.move_line_start(false);
        assert_eq!(e.caret(), 0);
        e.move_line_start(false);
        assert_eq!(e.caret(), 4);
    }

    #[test]
    fn click_positioning() {
        let mut e = editor("hello\nworld");
        e.index_for_line_col(1, 2, false);
        assert_eq!(e.caret(), 8);
        e.index_for_line_col(0, 99, false);
        assert_eq!(e.caret(), 5);
        e.index_for_line_col(1, 0, true);
        assert_eq!(e.selected_text(), Some("\n"));
    }

    #[test]
    fn select_word_and_line() {
        let mut e = editor("echo hello-world");
        e.select_word_at(6);
        assert_eq!(e.selected_text(), Some("hello-world"));
        let mut e = editor("one\ntwo\nthree");
        e.select_line_at(5);
        assert_eq!(e.selected_text(), Some("two\n"));
    }

    #[test]
    fn double_click_whitespace_selects_run() {
        let mut e = editor("a   b");
        e.select_word_at(2);
        assert_eq!(e.selected_text(), Some("   "));
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let e = editor("日本");
        assert_eq!(e.display_width("日本"), 4);
        let mut e2 = editor("日本語x");
        e2.index_for_line_col(0, 4, false);
        assert_eq!(e2.caret(), "日本".len());
    }

    #[test]
    fn delete_to_line_bounds() {
        let mut e = editor("alpha beta\ngamma");
        e.set_caret(10);
        e.delete_to_line_start();
        assert_eq!(e.text(), "\ngamma");
        assert_eq!(e.caret(), 0);
        e.move_line_end(false);
        e.delete_to_line_end();
        assert_eq!(e.text(), "\ngamma");
    }

    #[test]
    fn typing_after_undo_clears_redo() {
        let mut e = editor("abc");
        e.insert("d");
        e.undo();
        e.insert("x");
        assert!(!e.can_redo());
        assert_eq!(e.text(), "abcx");
    }
}
