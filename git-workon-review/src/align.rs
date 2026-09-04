//! Side-by-side row alignment, ported from the `review-tui-spike` prototype's `align.rs`.
//!
//! Walks a file's hunks against its full old/new text and produces one row vector pairing
//! old-side and new-side positions so the UI can render a row-aligned side-by-side view.
//! Outside hunks, lines pair 1:1. Inside a hunk, git emits deletions before additions within
//! each change block; we pair del[i] with add[i] and give the shorter side filler rows for the
//! excess.
//!
//! This module reads only hunk counters (`old_start`/`old_count`/`new_start`/`new_count`),
//! [`crate::model::Hunk::lines`], and each line's kind + `old_lnum`/`new_lnum`. Content is NOT
//! read from hunk lines here — rendering reads full file text by line number so numbers and
//! content stay in sync (M4 concern; out of scope for this module).
//!
//! ## Lineno invariant
//!
//! [`crate::model::HunkLine::old_lnum`]/`new_lnum` are `None` for the wrong side of an
//! addition/deletion (see the doc comment on [`crate::synthesis::LineSelection`], which relies
//! on the same guarantee). Concretely: a [`LineKind::Context`] line always has both linenos
//! populated; a [`LineKind::Deletion`] line always has `old_lnum` populated; a
//! [`LineKind::Addition`] line always has `new_lnum` populated. This is git2's own guarantee
//! (`Patch::line_in_hunk`'s `old_lineno`/`new_lineno`), not something this module can violate,
//! so the pairing code below `expect()`s the lineno for the side each kind is documented to
//! carry.

use crate::model::{Hunk, HunkLine, LineKind};

/// A row position on one side of the aligned view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// 1-based line number into the full file text for this side.
    Line(usize),
    Filler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Context,
    Del,
    Add,
    Filler,
}

#[derive(Debug, Clone, Copy)]
pub struct AlignedRow {
    pub old: Row,
    pub new: Row,
    pub old_kind: CellKind,
    pub new_kind: CellKind,
}

impl AlignedRow {
    /// True when this row is a paired change line (Del on old, Add on new) eligible for
    /// word-level diffing. Unpaired excess lines get whole-line emphasis instead.
    pub fn is_word_diff_pair(&self) -> bool {
        matches!(
            (self.old_kind, self.new_kind),
            (CellKind::Del, CellKind::Add)
        )
    }
}

pub struct Aligned {
    pub rows: Vec<AlignedRow>,
}

fn gap_end(start: usize, count: usize) -> usize {
    if count == 0 {
        start
    } else {
        start - 1
    }
}

/// Flush a pending del/add block, pairing by index and emitting filler rows for the excess on
/// the shorter side.
fn flush_block(dels: &[&HunkLine], adds: &[&HunkLine], rows: &mut Vec<AlignedRow>) {
    let max_len = dels.len().max(adds.len());
    for i in 0..max_len {
        let (old, old_kind) = match dels.get(i) {
            Some(d) => (
                Row::Line(d.old_lnum.expect("deletion line has old_lnum") as usize),
                CellKind::Del,
            ),
            None => (Row::Filler, CellKind::Filler),
        };
        let (new, new_kind) = match adds.get(i) {
            Some(a) => (
                Row::Line(a.new_lnum.expect("addition line has new_lnum") as usize),
                CellKind::Add,
            ),
            None => (Row::Filler, CellKind::Filler),
        };
        rows.push(AlignedRow {
            old,
            new,
            old_kind,
            new_kind,
        });
    }
}

/// Align a file's rows given its hunks. `old_line_count` / `new_line_count` are the total line
/// counts of the full old/new text, used to fill the tail gap after the last hunk.
pub fn align_file(hunks: &[Hunk], old_line_count: usize, new_line_count: usize) -> Aligned {
    let mut rows = Vec::new();
    let mut old_pos = 0usize; // count of old lines already emitted
    let mut new_pos = 0usize;

    for hunk in hunks {
        let old_start = hunk.old_start as usize;
        let old_count = hunk.old_count as usize;
        let new_start = hunk.new_start as usize;
        let new_count = hunk.new_count as usize;

        let old_ge = gap_end(old_start, old_count);
        let new_ge = gap_end(new_start, new_count);
        let old_gap = old_ge.saturating_sub(old_pos);
        let new_gap = new_ge.saturating_sub(new_pos);
        debug_assert_eq!(
            old_gap, new_gap,
            "context gap between hunks must be equal length on both sides"
        );
        let gap = old_gap.min(new_gap);
        for i in 0..gap {
            rows.push(AlignedRow {
                old: Row::Line(old_pos + i + 1),
                new: Row::Line(new_pos + i + 1),
                old_kind: CellKind::Context,
                new_kind: CellKind::Context,
            });
        }

        let mut pending_dels: Vec<&HunkLine> = Vec::new();
        let mut pending_adds: Vec<&HunkLine> = Vec::new();
        for line in &hunk.lines {
            match line.kind {
                LineKind::Deletion => pending_dels.push(line),
                LineKind::Addition => pending_adds.push(line),
                LineKind::Context => {
                    if !pending_dels.is_empty() || !pending_adds.is_empty() {
                        flush_block(&pending_dels, &pending_adds, &mut rows);
                        pending_dels.clear();
                        pending_adds.clear();
                    }
                    rows.push(AlignedRow {
                        old: Row::Line(line.old_lnum.expect("context line has old_lnum") as usize),
                        new: Row::Line(line.new_lnum.expect("context line has new_lnum") as usize),
                        old_kind: CellKind::Context,
                        new_kind: CellKind::Context,
                    });
                }
            }
        }
        if !pending_dels.is_empty() || !pending_adds.is_empty() {
            flush_block(&pending_dels, &pending_adds, &mut rows);
        }

        old_pos = old_start + old_count.saturating_sub(1);
        new_pos = new_start + new_count.saturating_sub(1);
    }

    // Tail gap after the last hunk (or the whole file, if there are no hunks).
    let old_tail = old_line_count.saturating_sub(old_pos);
    let new_tail = new_line_count.saturating_sub(new_pos);
    debug_assert_eq!(
        old_tail, new_tail,
        "trailing context after the last hunk must be equal length on both sides"
    );
    let tail = old_tail.min(new_tail);
    for i in 0..tail {
        rows.push(AlignedRow {
            old: Row::Line(old_pos + i + 1),
            new: Row::Line(new_pos + i + 1),
            old_kind: CellKind::Context,
            new_kind: CellKind::Context,
        });
    }

    Aligned { rows }
}

/// A row of the gap-collapsed display, layered over [`AlignedRow`]s.
///
/// Unchanged stretches longer than `2 * CONTEXT_LINES` collapse to a single [`DisplayRow::Gap`]
/// so the view doesn't scroll through pages of untouched code. Gap rows are layout-agnostic —
/// they span both panes in SBS.
#[derive(Debug, Clone, Copy)]
pub enum DisplayRow {
    Row(AlignedRow),
    Gap { skipped: usize },
}

/// Number of context lines kept around hunk content on each side of a gap.
pub const CONTEXT_LINES: usize = 3;

/// Collapse long unchanged stretches in `rows` into [`DisplayRow::Gap`] markers, keeping
/// [`CONTEXT_LINES`] rows of context immediately around hunk content (Del/Add/Filler rows).
///
/// A stretch of context rows collapses only when it is strictly longer than `2 * CONTEXT_LINES`
/// (enough to keep `CONTEXT_LINES` on both sides of the gap); shorter stretches, including ones
/// between two hunks that are close together, are left as-is (no gap row — the hunks
/// effectively merge under one continuous context run).
pub fn collapse_gaps(rows: &[AlignedRow]) -> Vec<DisplayRow> {
    collapse_gaps_with(rows, CONTEXT_LINES)
}

/// Same as [`collapse_gaps`] but with an explicit context-line count, for testing.
fn collapse_gaps_with(rows: &[AlignedRow], context: usize) -> Vec<DisplayRow> {
    let is_context = |row: &AlignedRow| {
        matches!(
            (row.old_kind, row.new_kind),
            (CellKind::Context, CellKind::Context)
        )
    };

    let mut out = Vec::with_capacity(rows.len());
    let mut i = 0;
    while i < rows.len() {
        if !is_context(&rows[i]) {
            out.push(DisplayRow::Row(rows[i]));
            i += 1;
            continue;
        }

        // Measure the full run of context rows starting at i.
        let run_start = i;
        let mut run_end = i;
        while run_end < rows.len() && is_context(&rows[run_end]) {
            run_end += 1;
        }
        let run_len = run_end - run_start;

        // Keep `context` lines of lead-in unless this run touches the start of the file (no
        // hunk before it to lead away from) or the end of the file (no hunk after it to lead
        // into) — those edges get no filler on the missing side.
        let keep_before = if run_start == 0 { 0 } else { context };
        let keep_after = if run_end == rows.len() { 0 } else { context };

        if (keep_before == 0 && keep_after == 0) || run_len <= keep_before + keep_after {
            // Either too short to collapse, or (keep_before == keep_after == 0) this run is
            // the entire row list — a wholly unchanged file with no hunk on either side to
            // contextualize. Emit every row, no gap.
            for row in &rows[run_start..run_end] {
                out.push(DisplayRow::Row(*row));
            }
        } else {
            for row in &rows[run_start..run_start + keep_before] {
                out.push(DisplayRow::Row(*row));
            }
            let skipped = run_len - keep_before - keep_after;
            out.push(DisplayRow::Gap { skipped });
            for row in &rows[run_end - keep_after..run_end] {
                out.push(DisplayRow::Row(*row));
            }
        }

        i = run_end;
    }
    out
}

/// One row of the inline (unified, single-column) display.
///
/// Built by [`inline_rows`] from the SAME gap-collapsed [`DisplayRow`] vector [`collapse_gaps`]
/// already produces for the side-by-side layout — inline reuses that pass unchanged rather than
/// re-running gap collapse over its own row type (context-gap detection is layout-agnostic; only
/// how the surviving rows spread onto the screen differs). Because a del/add change block
/// becomes MULTIPLE `InlineRow` entries (deletions first, then additions — there's no second
/// column to pad against, so unlike [`AlignedRow`] there is no `Filler` variant here), this
/// vector's indices are a DIFFERENT coordinate space than `display`'s: [`crate::app::FileView`]
/// keeps a separate word-span cache keyed by THIS vector's row index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineRow {
    /// An unchanged line; carries both linenos since old and new agree on its content.
    Context {
        old: usize,
        new: usize,
    },
    /// A deleted line. `paired_new` is the addition it was aligned with in the SAME
    /// [`AlignedRow`] (index-paired within the change block), if any — kept only so the renderer
    /// can still run word-level diffing on the pair even though the two lines are no longer
    /// visually adjacent.
    Del {
        old: usize,
        paired_new: Option<usize>,
    },
    /// An added line. `paired_old` mirrors [`InlineRow::Del::paired_new`].
    Add {
        new: usize,
        paired_old: Option<usize>,
    },
    Gap {
        skipped: usize,
    },
}

impl InlineRow {
    /// True when this row has an index-paired counterpart on the other side, eligible for
    /// word-level diffing — the inline analog of [`AlignedRow::is_word_diff_pair`].
    pub fn is_word_diff_pair(&self) -> bool {
        matches!(
            self,
            InlineRow::Del {
                paired_new: Some(_),
                ..
            } | InlineRow::Add {
                paired_old: Some(_),
                ..
            }
        )
    }
}

/// Convert a gap-collapsed side-by-side display vector into the inline layout's row vector.
///
/// Walks maximal runs of non-context rows (a "change block": consecutive `AlignedRow`s where
/// `old_kind`/`new_kind` isn't `(Context, Context)`) and, within each run, emits every deletion
/// line first, then every addition line — matching git's own convention of listing removed lines
/// before added ones — dropping `Filler` entries entirely (inline has no second column to pad
/// against).
pub fn inline_rows(display: &[DisplayRow]) -> Vec<InlineRow> {
    let mut out = Vec::with_capacity(display.len());
    let mut run: Vec<AlignedRow> = Vec::new();

    fn flush(run: &mut Vec<AlignedRow>, out: &mut Vec<InlineRow>) {
        for r in run.iter().filter(|r| r.old_kind == CellKind::Del) {
            let Row::Line(old) = r.old else {
                unreachable!("a Del row always carries a Line on its old side")
            };
            let paired_new = match r.new {
                Row::Line(n) if r.new_kind == CellKind::Add => Some(n),
                _ => None,
            };
            out.push(InlineRow::Del { old, paired_new });
        }
        for r in run.iter().filter(|r| r.new_kind == CellKind::Add) {
            let Row::Line(new) = r.new else {
                unreachable!("an Add row always carries a Line on its new side")
            };
            let paired_old = match r.old {
                Row::Line(o) if r.old_kind == CellKind::Del => Some(o),
                _ => None,
            };
            out.push(InlineRow::Add { new, paired_old });
        }
        run.clear();
    }

    for row in display {
        match row {
            DisplayRow::Gap { skipped } => {
                flush(&mut run, &mut out);
                out.push(InlineRow::Gap { skipped: *skipped });
            }
            DisplayRow::Row(r)
                if r.old_kind == CellKind::Context && r.new_kind == CellKind::Context =>
            {
                flush(&mut run, &mut out);
                let (Row::Line(old), Row::Line(new)) = (r.old, r.new) else {
                    unreachable!("a Context row always carries a Line on both sides")
                };
                out.push(InlineRow::Context { old, new });
            }
            DisplayRow::Row(r) => run.push(*r),
        }
    }
    flush(&mut run, &mut out);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn parity_invariant_holds() {
        // 3 dels / 1 add block inside a hunk with context on both sides.
        let h = hunk(
            1,
            5,
            1,
            3,
            vec![
                hl(LineKind::Context, Some(1), Some(1)),
                hl(LineKind::Deletion, Some(2), None),
                hl(LineKind::Deletion, Some(3), None),
                hl(LineKind::Deletion, Some(4), None),
                hl(LineKind::Addition, None, Some(2)),
                hl(LineKind::Context, Some(5), Some(3)),
            ],
        );
        let aligned = align_file(&[h], 5, 3);
        for row in &aligned.rows {
            assert_eq!(
                matches!(row.old, Row::Filler),
                row.old_kind == CellKind::Filler
            );
            assert_eq!(
                matches!(row.new, Row::Filler),
                row.new_kind == CellKind::Filler
            );
        }

        // ctx1, then 3 paired-or-filler rows for the del/add block, then ctx2.
        assert_eq!(aligned.rows.len(), 5);
        assert_eq!(aligned.rows[0].old_kind, CellKind::Context);
        assert_eq!(aligned.rows[0].new_kind, CellKind::Context);

        // del1/add1 paired.
        assert_eq!(aligned.rows[1].old, Row::Line(2));
        assert_eq!(aligned.rows[1].new, Row::Line(2));
        assert_eq!(aligned.rows[1].old_kind, CellKind::Del);
        assert_eq!(aligned.rows[1].new_kind, CellKind::Add);
        assert!(aligned.rows[1].is_word_diff_pair());

        // del2/del3 have no add counterpart -> filler on new side.
        assert_eq!(aligned.rows[2].old, Row::Line(3));
        assert_eq!(aligned.rows[2].new, Row::Filler);
        assert_eq!(aligned.rows[2].new_kind, CellKind::Filler);
        assert!(!aligned.rows[2].is_word_diff_pair());

        assert_eq!(aligned.rows[3].old, Row::Line(4));
        assert_eq!(aligned.rows[3].new, Row::Filler);

        assert_eq!(aligned.rows[4].old_kind, CellKind::Context);
        assert_eq!(aligned.rows[4].old, Row::Line(5));
        assert_eq!(aligned.rows[4].new, Row::Line(3));
    }

    #[test]
    fn pure_addition_at_start_of_file() {
        let h = hunk(
            0,
            0,
            1,
            2,
            vec![
                hl(LineKind::Addition, None, Some(1)),
                hl(LineKind::Addition, None, Some(2)),
            ],
        );
        let aligned = align_file(&[h], 0, 2);
        assert_eq!(aligned.rows.len(), 2);
        assert_eq!(aligned.rows[0].old, Row::Filler);
        assert_eq!(aligned.rows[0].new, Row::Line(1));
        assert_eq!(aligned.rows[1].old, Row::Filler);
        assert_eq!(aligned.rows[1].new, Row::Line(2));
    }

    #[test]
    fn no_hunks_pairs_whole_file_1to1() {
        let aligned = align_file(&[], 4, 4);
        assert_eq!(aligned.rows.len(), 4);
        for (i, row) in aligned.rows.iter().enumerate() {
            assert_eq!(row.old, Row::Line(i + 1));
            assert_eq!(row.new, Row::Line(i + 1));
            assert_eq!(row.old_kind, CellKind::Context);
        }
    }

    fn context_row(n: usize) -> AlignedRow {
        AlignedRow {
            old: Row::Line(n),
            new: Row::Line(n),
            old_kind: CellKind::Context,
            new_kind: CellKind::Context,
        }
    }

    fn change_row(old: Row, new: Row, old_kind: CellKind, new_kind: CellKind) -> AlignedRow {
        AlignedRow {
            old,
            new,
            old_kind,
            new_kind,
        }
    }

    #[test]
    fn tiny_file_produces_no_gaps() {
        // Whole file is context, shorter than 2 * context: no gap.
        let rows: Vec<AlignedRow> = (1..=4).map(context_row).collect();
        let display = collapse_gaps_with(&rows, 3);
        assert_eq!(display.len(), 4);
        assert!(display.iter().all(|r| matches!(r, DisplayRow::Row(_))));
    }

    #[test]
    fn gap_between_hunks_collapses_middle() {
        // hunk1 change, 10 lines context, hunk2 change: with context=3, the middle 4 lines
        // (10 - 3 - 3) collapse into one gap row.
        let mut rows = vec![change_row(
            Row::Line(1),
            Row::Line(1),
            CellKind::Del,
            CellKind::Add,
        )];
        rows.extend((2..=11).map(context_row));
        rows.push(change_row(
            Row::Line(12),
            Row::Line(12),
            CellKind::Del,
            CellKind::Add,
        ));

        let display = collapse_gaps_with(&rows, 3);
        // change, 3 ctx, gap, 3 ctx, change
        assert_eq!(display.len(), 9);
        assert!(matches!(display[0], DisplayRow::Row(_)));
        for row in &display[1..4] {
            assert!(matches!(row, DisplayRow::Row(r) if r.old_kind == CellKind::Context));
        }
        match display[4] {
            DisplayRow::Gap { skipped } => assert_eq!(skipped, 4),
            other => panic!("expected gap row, got {other:?}"),
        }
        for row in &display[5..8] {
            assert!(matches!(row, DisplayRow::Row(r) if r.old_kind == CellKind::Context));
        }
        assert!(matches!(display[8], DisplayRow::Row(_)));

        // The gap hides the same count on both sides by construction (rows are already
        // parity-paired context lines), but assert explicitly on the surviving rows'
        // continuity: line just before the gap and line just after are the expected distance
        // apart on both old and new sides.
        if let (DisplayRow::Row(before), DisplayRow::Row(after)) = (display[3], display[5]) {
            let (Row::Line(before_old), Row::Line(before_new)) = (before.old, before.new) else {
                panic!("expected line rows around the gap");
            };
            // after is the next change row (old=12,new=12); the gap plus kept context must
            // account for all lines strictly between.
            let (Row::Line(after_old), Row::Line(after_new)) = (after.old, after.new) else {
                panic!("expected line rows around the gap");
            };
            assert_eq!(
                after_old - before_old,
                after_new - before_new,
                "gap hides equal spans"
            );
        }
    }

    #[test]
    fn adjacent_hunks_with_too_little_context_merge_without_gap() {
        // Only 4 lines of context between two change blocks with context=3: 4 <= 3+3, no gap.
        let mut rows = vec![change_row(
            Row::Line(1),
            Row::Line(1),
            CellKind::Del,
            CellKind::Add,
        )];
        rows.extend((2..=5).map(context_row));
        rows.push(change_row(
            Row::Line(6),
            Row::Line(6),
            CellKind::Del,
            CellKind::Add,
        ));

        let display = collapse_gaps_with(&rows, 3);
        assert_eq!(display.len(), rows.len());
        assert!(display.iter().all(|r| matches!(r, DisplayRow::Row(_))));
    }

    #[test]
    fn gap_at_file_start_has_no_lead_in() {
        // Leading context run (file starts unchanged) before the first hunk: no context to
        // "lead away from" on the left edge, so the whole run before the trailing keep-window
        // can collapse.
        let mut rows: Vec<AlignedRow> = (1..=10).map(context_row).collect();
        rows.push(change_row(
            Row::Line(11),
            Row::Line(11),
            CellKind::Del,
            CellKind::Add,
        ));

        let display = collapse_gaps_with(&rows, 3);
        // gap, 3 ctx, change
        assert_eq!(display.len(), 5);
        match display[0] {
            DisplayRow::Gap { skipped } => assert_eq!(skipped, 7),
            other => panic!("expected gap row, got {other:?}"),
        }
        for row in &display[1..4] {
            assert!(matches!(row, DisplayRow::Row(_)));
        }
        assert!(matches!(display[4], DisplayRow::Row(_)));
    }

    #[test]
    fn gap_at_file_end_has_no_trail_out() {
        let mut rows = vec![change_row(
            Row::Line(1),
            Row::Line(1),
            CellKind::Del,
            CellKind::Add,
        )];
        rows.extend((2..=11).map(context_row));

        let display = collapse_gaps_with(&rows, 3);
        // change, 3 ctx, gap
        assert_eq!(display.len(), 5);
        assert!(matches!(display[0], DisplayRow::Row(_)));
        for row in &display[1..4] {
            assert!(matches!(row, DisplayRow::Row(_)));
        }
        match display[4] {
            DisplayRow::Gap { skipped } => assert_eq!(skipped, 7),
            other => panic!("expected gap row, got {other:?}"),
        }
    }

    #[test]
    fn inline_del_run_precedes_add_run_within_a_block() {
        // 3 dels / 1 add block: SBS index-pairs del[0]/add[0] and fillers the rest; inline must
        // emit all 3 dels first, then the 1 add — not interleaved by pairing index.
        let h = hunk(
            1,
            5,
            1,
            3,
            vec![
                hl(LineKind::Context, Some(1), Some(1)),
                hl(LineKind::Deletion, Some(2), None),
                hl(LineKind::Deletion, Some(3), None),
                hl(LineKind::Deletion, Some(4), None),
                hl(LineKind::Addition, None, Some(2)),
                hl(LineKind::Context, Some(5), Some(3)),
            ],
        );
        let aligned = align_file(&[h], 5, 3);
        let display = collapse_gaps(&aligned.rows);
        let inline = inline_rows(&display);

        assert_eq!(
            inline,
            vec![
                InlineRow::Context { old: 1, new: 1 },
                InlineRow::Del {
                    old: 2,
                    paired_new: Some(2)
                },
                InlineRow::Del {
                    old: 3,
                    paired_new: None
                },
                InlineRow::Del {
                    old: 4,
                    paired_new: None
                },
                InlineRow::Add {
                    new: 2,
                    paired_old: Some(2)
                },
                InlineRow::Context { old: 5, new: 3 },
            ]
        );
    }

    #[test]
    fn inline_has_no_filler_rows() {
        let h = hunk(
            0,
            0,
            1,
            2,
            vec![
                hl(LineKind::Addition, None, Some(1)),
                hl(LineKind::Addition, None, Some(2)),
            ],
        );
        let aligned = align_file(&[h], 0, 2);
        let display = collapse_gaps(&aligned.rows);
        let inline = inline_rows(&display);

        assert_eq!(
            inline,
            vec![
                InlineRow::Add {
                    new: 1,
                    paired_old: None
                },
                InlineRow::Add {
                    new: 2,
                    paired_old: None
                },
            ]
        );
    }

    #[test]
    fn inline_passes_gap_rows_through_unchanged() {
        let mut rows = vec![change_row(
            Row::Line(1),
            Row::Line(1),
            CellKind::Del,
            CellKind::Add,
        )];
        rows.extend((2..=11).map(context_row));
        rows.push(change_row(
            Row::Line(12),
            Row::Line(12),
            CellKind::Del,
            CellKind::Add,
        ));

        let display = collapse_gaps_with(&rows, 3);
        let inline = inline_rows(&display);
        assert!(
            inline
                .iter()
                .any(|r| matches!(r, InlineRow::Gap { skipped: 4 })),
            "expected the gap row to survive the inline conversion unchanged: {inline:?}"
        );
    }
}
