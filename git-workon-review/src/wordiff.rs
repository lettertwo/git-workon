//! Word-level diff spans for a paired del/add line.

use similar::{ChangeTag, TextDiff};

/// A byte range `[start, end)` into a line's text that should be rendered
/// with emphasized ("strong") diff background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// Compute word-granularity change spans for a paired old/new line. Returns
/// (old_spans, new_spans): byte ranges into `old_text` / `new_text` that
/// differ at word granularity.
pub fn word_diff_spans(old_text: &str, new_text: &str) -> (Vec<Span>, Vec<Span>) {
    let diff = TextDiff::configure().diff_words(old_text, new_text);

    let mut old_spans = Vec::new();
    let mut new_spans = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;

    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                old_pos += len;
                new_pos += len;
            }
            ChangeTag::Delete => {
                old_spans.push(Span {
                    start: old_pos,
                    end: old_pos + len,
                });
                old_pos += len;
            }
            ChangeTag::Insert => {
                new_spans.push(Span {
                    start: new_pos,
                    end: new_pos + len,
                });
                new_pos += len;
            }
        }
    }

    (merge_adjacent(old_spans), merge_adjacent(new_spans))
}

/// Merge spans that are directly adjacent (no gap) to reduce fragmentation
/// from word-boundary splitting.
fn merge_adjacent(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by_key(|s| s.start);
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        if let Some(last) = out.last_mut() {
            if last.end == span.start {
                last.end = span.end;
                continue;
            }
        }
        out.push(span);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_changed_word() {
        let (old_spans, new_spans) = word_diff_spans("let x = 1;", "let x = 10;");
        assert!(!old_spans.is_empty());
        assert!(!new_spans.is_empty());
        let old_changed = &old_spans[0];
        let old_text = "let x = 1;";
        assert!(old_text[old_changed.start..old_changed.end].contains('1'));
        let new_changed = &new_spans[0];
        let new_text = "let x = 10;";
        assert!(new_text[new_changed.start..new_changed.end].contains("10"));
    }

    #[test]
    fn identical_lines_have_no_spans() {
        let (old_spans, new_spans) = word_diff_spans("same line", "same line");
        assert!(old_spans.is_empty());
        assert!(new_spans.is_empty());
    }
}
