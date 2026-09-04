//! A one-row text-input primitive: [`PromptState`] holds a buffer plus a byte-offset cursor and
//! exposes pure edit operations (never touches [`crate::app::App`] or terminal I/O — the
//! keymap/cascade wiring that turns key events into these calls, and the pane it renders inside,
//! are a later changeset's job). In-diff navigation's outline fuzzy filter (`/` in the
//! outline pane) and in-diff search (`/` in the diff pane) both need "one editable line with a
//! blinking-cursor feel"; rather than grow that logic twice, this module is that shared line
//! editor, built once and unused until
//! the next two changesets wire it up.
//!
//! Emacs/readline-flavored bindings were chosen over vim-insert-mode ones because the prototype's
//! picker and vim's own cmdline both use them (`Ctrl-a`/`Ctrl-e`/`Ctrl-u`/`Ctrl-w`) — matching
//! muscle memory the target users already have from both source of inspiration.
//!
//! ## Byte offsets, not char counts
//!
//! [`PromptState::cursor`] is a *byte* offset into [`PromptState::buffer`], always sitting on a
//! UTF-8 char boundary — never a char count or a display column. Byte offsets are what
//! `String::insert`/`remove`/slicing want, so every edit op stays a direct buffer mutation with
//! no index translation. Only [`PromptState::render_line`], which has to say "the cursor sits
//! under display column N," walks the buffer accumulating [`unicode_width::UnicodeWidthChar`]
//! widths to translate — the same pattern `render::hscroll_cut` already uses for the same reason
//! (a byte offset and a display column are different units the instant a line has a multibyte or
//! wide character in it).

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// A single editable line: the text typed so far, and where the cursor sits within it.
///
/// All mutating methods keep [`Self::cursor`] on a valid UTF-8 char boundary in
/// [`Self::buffer`] (`0..=buffer.len()`) — never mid-codepoint, so every op can slice/insert the
/// buffer directly without a `is_char_boundary` guard at each call site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptState {
    buffer: String,
    cursor: usize,
}

impl PromptState {
    /// An empty prompt, cursor at the start — the state a filter/search input opens in.
    pub fn new() -> Self {
        Self::default()
    }

    /// The text typed so far.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// The cursor's byte offset into [`Self::buffer`] (always a char boundary).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// `true` when nothing has been typed — the caller-facing "is there a query at all" check
    /// (the outline fuzzy filter/in-diff search both fall back to their unfiltered/inactive
    /// behavior on an empty buffer).
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Reset to a fresh, empty prompt — `Ctrl-c`'s "clear and defocus" behavior (the outline
    /// fuzzy filter) is one call to this plus a focus-flag flip the caller owns.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    /// Insert `c` at the cursor, then advance the cursor past it.
    pub fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the char immediately before the cursor (readline's `Backspace`) — a no-op at the
    /// start of the buffer.
    pub fn backspace(&mut self) {
        let Some(prev) = self.prev_boundary() else {
            return;
        };
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
    }

    /// Delete the char immediately after the cursor (readline's `Delete`/`Ctrl-d`) — a no-op at
    /// the end of the buffer.
    pub fn delete(&mut self) {
        let Some(next) = self.next_boundary() else {
            return;
        };
        self.buffer.drain(self.cursor..next);
    }

    /// Move the cursor one char left — a no-op at the start of the buffer.
    pub fn move_left(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.cursor = prev;
        }
    }

    /// Move the cursor one char right — a no-op at the end of the buffer.
    pub fn move_right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    /// `Ctrl-a`: jump to the start of the buffer.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// `Ctrl-e`: jump to the end of the buffer.
    pub fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    /// `Ctrl-u`: delete everything from the start of the buffer up to (not including) the
    /// cursor, then leave the cursor at the (now empty) start — readline's "clear to start of
    /// line," not a full [`Self::clear`] (text after the cursor survives).
    pub fn clear_to_start(&mut self) {
        self.buffer.drain(..self.cursor);
        self.cursor = 0;
    }

    /// `Ctrl-w`: delete the "word" immediately before the cursor — readline/shell semantics:
    /// first skip any run of trailing whitespace, then delete the run of non-whitespace before
    /// that. A no-op at the start of the buffer.
    pub fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let before = &self.buffer[..self.cursor];
        let mut end = self.cursor;
        let mut chars = before.char_indices().rev().peekable();

        // Skip trailing whitespace first, so `"foo bar "` + `Ctrl-w` deletes `"bar "`, not just
        // the trailing space (matches bash/readline, not a naive "delete back to whitespace").
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

        self.buffer.drain(start..self.cursor);
        self.cursor = start;
    }

    /// The byte offset of the char boundary immediately before the cursor, or `None` at the
    /// start of the buffer.
    fn prev_boundary(&self) -> Option<usize> {
        self.buffer[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    /// The byte offset of the char boundary immediately after the cursor, or `None` at the end
    /// of the buffer.
    fn next_boundary(&self) -> Option<usize> {
        self.buffer[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }

    /// Render this prompt as a single [`Line`]: the buffer's text plus a visible cursor cell
    /// (styled with [`Modifier::REVERSED`], a block-cursor look with no palette dependency — the
    /// pane that ends up hosting this decides surrounding chrome/colors, so this stays as
    /// theme-agnostic as `render::hscroll_cut`'s column math it borrows). When the cursor sits
    /// past the last char (the common case — typing appends at the end), the cell is a single
    /// reversed space so the cursor is still visible on an otherwise-plain trailing position.
    ///
    /// Splits the buffer at the cursor's char boundary rather than indexing by display column —
    /// the cursor's OWN cell always covers exactly one char (or the trailing space), so no
    /// column translation is needed here at all; [`unicode_width`] only matters if a caller later
    /// needs to reason about the rendered line's total display width, which this helper doesn't
    /// do.
    pub fn render_line(&self) -> Line<'static> {
        let before = self.buffer[..self.cursor].to_string();
        let mut spans = Vec::with_capacity(3);
        if !before.is_empty() {
            spans.push(Span::raw(before));
        }

        match self.next_boundary() {
            Some(next) => {
                spans.push(Span::styled(
                    self.buffer[self.cursor..next].to_string(),
                    Modifier::REVERSED,
                ));
                let after = &self.buffer[next..];
                if !after.is_empty() {
                    spans.push(Span::raw(after.to_string()));
                }
            }
            None => spans.push(Span::styled(" ".to_string(), Modifier::REVERSED)),
        }

        Line::from(spans)
    }

    /// The cursor's 0-based display column (unicode-width aware) — for a caller that needs to
    /// place a terminal cursor or align adjacent chrome rather than just render the line via
    /// [`Self::render_line`] (which needs no column math of its own; see that method's doc
    /// comment).
    pub fn cursor_col(&self) -> usize {
        self.buffer[..self.cursor]
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_appends_and_advances_cursor() {
        let mut p = PromptState::new();
        p.insert_char('a');
        p.insert_char('b');
        assert_eq!(p.buffer(), "ab");
        assert_eq!(p.cursor(), 2);
    }

    #[test]
    fn insert_in_the_middle_shifts_the_tail() {
        let mut p = PromptState::new();
        for c in "ac".chars() {
            p.insert_char(c);
        }
        p.move_left();
        p.insert_char('b');
        assert_eq!(p.buffer(), "abc");
        assert_eq!(p.cursor(), 2);
    }

    #[test]
    fn backspace_at_start_is_a_noop() {
        let mut p = PromptState::new();
        p.backspace();
        assert_eq!(p.buffer(), "");
        assert_eq!(p.cursor(), 0);
    }

    #[test]
    fn backspace_deletes_the_char_before_the_cursor() {
        let mut p = PromptState::new();
        for c in "abc".chars() {
            p.insert_char(c);
        }
        p.backspace();
        assert_eq!(p.buffer(), "ab");
        assert_eq!(p.cursor(), 2);
    }

    #[test]
    fn delete_at_end_is_a_noop() {
        let mut p = PromptState::new();
        for c in "abc".chars() {
            p.insert_char(c);
        }
        p.delete();
        assert_eq!(p.buffer(), "abc");
        assert_eq!(p.cursor(), 3);
    }

    #[test]
    fn delete_removes_the_char_after_the_cursor() {
        let mut p = PromptState::new();
        for c in "abc".chars() {
            p.insert_char(c);
        }
        p.move_home();
        p.delete();
        assert_eq!(p.buffer(), "bc");
        assert_eq!(p.cursor(), 0);
    }

    #[test]
    fn move_left_and_right_clamp_at_the_edges() {
        let mut p = PromptState::new();
        p.insert_char('a');
        p.move_right();
        assert_eq!(p.cursor(), 1, "move_right must not run past the end");
        p.move_left();
        p.move_left();
        assert_eq!(p.cursor(), 0, "move_left must not run past the start");
    }

    #[test]
    fn home_and_end_jump_to_the_buffer_edges() {
        let mut p = PromptState::new();
        for c in "hello".chars() {
            p.insert_char(c);
        }
        p.move_home();
        assert_eq!(p.cursor(), 0);
        p.move_end();
        assert_eq!(p.cursor(), 5);
    }

    #[test]
    fn clear_to_start_drops_the_prefix_and_keeps_the_suffix() {
        let mut p = PromptState::new();
        for c in "hello world".chars() {
            p.insert_char(c);
        }
        for _ in 0.."world".len() {
            p.move_left();
        }
        p.clear_to_start();
        assert_eq!(p.buffer(), "world");
        assert_eq!(p.cursor(), 0);
    }

    #[test]
    fn clear_resets_buffer_and_cursor() {
        let mut p = PromptState::new();
        for c in "hello".chars() {
            p.insert_char(c);
        }
        p.clear();
        assert_eq!(p.buffer(), "");
        assert_eq!(p.cursor(), 0);
        assert!(p.is_empty());
    }

    #[test]
    fn delete_word_back_deletes_the_trailing_word() {
        let mut p = PromptState::new();
        for c in "foo bar".chars() {
            p.insert_char(c);
        }
        p.delete_word_back();
        assert_eq!(p.buffer(), "foo ");
        assert_eq!(p.cursor(), 4);
    }

    #[test]
    fn delete_word_back_skips_trailing_whitespace_first() {
        let mut p = PromptState::new();
        for c in "foo bar   ".chars() {
            p.insert_char(c);
        }
        p.delete_word_back();
        assert_eq!(
            p.buffer(),
            "foo ",
            "Ctrl-w on trailing whitespace should delete the word AND the whitespace, \
             matching readline"
        );
        assert_eq!(p.cursor(), 4);
    }

    #[test]
    fn delete_word_back_at_start_is_a_noop() {
        let mut p = PromptState::new();
        p.delete_word_back();
        assert_eq!(p.buffer(), "");
    }

    #[test]
    fn delete_word_back_from_a_single_word_clears_the_buffer() {
        let mut p = PromptState::new();
        for c in "hello".chars() {
            p.insert_char(c);
        }
        p.delete_word_back();
        assert_eq!(p.buffer(), "");
        assert_eq!(p.cursor(), 0);
    }

    #[test]
    fn cursor_clamps_to_char_boundaries_with_multibyte_text() {
        let mut p = PromptState::new();
        for c in "héllo".chars() {
            p.insert_char(c);
        }
        // "h" (1 byte) + "é" (2 bytes) = cursor should land at byte 3 after two inserts, and
        // every subsequent move must keep landing on a char boundary (a panic here would mean
        // `move_left`/`move_right` walked into the middle of "é"'s 2-byte encoding).
        p.move_home();
        p.move_right();
        assert_eq!(
            p.cursor(),
            1,
            "cursor should sit right after the 1-byte 'h'"
        );
        p.move_right();
        assert_eq!(
            p.cursor(),
            3,
            "cursor should skip clean over 'é' (2 bytes), landing at byte 3"
        );
    }

    #[test]
    fn backspace_removes_one_whole_multibyte_char() {
        let mut p = PromptState::new();
        for c in "hé".chars() {
            p.insert_char(c);
        }
        p.backspace();
        assert_eq!(p.buffer(), "h");
        assert_eq!(p.cursor(), 1);
    }

    #[test]
    fn cursor_col_counts_wide_cjk_chars_as_two_columns() {
        let mut p = PromptState::new();
        for c in "漢字".chars() {
            p.insert_char(c);
        }
        assert_eq!(
            p.cursor_col(),
            4,
            "two double-width chars should occupy 4 display columns"
        );
    }

    #[test]
    fn cursor_col_counts_narrow_accented_chars_as_one_column() {
        let mut p = PromptState::new();
        for c in "héllo".chars() {
            p.insert_char(c);
        }
        assert_eq!(p.cursor_col(), 5, "'héllo' is 5 narrow chars wide");
    }

    #[test]
    fn render_line_places_the_cursor_span_at_the_end_when_typing() {
        let mut p = PromptState::new();
        for c in "ab".chars() {
            p.insert_char(c);
        }
        let line = p.render_line();
        // "ab" followed by a trailing reversed-space cursor cell: two spans, not three.
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "ab");
        assert_eq!(line.spans[1].content, " ");
        assert!(line.spans[1]
            .style
            .add_modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn render_line_wraps_the_char_under_the_cursor_when_not_at_the_end() {
        let mut p = PromptState::new();
        for c in "abc".chars() {
            p.insert_char(c);
        }
        p.move_home();
        p.move_right();
        let line = p.render_line();
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "a");
        assert_eq!(line.spans[1].content, "b");
        assert!(line.spans[1]
            .style
            .add_modifier
            .contains(Modifier::REVERSED));
        assert_eq!(line.spans[2].content, "c");
    }

    #[test]
    fn render_line_on_an_empty_prompt_is_a_single_cursor_cell() {
        let p = PromptState::new();
        let line = p.render_line();
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, " ");
        assert!(line.spans[0]
            .style
            .add_modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn render_line_wraps_a_wide_cjk_char_under_the_cursor() {
        let mut p = PromptState::new();
        for c in "a漢b".chars() {
            p.insert_char(c);
        }
        p.move_home();
        p.move_right();
        let line = p.render_line();
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].content, "漢");
        assert!(line.spans[1]
            .style
            .add_modifier
            .contains(Modifier::REVERSED));
    }
}
