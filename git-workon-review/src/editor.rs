//! Multi-line text buffer for the annotation editor modal (ADR-039 slice 3) — the multi-line
//! counterpart to [`crate::prompt::PromptState`]'s single line. `tui.rs`'s modal key-handling
//! arm drives this the same way it drives [`crate::prompt::PromptState`]: it decodes the shared
//! readline subset through its own `prompt_edit_for_key`, then layers `Up`/`Down`/`Enter` on top
//! for multi-line motion (`Ctrl-s` submits, `Esc` cancels — both stay in `tui.rs`/`app.rs`, this
//! module owns no keyboard policy).
//!
//! [`Self::col`] is a BYTE offset into the CURRENT line, the same discipline
//! [`crate::prompt::PromptState::cursor`] uses and for the same reason — `String::insert`/
//! `remove`/slicing want byte offsets, so every edit stays a direct mutation with no index
//! translation; only screen placement ([`Self::cursor_screen_pos`]) needs a display column.

use unicode_width::UnicodeWidthChar;

use crate::wrap::wrap_text;

/// A multi-line editable buffer: one or more lines of text, a cursor (line index + byte-offset
/// column within it), and a scroll offset for a viewport shorter than the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorState {
    lines: Vec<String>,
    line: usize,
    col: usize,
    scroll: usize,
}

impl EditorState {
    /// A fresh editor: one empty line, cursor at its start — what every slice-3 authoring flow
    /// (create, reply) opens with; nothing seeds a draft yet.
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            line: 0,
            col: 0,
            scroll: 0,
        }
    }

    /// Seed the editor from existing text, cursor at the end — the natural counterpart to
    /// [`Self::text`], for a future edit-in-place verb this slice doesn't add.
    #[cfg(test)]
    pub fn from_text(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        let line = lines.len() - 1;
        let col = lines[line].len();
        Self {
            lines,
            line,
            col,
            scroll: 0,
        }
    }

    /// The buffer's lines, in source order — the render side reads this directly rather than
    /// re-deriving it from [`Self::text`] every frame.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The whole buffer, newline-joined — what a submit writes through the annotation store.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Whether the buffer holds anything worth confirming before a discard — a lone empty line
    /// is the "just opened, nothing typed yet" state.
    pub fn is_dirty(&self) -> bool {
        self.lines.len() > 1 || !self.lines[0].is_empty()
    }

    pub fn cursor_line(&self) -> usize {
        self.line
    }

    pub fn cursor_col(&self) -> usize {
        self.col
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    fn current(&self) -> &str {
        &self.lines[self.line]
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.current()[..self.col]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.current()[self.col..]
            .chars()
            .next()
            .map(|c| self.col + c.len_utf8())
    }

    /// Insert `c` at the cursor, then advance past it — same as
    /// [`crate::prompt::PromptState::insert_char`], over the current line only.
    pub fn insert_char(&mut self, c: char) {
        let col = self.col;
        self.lines[self.line].insert(col, c);
        self.col += c.len_utf8();
    }

    /// `Enter`: split the current line at the cursor into two, cursor landing at the start of
    /// the new (second) line. Plain-text authoring, no auto-indent — a comment body has no
    /// syntax to indent against.
    pub fn newline(&mut self) {
        let tail = self.lines[self.line].split_off(self.col);
        self.lines.insert(self.line + 1, tail);
        self.line += 1;
        self.col = 0;
    }

    /// `Backspace`: delete the char before the cursor. At column 0 this joins the current line
    /// onto the PREVIOUS one instead (the multi-line extra over
    /// [`crate::prompt::PromptState::backspace`], which has no line to join into) — a no-op only
    /// on the buffer's very first line.
    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.lines[self.line].drain(prev..self.col);
            self.col = prev;
            return;
        }
        if self.line == 0 {
            return;
        }
        let tail = self.lines.remove(self.line);
        self.line -= 1;
        self.col = self.lines[self.line].len();
        self.lines[self.line].push_str(&tail);
    }

    /// `Delete`: delete the char after the cursor. At the end of a line this joins the NEXT
    /// line onto this one — the multi-line mirror of [`Self::backspace`]'s join.
    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.lines[self.line].drain(self.col..next);
            return;
        }
        if self.line + 1 >= self.lines.len() {
            return;
        }
        let tail = self.lines.remove(self.line + 1);
        self.lines[self.line].push_str(&tail);
    }

    /// Move left one char; at column 0, wraps to the end of the previous line (a no-op on the
    /// buffer's first line).
    pub fn move_left(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.col = prev;
        } else if self.line > 0 {
            self.line -= 1;
            self.col = self.lines[self.line].len();
        }
    }

    /// Move right one char; past the last char, wraps to the start of the next line (a no-op on
    /// the buffer's last line).
    pub fn move_right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.col = next;
        } else if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.col = 0;
        }
    }

    /// `Up`: previous line, column clamped to its length so the cursor never lands mid a
    /// shorter line's missing tail. Readline has no analog for this — multi-line-specific.
    pub fn move_up(&mut self) {
        if self.line == 0 {
            return;
        }
        self.line -= 1;
        self.col = self.col.min(self.lines[self.line].len());
    }

    /// `Down`: mirror of [`Self::move_up`].
    pub fn move_down(&mut self) {
        if self.line + 1 >= self.lines.len() {
            return;
        }
        self.line += 1;
        self.col = self.col.min(self.lines[self.line].len());
    }

    /// `Ctrl-a`/`Home`: start of the CURRENT line (not the whole buffer — multi-line
    /// [`crate::prompt::PromptState::move_home`] has no "whole buffer" to distinguish from).
    pub fn move_home(&mut self) {
        self.col = 0;
    }

    /// `Ctrl-e`/`End`: end of the current line.
    pub fn move_end(&mut self) {
        self.col = self.lines[self.line].len();
    }

    /// `Ctrl-u`: delete from the start of the current line up to the cursor — same rule as
    /// [`crate::prompt::PromptState::clear_to_start`], scoped to one line.
    pub fn clear_to_start(&mut self) {
        self.lines[self.line].drain(..self.col);
        self.col = 0;
    }

    /// `Ctrl-w`: delete the "word" immediately before the cursor on the current line — same
    /// skip-trailing-whitespace-then-delete-the-word algorithm as
    /// [`crate::prompt::PromptState::delete_word_back`].
    pub fn delete_word_back(&mut self) {
        if self.col == 0 {
            return;
        }
        let before = &self.lines[self.line][..self.col];
        let mut end = self.col;
        let mut chars = before.char_indices().rev().peekable();
        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() {
                end = i;
                chars.next();
            } else {
                break;
            }
        }
        let mut start = end;
        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            start = i;
            chars.next();
        }
        self.lines[self.line].drain(start..self.col);
        self.col = start;
    }

    /// The buffer wrapped to `width` display columns ([`wrap_text`]), one wrap call per SOURCE
    /// line — a wrap must never merge two authored lines into one; the editor's own `\n`s are
    /// paragraph breaks, not fill text `wrap_text` is free to re-flow across.
    pub fn wrapped_lines(&self, width: usize) -> Vec<String> {
        self.lines
            .iter()
            .flat_map(|l| wrap_text(l, width))
            .collect()
    }

    /// Where the cursor lands in [`Self::wrapped_lines`]' output space: `(row, col)`, both
    /// 0-based, `col` a DISPLAY column (unicode-width aware, matching
    /// `render::hscroll_cut`/[`crate::prompt::PromptState::cursor_col`]) rather than the byte
    /// offset [`Self::col`] holds.
    ///
    /// Counts whole wrapped rows for every source line before [`Self::line`], then re-wraps just
    /// the PREFIX of the current line up to the cursor to find which of ITS wrapped rows the
    /// cursor sits on and how wide that row's prefix is — cheap (editor buffers are short) and
    /// exact for the common case; a cursor sitting mid a run of collapsed whitespace (see
    /// [`wrap_text`]'s doc comment on whitespace collapsing) can land a column or two off, which
    /// is the same imprecision `wrap_text` itself accepts for prose reflow.
    pub fn cursor_screen_pos(&self, width: usize) -> (usize, usize) {
        let mut row = 0usize;
        for line in &self.lines[..self.line] {
            row += wrap_text(line, width).len();
        }
        let prefix = &self.lines[self.line][..self.col];
        let prefix_wrapped = wrap_text(prefix, width);
        // `wrap_text` never returns an empty `Vec` (an empty prefix still yields one empty
        // line — see its doc comment), so `last()` always has something to measure.
        let last_row = prefix_wrapped.last().map(String::as_str).unwrap_or("");
        let col = last_row
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();
        row += prefix_wrapped.len() - 1;
        (row, col)
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::EditorState;

    #[test]
    fn insert_appends_and_advances_cursor() {
        let mut e = EditorState::new();
        e.insert_char('a');
        e.insert_char('b');
        assert_eq!(e.lines(), &["ab"]);
        assert_eq!((e.cursor_line(), e.cursor_col()), (0, 2));
    }

    #[test]
    fn newline_splits_the_line_at_the_cursor() {
        let mut e = EditorState::new();
        for c in "abcd".chars() {
            e.insert_char(c);
        }
        e.move_left();
        e.move_left();
        e.newline();
        assert_eq!(e.lines(), &["ab", "cd"]);
        assert_eq!((e.cursor_line(), e.cursor_col()), (1, 0));
    }

    #[test]
    fn backspace_at_column_zero_joins_the_previous_line() {
        let mut e = EditorState::from_text("ab\ncd");
        // `from_text` leaves the cursor at the end of the seeded text; walk it back to the
        // start of the second line (column 0) — same field access `mod tests` gets on any
        // private field of its parent module's types.
        e.col = 0;
        e.backspace();
        assert_eq!(e.lines(), &["abcd"]);
        assert_eq!((e.cursor_line(), e.cursor_col()), (0, 2));
    }

    #[test]
    fn backspace_on_the_first_line_at_column_zero_is_a_noop() {
        let mut e = EditorState::new();
        e.backspace();
        assert_eq!(e.lines(), &[""]);
    }

    #[test]
    fn delete_at_end_of_line_joins_the_next_line() {
        let mut e = EditorState::from_text("ab\ncd");
        e.move_home();
        e.move_up();
        e.move_end();
        e.delete();
        assert_eq!(e.lines(), &["abcd"]);
        assert_eq!((e.cursor_line(), e.cursor_col()), (0, 2));
    }

    #[test]
    fn move_up_down_clamp_column_to_the_shorter_line() {
        let mut e = EditorState::from_text("abcdef\nxy");
        // Cursor starts at the end of "xy" (col 2); moving up should clamp to "abcdef"'s
        // column 2, not carry a column past its own length.
        e.move_up();
        assert_eq!((e.cursor_line(), e.cursor_col()), (0, 2));
        e.move_end();
        e.move_down();
        assert_eq!((e.cursor_line(), e.cursor_col()), (1, 2));
    }

    #[test]
    fn is_dirty_is_false_only_for_a_fresh_empty_buffer() {
        let mut e = EditorState::new();
        assert!(!e.is_dirty());
        e.insert_char('x');
        assert!(e.is_dirty());
    }

    #[test]
    fn text_round_trips_through_newline_and_submit() {
        let mut e = EditorState::new();
        for c in "line one".chars() {
            e.insert_char(c);
        }
        e.newline();
        for c in "line two".chars() {
            e.insert_char(c);
        }
        assert_eq!(e.text(), "line one\nline two");
    }

    #[test]
    fn wrapped_lines_never_merges_across_a_source_newline() {
        let mut e = EditorState::new();
        for c in "a b".chars() {
            e.insert_char(c);
        }
        e.newline();
        for c in "c d".chars() {
            e.insert_char(c);
        }
        // Width 3 alone would fit "a b" and "c d" onto one combined greedy-wrapped line if the
        // wrap ran over the joined text — asserting it doesn't confirms the per-source-line call.
        assert_eq!(e.wrapped_lines(3), vec!["a b", "c d"]);
    }

    #[test]
    fn cursor_screen_pos_tracks_a_wrapped_row_and_column() {
        let mut e = EditorState::new();
        for c in "hello world".chars() {
            e.insert_char(c);
        }
        // Width 5 wraps "hello world" to ["hello", "world"]; the cursor (at the end, after
        // "world") should land on row 1, column 5.
        assert_eq!(e.cursor_screen_pos(5), (1, 5));
    }

    #[test]
    fn delete_word_back_skips_trailing_whitespace_then_deletes_the_word() {
        let mut e = EditorState::new();
        for c in "foo bar ".chars() {
            e.insert_char(c);
        }
        e.delete_word_back();
        assert_eq!(e.lines(), &["foo "]);
    }
}
