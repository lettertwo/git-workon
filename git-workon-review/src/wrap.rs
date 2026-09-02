//! Pure greedy word-wrap, pulled forward from slice 5's chapter-prose plan into slice 3
//! (ADR-039) because the multi-line annotation editor ([`crate::editor`]) needs it first — see
//! that module's `wrapped_lines` doc comment. Nothing in this crate wraps text today; do not use
//! `Paragraph::wrap` (no addressable output-line count, which both the editor's cursor placement
//! and slice 5's chapter height budget need).
//!
//! Rules: a `\n` in the source is always a hard break, never merged with the surrounding text;
//! within a paragraph, words fill greedily up to `width` display columns
//! ([`unicode_width::UnicodeWidthChar`], the same column math `render::hscroll_cut` uses for the
//! same reason — a byte and a display column disagree the instant a line has a multibyte or wide
//! char); a single word wider than `width` breaks AT the column. Unlike `hscroll_cut` (which
//! renders a fixed viewport and can't backtrack, so a straddling wide char is dropped and padded)
//! wrapping just starts a new output line, so a straddling char moves whole onto it instead of
//! being dropped.

use unicode_width::UnicodeWidthChar;

/// Wrap `text` to `width` display columns, one [`String`] per output line. Empty input yields
/// one empty line (a blank buffer/chapter still occupies a row); `width == 0` degrades to one
/// char per line rather than looping forever on a word that can never fit.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    text.split('\n')
        .flat_map(|paragraph| wrap_paragraph(paragraph, width))
        .collect()
}

/// Greedy-fill one `\n`-free paragraph. Whitespace runs collapse to a single joining space
/// between words (prose/comment wrapping, not a fixed-width preformatted block) — the exact
/// column an original run of spaces landed on isn't meaningful once the line has been re-flowed
/// anyway.
fn wrap_paragraph(paragraph: &str, width: usize) -> Vec<String> {
    let words: Vec<&str> = paragraph.split_whitespace().collect();
    if words.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for word in words {
        for piece in break_overlong(word, width) {
            let piece_width = display_width(&piece);
            let sep_width = if current.is_empty() { 0 } else { 1 };
            if !current.is_empty() && current_width + sep_width + piece_width > width {
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
            if !current.is_empty() {
                current.push(' ');
                current_width += 1;
            }
            current.push_str(&piece);
            current_width += piece_width;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Split `word` into `width`-or-narrower chunks when it alone is too wide to fit a line;
/// returns it unchanged as the sole element otherwise. Chunk boundaries never split a char (a
/// wide char that would push a chunk over `width` starts the NEXT chunk instead).
fn break_overlong(word: &str, width: usize) -> Vec<String> {
    if display_width(word) <= width {
        return vec![word.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for c in word.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if !current.is_empty() && current_width + w > width {
            chunks.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(c);
        current_width += w;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthChar;

    use super::wrap_text;

    #[test]
    fn empty_input_yields_one_empty_line() {
        assert_eq!(wrap_text("", 10), vec![String::new()]);
    }

    #[test]
    fn short_text_stays_on_one_line() {
        assert_eq!(wrap_text("hello world", 20), vec!["hello world"]);
    }

    #[test]
    fn greedy_fill_breaks_at_the_word_boundary() {
        // "hello world" is 11 columns; width 8 fits "hello" (5) but not "hello world" (11), and
        // "hello" + " " + "world" (11) doesn't fit either, so "world" starts a new line.
        assert_eq!(wrap_text("hello world", 8), vec!["hello", "world"]);
    }

    #[test]
    fn hard_break_on_newline_is_never_merged_with_fill() {
        assert_eq!(wrap_text("hello\nworld", 20), vec!["hello", "world"]);
    }

    #[test]
    fn blank_paragraph_between_hard_breaks_survives_as_an_empty_line() {
        assert_eq!(wrap_text("a\n\nb", 20), vec!["a", "", "b"]);
    }

    #[test]
    fn overlong_word_breaks_at_the_column() {
        assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wide_chars_count_by_display_width_not_char_count() {
        // Each CJK char below is 2 display columns wide; width 4 fits exactly two per line.
        let text = "\u{6f22}\u{5b57}\u{6f22}\u{5b57}";
        assert_eq!(UnicodeWidthChar::width('\u{6f22}'), Some(2));
        assert_eq!(
            wrap_text(text, 4),
            vec!["\u{6f22}\u{5b57}", "\u{6f22}\u{5b57}"]
        );
    }

    #[test]
    fn width_zero_degrades_to_one_char_per_line_instead_of_looping() {
        assert_eq!(wrap_text("ab", 0), vec!["a", "b"]);
    }
}
