//! Literal, smartcase text search over a file's pre-collapse diff rows (the in-diff search: `/`
//! in the diff view). [`compute_matches`] scans [`crate::align::AlignedRow`]s — the space BEFORE gap-collapse
//! — so a search sees hidden context exactly like it sees visible content; the caller (`app.rs`)
//! is what auto-expands a gap a match lands inside, on jump.
//!
//! Kept as pure functions over `AlignedRow`s (mirroring `align.rs`'s own style) rather than a
//! `FileView` method, so this stays independently unit-testable without building a whole
//! `App`/`FileView` fixture — `old_line`/`new_line` are taken as closures rather than a `FileView`
//! reference for the same reason.

use crate::align::{AlignedRow, CellKind, Row};

/// Which side(s) of an [`AlignedRow`] a [`SearchMatch`] highlights. `Both` is a context row: the
/// same content renders on both SBS columns (and as a single [`crate::align::InlineRow::Context`]
/// row in the inline layout), so one match covers both — scanning it twice would double every
/// context-line match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSide {
    Old,
    New,
    Both,
}

/// One literal match against a file's pre-collapse row space: which [`AlignedRow`] (by index into
/// the file's `aligned` vector — the SAME `key` space [`crate::align::DisplayRow::Gap`] carries,
/// so a caller can resolve "is this match hidden, and behind which gap" via
/// [`crate::align::gap_key_for_aligned_idx`]), which side(s), the byte range `[start, end)` within
/// that line's text, and — redundantly, for cheap ordering against a cursor position without
/// re-reading `aligned` — the row's own (old, new) 1-based line numbers (`None` on the side a
/// `Del`/`Add` row's `Filler` counterpart doesn't carry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub aligned_idx: usize,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub side: SearchSide,
    pub start: usize,
    pub end: usize,
}

/// Smartcase, per vim's own rule: case-sensitive if `query` contains any uppercase char,
/// case-insensitive otherwise. Returns every non-overlapping literal match's byte range in
/// `haystack`, left-to-right; empty for an empty query.
///
/// The insensitive path folds char-by-char over the ORIGINAL string rather than comparing
/// against `haystack.to_lowercase()`: whole-string lowercasing can change byte lengths
/// (`'İ'` lowercases to `"i\u{307}"`), which would shift every offset after such a char —
/// and these offsets are later used to slice the original line's text at render time, where
/// a shifted offset can land mid-codepoint and panic.
fn find_all(haystack: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    if query.chars().any(|c| c.is_uppercase()) {
        let mut out = Vec::new();
        let mut start = 0;
        while start <= haystack.len() {
            let Some(pos) = haystack[start..].find(query) else {
                break;
            };
            let s = start + pos;
            let e = s + query.len();
            out.push((s, e));
            start = e.max(s + 1);
        }
        return out;
    }
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut out = Vec::new();
    let mut skip_until = 0;
    for (s, _) in haystack.char_indices() {
        if s < skip_until {
            continue;
        }
        if let Some(e) = folded_match_at(haystack, s, &needle) {
            out.push((s, e));
            skip_until = e.max(s + 1);
        }
    }
    out
}

/// Whether the case-folded `needle` matches `haystack` starting at byte offset `start` (a char
/// boundary); returns the match's END byte offset into the ORIGINAL `haystack` on success. The
/// needle must be exhausted exactly at a haystack char boundary — a needle ending partway through
/// one char's multi-char lowercase expansion (`"i"` against `'İ'` → `"i\u{307}"`) is NOT a match,
/// since there is no original-string byte offset that could represent "half of that char".
fn folded_match_at(haystack: &str, start: usize, needle: &[char]) -> Option<usize> {
    let mut ni = 0;
    for (off, c) in haystack[start..].char_indices() {
        for fc in c.to_lowercase() {
            if ni >= needle.len() || fc != needle[ni] {
                return None;
            }
            ni += 1;
        }
        if ni == needle.len() {
            return Some(start + off + c.len_utf8());
        }
    }
    None
}

/// Scan every row of `rows` (a file's pre-collapse [`AlignedRow`] vector) for literal, smartcase
/// matches of `query`, addressing hidden-context rows exactly like visible ones. `old_line`/
/// `new_line` fetch a 1-based line's text (mirrors [`crate::app::FileView::old_line`]/`new_line`).
///
/// A [`CellKind::Context`] row is scanned ONCE (its new-side text — old and new agree on content
/// there) and reported as [`SearchSide::Both`]. A `Del`/`Add` row (or a paired change block's
/// `Filler` side) is scanned on whichever side actually carries a [`Row::Line`]; a row with
/// `Filler` on both sides (never produced by [`crate::align::align_file`], but not this function's
/// job to assume) simply contributes nothing.
pub fn compute_matches(
    rows: &[AlignedRow],
    query: &str,
    old_line: impl Fn(usize) -> String,
    new_line: impl Fn(usize) -> String,
) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (idx, row) in rows.iter().enumerate() {
        let both_context = row.old_kind == CellKind::Context && row.new_kind == CellKind::Context;
        if both_context {
            if let Row::Line(n) = row.new {
                let text = new_line(n);
                for (start, end) in find_all(&text, query) {
                    out.push(SearchMatch {
                        aligned_idx: idx,
                        old_lineno: row_lineno(row.old),
                        new_lineno: Some(n),
                        side: SearchSide::Both,
                        start,
                        end,
                    });
                }
            }
            continue;
        }
        if let Row::Line(n) = row.old {
            let text = old_line(n);
            for (start, end) in find_all(&text, query) {
                out.push(SearchMatch {
                    aligned_idx: idx,
                    old_lineno: Some(n),
                    new_lineno: row_lineno(row.new),
                    side: SearchSide::Old,
                    start,
                    end,
                });
            }
        }
        if let Row::Line(n) = row.new {
            let text = new_line(n);
            for (start, end) in find_all(&text, query) {
                out.push(SearchMatch {
                    aligned_idx: idx,
                    old_lineno: row_lineno(row.old),
                    new_lineno: Some(n),
                    side: SearchSide::New,
                    start,
                    end,
                });
            }
        }
    }
    out
}

fn row_lineno(row: Row) -> Option<usize> {
    match row {
        Row::Line(n) => Some(n),
        Row::Filler => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::align::{align_file, collapse_gaps};
    use crate::model::{Hunk, HunkLine, LineKind};

    fn hl(kind: LineKind, old: Option<u32>, new: Option<u32>) -> HunkLine {
        HunkLine {
            kind,
            content: Vec::new(),
            old_lnum: old,
            new_lnum: new,
            missing_newline: false,
        }
    }

    fn hunk(
        old_start: u32,
        old_count: u32,
        new_start: u32,
        new_count: u32,
        lines: Vec<HunkLine>,
    ) -> Hunk {
        Hunk {
            old_start,
            old_count,
            new_start,
            new_count,
            header: Vec::new(),
            lines,
        }
    }

    #[test]
    fn matches_a_paired_del_add_line_on_both_sides_independently() {
        let h = hunk(
            1,
            1,
            1,
            1,
            vec![
                hl(LineKind::Deletion, Some(1), None),
                hl(LineKind::Addition, None, Some(1)),
            ],
        );
        let aligned = align_file(&[h], 1, 1).rows;
        let old = |n: usize| {
            if n == 1 {
                "needle here".to_string()
            } else {
                String::new()
            }
        };
        let new = |n: usize| {
            if n == 1 {
                "no needle".to_string()
            } else {
                String::new()
            }
        };
        let matches = compute_matches(&aligned, "needle", old, new);
        assert_eq!(matches.len(), 2, "one match per side: {matches:?}");
        assert!(matches.iter().any(|m| m.side == SearchSide::Old));
        assert!(matches.iter().any(|m| m.side == SearchSide::New));
    }

    #[test]
    fn matches_a_context_line_once_as_both() {
        let aligned = align_file(&[], 1, 1).rows;
        let old = |_: usize| "same text".to_string();
        let new = |_: usize| "same text".to_string();
        let matches = compute_matches(&aligned, "text", old, new);
        assert_eq!(matches.len(), 1, "a context row must match once, not twice");
        assert_eq!(matches[0].side, SearchSide::Both);
    }

    #[test]
    fn smartcase_is_case_sensitive_only_when_the_query_has_uppercase() {
        let aligned = align_file(&[], 1, 1).rows;
        let old = |_: usize| "Needle".to_string();
        let new = |_: usize| "Needle".to_string();
        assert_eq!(
            compute_matches(&aligned, "needle", old, new).len(),
            1,
            "an all-lowercase query is case-insensitive"
        );
        assert_eq!(
            compute_matches(&aligned, "Needle", old, new).len(),
            1,
            "an exact-case query still matches"
        );
        assert_eq!(
            compute_matches(&aligned, "NEEDLE", old, new).len(),
            0,
            "a query with any uppercase char turns on case-sensitivity"
        );
    }

    fn context_row(n: usize) -> AlignedRow {
        AlignedRow {
            old: Row::Line(n),
            new: Row::Line(n),
            old_kind: CellKind::Context,
            new_kind: CellKind::Context,
        }
    }

    fn change_row(n: usize) -> AlignedRow {
        AlignedRow {
            old: Row::Line(n),
            new: Row::Line(n),
            old_kind: CellKind::Del,
            new_kind: CellKind::Add,
        }
    }

    #[test]
    fn sees_matches_in_rows_that_would_collapse_into_a_gap() {
        // A long unchanged run bracketed by real change rows (mirrors align.rs's own
        // `change_then_context_run_then_change` gap fixture) — with a needle buried deep inside
        // it, `compute_matches` must still find it, since it scans the PRE-collapse space.
        let mut rows = vec![change_row(1)];
        rows.extend((2..=17).map(context_row));
        rows.push(change_row(18));
        // Confirm the fixture actually produces a gap, so this test is meaningful.
        assert!(collapse_gaps(&rows)
            .iter()
            .any(|r| matches!(r, crate::align::DisplayRow::Gap { .. })));

        let old = |n: usize| {
            if n == 10 {
                "buried needle".to_string()
            } else {
                "x".to_string()
            }
        };
        let new = |n: usize| {
            if n == 10 {
                "buried needle".to_string()
            } else {
                "x".to_string()
            }
        };
        let matches = compute_matches(&rows, "needle", old, new);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].new_lineno, Some(10));
    }

    #[test]
    fn insensitive_offsets_stay_valid_when_lowercasing_would_shift_byte_lengths() {
        // 'İ' (U+0130, 2 bytes) lowercases to "i\u{307}" (3 bytes) — whole-string lowercasing
        // would shift every offset after it by one byte, handing render-time slicing a
        // mid-codepoint index. The reported range must slice the ORIGINAL text cleanly.
        let aligned = align_file(&[], 1, 1).rows;
        let text = "İstanbul needle";
        let line = move |_: usize| text.to_string();
        let matches = compute_matches(&aligned, "needle", line, line);
        assert_eq!(matches.len(), 1);
        let (start, end) = (matches[0].start, matches[0].end);
        assert_eq!(
            &text[start..end],
            "needle",
            "match offsets must index the original (un-lowercased) line text"
        );
    }

    #[test]
    fn insensitive_needle_ending_mid_lowercase_expansion_is_not_a_match() {
        // 'İ' folds to two chars ("i\u{307}"); a query consuming only the first has no valid
        // end offset in the original string, so it must not match at that position — but the
        // plain 'i' later in the same line still does.
        let aligned = align_file(&[], 1, 1).rows;
        let text = "İzmir";
        let line = move |_: usize| text.to_string();
        let matches = compute_matches(&aligned, "i", line, line);
        assert_eq!(matches.len(), 1, "only the plain 'i' in 'zmir' matches");
        assert_eq!(&text[matches[0].start..matches[0].end], "i");
    }
}
