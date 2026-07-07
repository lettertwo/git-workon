//! Frame rendering: header, side-by-side diff body, footer.
//!
//! Ported from the `review-tui-spike` prototype's `ui.rs`, adapted to render [`App`]'s
//! gap-collapsed [`crate::align::DisplayRow`]s instead of a flat aligned-row list, and extended
//! with a full-width `Gap` row (the collapsed-context marker is the same on both sides, so it
//! spans the whole body rather than living in one pane).

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::align::{CellKind, DisplayRow, InlineRow, Row};
use crate::app::{App, EffectiveZoom, FileView, Layout as AppLayout, Role};
use crate::highlight::FgSpan;
use crate::model::FileStatus;
use crate::wordiff::Span as WordSpan;

const BG_DEL_SUBTLE: Color = Color::Rgb(60, 24, 24);
const BG_DEL_STRONG: Color = Color::Rgb(120, 40, 40);
const BG_ADD_SUBTLE: Color = Color::Rgb(20, 48, 24);
const BG_ADD_STRONG: Color = Color::Rgb(32, 100, 48);
const FG_DEFAULT: Color = Color::Gray;
const FG_DIM: Color = Color::DarkGray;
const FG_GUTTER: Color = Color::DarkGray;
/// Tint blended into the cursor row's background (see [`blend_bg`]) — a cool slate-blue, chosen
/// to read as "cursor here" without competing with the warm del/add hues above.
const BG_CURSOR: Color = Color::Rgb(45, 50, 90);

/// Blend the cursor row's tint into an existing background, so the cursor highlight composites
/// with (rather than replaces) del/add/word-diff emphasis on the same row — the row highlight is
/// a wash over the whole row, not a mask. `None` (a context/gap cell with no bg span at all)
/// resolves to the tint directly, since there's nothing to blend against. Non-RGB colors
/// shouldn't occur here (every bg constant in this module is `Color::Rgb`) but pass through
/// unblended rather than panicking if one ever does.
fn blend_bg(base: Option<Color>, tint: Color) -> Color {
    match (base, tint) {
        (Some(Color::Rgb(r, g, b)), Color::Rgb(tr, tg, tb)) => {
            // 60% of the original emphasis, 40% tint — enough to read as a distinct highlight
            // without washing out del/add/word-diff coloring underneath it.
            let mix = |c: u8, t: u8| ((u16::from(c) * 3 + u16::from(t) * 2) / 5) as u8;
            Color::Rgb(mix(r, tr), mix(g, tg), mix(b, tb))
        }
        (Some(other), _) => other,
        (None, tint) => tint,
    }
}

/// Apply the cursor row's highlight to an already-built line: blend [`BG_CURSOR`] into every
/// span's background (see [`blend_bg`]), then pad the line out to `width` with solid tint so the
/// highlight covers the full row even past the line's own rendered content (a short line, or one
/// pane of a filler/deleted-file row, would otherwise leave the tail of the row unhighlighted).
fn apply_cursor_row(mut line: Line<'static>, width: u16) -> Line<'static> {
    for span in &mut line.spans {
        let bg = blend_bg(span.style.bg, BG_CURSOR);
        span.style = span.style.bg(bg);
    }
    let used = line.width() as u16;
    if used < width {
        line.spans.push(TSpan::styled(
            " ".repeat((width - used) as usize),
            Style::default().bg(BG_CURSOR),
        ));
    }
    line
}

/// One resolved (bg, fg) pair for a byte range of a line.
struct Segment {
    start: usize,
    end: usize,
    bg: Option<Color>,
    fg: Color,
}

/// Merge background-role spans and syntax fg spans into a flat list of non-overlapping
/// segments covering `[0, len)`.
fn compose_segments(
    len: usize,
    bg_spans: &[(usize, usize, Color)],
    fg_spans: Option<&Vec<FgSpan>>,
) -> Vec<Segment> {
    let mut boundaries: Vec<usize> = vec![0, len];
    for (s, e, _) in bg_spans {
        boundaries.push((*s).min(len));
        boundaries.push((*e).min(len));
    }
    if let Some(fgs) = fg_spans {
        for span in fgs {
            boundaries.push(span.start.min(len));
            boundaries.push(span.end.min(len));
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut segments = Vec::with_capacity(boundaries.len());
    for w in boundaries.windows(2) {
        let (start, end) = (w[0], w[1]);
        if start >= end {
            continue;
        }
        let mid = start;
        // Later-pushed bg spans are more specific (word-level strong emphasis is pushed after
        // the whole-line subtle span in `content_spans`) and must win, so the lookup scans in
        // REVERSE push order. The spike's forward `find` silently dropped word-level emphasis:
        // the whole-line subtle span contains every offset, so it always matched first.
        let bg = bg_spans
            .iter()
            .rev()
            .find(|(s, e, _)| mid >= *s && mid < *e)
            .map(|(_, _, c)| *c);
        let fg = fg_spans
            .and_then(|fgs| fgs.iter().find(|s| mid >= s.start && mid < s.end))
            .map(|s| s.color)
            .unwrap_or(FG_DEFAULT);
        segments.push(Segment { start, end, bg, fg });
    }
    segments
}

fn gutter_width(max_lineno: usize) -> usize {
    max_lineno.to_string().len().max(3)
}

/// Which side of the aligned pair a pane line is being built for — determines which of
/// [`FileView`]'s two parallel (text, highlight) sources to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

/// Build the styled content spans (everything after the gutter) for one line of text, shared by
/// [`build_pane_line`] (SBS) and [`build_inline_line`] (inline) — the two differ only in how they
/// resolve `text`/`hl`/`emphasis` from a [`Row`] vs an [`InlineRow`] and in their gutter, not in
/// how a resolved line gets colored.
///
/// `emphasis` is `Some((subtle, strong))` for a `Del`/`Add` line (whole-line subtle background,
/// plus per-`word_spans` strong background when `is_word_pair`; whole-line strong when not paired
/// — an unpaired excess line) and `None` for `Context`/`Filler` (no background emphasis at all).
fn content_spans(
    text: &str,
    hl: Option<&Vec<FgSpan>>,
    emphasis: Option<(Color, Color)>,
    word_spans: &[WordSpan],
    is_word_pair: bool,
) -> Vec<TSpan<'static>> {
    let mut bg_spans: Vec<(usize, usize, Color)> = Vec::new();
    if let Some((subtle_bg, strong_bg)) = emphasis {
        if is_word_pair {
            bg_spans.push((0, text.len(), subtle_bg));
            for s in word_spans {
                bg_spans.push((s.start, s.end, strong_bg));
            }
        } else {
            // Unpaired excess line: whole-line strong emphasis.
            bg_spans.push((0, text.len(), strong_bg));
        }
    }

    let segments = compose_segments(text.len(), &bg_spans, hl);
    let mut spans = Vec::with_capacity(segments.len().max(1));
    if segments.is_empty() && !text.is_empty() {
        spans.push(TSpan::styled(
            text.to_string(),
            Style::default().fg(FG_DEFAULT),
        ));
    }
    for seg in segments {
        let mut style = Style::default().fg(seg.fg);
        if let Some(bg) = seg.bg {
            style = style.bg(bg);
        }
        spans.push(TSpan::styled(text[seg.start..seg.end].to_string(), style));
    }
    spans
}

/// Build a single rendered line for one pane at a display row's resolved [`Row`]/[`CellKind`].
#[allow(clippy::too_many_arguments)]
fn build_pane_line(
    view: &FileView,
    side: Side,
    row: Row,
    kind: CellKind,
    word_spans: &[WordSpan],
    is_word_pair: bool,
    subtle_bg: Color,
    strong_bg: Color,
    gutter_w: usize,
    content_w: usize,
) -> Line<'static> {
    match row {
        Row::Filler => {
            let pattern: String = "╱".repeat(content_w + gutter_w + 1);
            Line::from(TSpan::styled(pattern, Style::default().fg(FG_DIM)))
        }
        Row::Line(n) => {
            let text = match side {
                Side::Old => view.old_line(n),
                Side::New => view.new_line(n),
            };
            let hl = match side {
                Side::Old => view.old_hl.as_ref(),
                Side::New => view.new_hl.as_ref(),
            }
            .and_then(|v| v.get(n - 1));

            let gutter = format!("{n:>gutter_w$} ");
            let mut spans = vec![TSpan::styled(gutter, Style::default().fg(FG_GUTTER))];

            let emphasis = match kind {
                CellKind::Del | CellKind::Add => Some((subtle_bg, strong_bg)),
                CellKind::Context | CellKind::Filler => None,
            };
            spans.extend(content_spans(text, hl, emphasis, word_spans, is_word_pair));
            Line::from(spans)
        }
    }
}

/// Render one frame: header, SBS body, footer.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let vlayout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let header_area = vlayout[0];
    let body_area = vlayout[1];
    let footer_area = vlayout[2];

    render_header(frame, app, header_area);
    render_footer(frame, footer_area);
    render_body(frame, app, body_area);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let idx = app.current + 1;
    let n = app.files.len();
    let label = match app.files.get(app.current) {
        Some(f) if f.status == FileStatus::Renamed || f.status == FileStatus::Copied => {
            format!(
                "{} @ {} -> {}",
                f.old_path.as_deref().unwrap_or(""),
                app.base_label,
                f.path
            )
        }
        Some(f) => f.path.clone(),
        None => String::new(),
    };
    let text = format!("[{idx}/{n}] {label}");
    frame.render_widget(
        Paragraph::new(text).style(Style::default().add_modifier(Modifier::BOLD)),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect) {
    let text = "j/k scroll  ]f/[f file  ]h/[h hunk  L layout  z zoom  w focus  q quit";
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(FG_DIM)),
        area,
    );
}

/// Write a gap row's `··· N unchanged lines ···` marker across the FULL body width (both panes
/// and the divider column) — unlike a per-pane content row, a gap hides the same span on both
/// sides, so it isn't "about" one side or the other.
fn render_gap_row(buf: &mut Buffer, area: Rect, y: u16, skipped: usize, is_cursor: bool) {
    let msg = format!("··· {skipped} unchanged lines ···");
    let line = Line::from(TSpan::styled(msg, Style::default().fg(FG_DIM)));
    let line = if is_cursor {
        apply_cursor_row(line, area.width)
    } else {
        line
    };
    buf.set_line(area.x, y, &line, area.width);
}

fn render_body(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.files.is_empty() {
        frame.render_widget(Paragraph::new("(no changes)"), area);
        return;
    }

    let idx = app.current;
    if app.files[idx].is_binary {
        let msg = format!("[Binary file: {}]", app.files[idx].path);
        frame.render_widget(Paragraph::new(msg).style(Style::default().fg(FG_DIM)), area);
        return;
    }

    app.ensure_loaded(idx);

    // The gate re-evaluates the effective zoom for the current file every frame (no caching —
    // ratatui relayout is free, per locked decision #3).
    match app.effective_zoom_for(idx) {
        EffectiveZoom::Single(role) => {
            app.pane_height = area.height as usize;
            let scroll = app.scroll;
            let cursor = Some(app.cursor);
            match app.layout {
                AppLayout::Sbs => render_pane_sbs(frame, app, area, idx, role, scroll, cursor),
                AppLayout::Inline => {
                    render_pane_inline(frame, app, area, idx, role, scroll, cursor)
                }
            }
        }
        EffectiveZoom::Split => render_body_split(frame, app, area, idx),
    }
}

/// Render the two-pane split: unstaged pane on top, staged on the bottom, each with a dim role
/// caption, each rendering its role view in the current [`AppLayout`] with its OWN cursor+scroll —
/// the cursor highlight draws only in the focused pane. The body area splits caption(1) +
/// unstaged-content + caption(1) + staged-content, with the remainder halved between the two
/// content panes (even split).
fn render_body_split(frame: &mut Frame, app: &mut App, area: Rect, idx: usize) {
    // Too short to fit two captions plus a content line each: fall back to the focused pane alone,
    // rendered over the whole area, so the user still sees SOMETHING navigable.
    if area.height < 4 {
        let role = app.split_focus_role();
        app.pane_height = area.height as usize;
        let (scroll, cursor) = app.pane_render_state(role);
        match app.layout {
            AppLayout::Sbs => render_pane_sbs(frame, app, area, idx, role, scroll, cursor),
            AppLayout::Inline => render_pane_inline(frame, app, area, idx, role, scroll, cursor),
        }
        return;
    }

    let content_total = area.height - 2;
    let top_h = content_total / 2;
    let bot_h = content_total - top_h;

    let unstaged_caption = Rect::new(area.x, area.y, area.width, 1);
    let unstaged_content = Rect::new(area.x, area.y + 1, area.width, top_h);
    let staged_caption = Rect::new(area.x, area.y + 1 + top_h, area.width, 1);
    let staged_content = Rect::new(area.x, area.y + 2 + top_h, area.width, bot_h);

    // The focused pane owns `pane_height`; the other, `alt_height`. Both scrolls are derived here,
    // once the (render-time-only) heights are known.
    let (focused_h, unfocused_h) = if app.split_focus_role() == Role::Unstaged {
        (top_h, bot_h)
    } else {
        (bot_h, top_h)
    };
    app.pane_height = focused_h as usize;
    app.alt_height = unfocused_h as usize;
    app.derive_scroll();
    app.derive_alt_scroll();

    render_caption(frame.buffer_mut(), unstaged_caption, "UNSTAGED");
    render_caption(frame.buffer_mut(), staged_caption, "STAGED");

    let (u_scroll, u_cursor) = app.pane_render_state(Role::Unstaged);
    let (s_scroll, s_cursor) = app.pane_render_state(Role::Staged);
    match app.layout {
        AppLayout::Sbs => {
            render_pane_sbs(
                frame,
                app,
                unstaged_content,
                idx,
                Role::Unstaged,
                u_scroll,
                u_cursor,
            );
            render_pane_sbs(
                frame,
                app,
                staged_content,
                idx,
                Role::Staged,
                s_scroll,
                s_cursor,
            );
        }
        AppLayout::Inline => {
            render_pane_inline(
                frame,
                app,
                unstaged_content,
                idx,
                Role::Unstaged,
                u_scroll,
                u_cursor,
            );
            render_pane_inline(
                frame,
                app,
                staged_content,
                idx,
                Role::Staged,
                s_scroll,
                s_cursor,
            );
        }
    }
}

/// Write a split pane's role caption (`── LABEL ──`) across the pane width, styled like the dim
/// gap-row markers.
fn render_caption(buf: &mut Buffer, area: Rect, label: &str) {
    let text = format!("── {label} ──");
    let line = Line::from(TSpan::styled(text, Style::default().fg(FG_DIM)));
    buf.set_line(area.x, area.y, &line, area.width);
}

/// Render one SBS pane of `role`'s view for file `idx` into `area`, scrolled to `scroll`. The
/// cursor-row highlight draws only when `cursor` is `Some` (the focused pane) and matches a visible
/// row — a split's unfocused pane passes `None`.
#[allow(clippy::too_many_arguments)]
fn render_pane_sbs(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    idx: usize,
    role: Role,
    scroll: usize,
    cursor: Option<usize>,
) {
    let left_w = area.width.saturating_sub(1) / 2;
    let right_w = area.width.saturating_sub(1).saturating_sub(left_w);
    let hlayout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_w),
            Constraint::Length(1),
            Constraint::Length(right_w),
        ])
        .split(area);
    let old_area = hlayout[0];
    let div_area = hlayout[1];
    let new_area = hlayout[2];

    let Some(view) = app.role_view_ref(idx, role) else {
        frame.render_widget(Paragraph::new("(failed to load file)"), old_area);
        return;
    };
    let old_gutter_w = gutter_width(view.old_line_count());
    let new_gutter_w = gutter_width(view.new_line_count());
    let end = (scroll + area.height as usize).min(view.display.len());

    // Phase 1 (mutable): populate the word-span cache for visible paired rows. Phase 2 below
    // re-borrows `app`/`view` immutably to build lines — kept as the same two-phase dance the
    // spike used (see app.rs's `word_spans_for_row`/`peek_word_spans` split) rather than
    // restructured, since `FileView` lives behind `App`'s `Vec<Option<FileView>>` and the
    // borrow checker requires the cache-populating borrow to end before the line-building
    // borrow begins; there's no runtime benefit to trading that compile-time proof for
    // `RefCell` interior mutability here.
    if let Some(view) = app.role_view_mut(idx, role) {
        for row_idx in scroll..end {
            if matches!(view.display.get(row_idx), Some(DisplayRow::Row(r)) if r.is_word_diff_pair())
            {
                view.word_spans_for_row(row_idx);
            }
        }
    }

    let Some(view) = app.role_view_ref(idx, role) else {
        return;
    };

    for y in area.y..area.y + area.height {
        frame
            .buffer_mut()
            .set_string(div_area.x, y, "│", Style::default().fg(FG_DIM));
    }

    for (i, row_idx) in (scroll..end).enumerate() {
        let y = area.y + i as u16;
        let is_cursor = cursor == Some(row_idx);
        match &view.display[row_idx] {
            DisplayRow::Gap { skipped } => {
                render_gap_row(frame.buffer_mut(), area, y, *skipped, is_cursor);
            }
            DisplayRow::Row(row) => {
                let is_pair = row.is_word_diff_pair();
                let (old_words, new_words) = if is_pair {
                    view.peek_word_spans(row_idx)
                } else {
                    (Vec::new(), Vec::new())
                };

                let old_line = build_pane_line(
                    view,
                    Side::Old,
                    row.old,
                    row.old_kind,
                    &old_words,
                    is_pair,
                    BG_DEL_SUBTLE,
                    BG_DEL_STRONG,
                    old_gutter_w,
                    old_area.width as usize,
                );
                let new_line = build_pane_line(
                    view,
                    Side::New,
                    row.new,
                    row.new_kind,
                    &new_words,
                    is_pair,
                    BG_ADD_SUBTLE,
                    BG_ADD_STRONG,
                    new_gutter_w,
                    new_area.width as usize,
                );
                let (old_line, new_line) = if is_cursor {
                    (
                        apply_cursor_row(old_line, old_area.width),
                        apply_cursor_row(new_line, new_area.width),
                    )
                } else {
                    (old_line, new_line)
                };
                frame
                    .buffer_mut()
                    .set_line(old_area.x, y, &old_line, old_area.width);
                frame
                    .buffer_mut()
                    .set_line(new_area.x, y, &new_line, new_area.width);
                // The divider column was painted once for the whole pane height above, with the
                // default background; re-tint just this row's divider cell so the cursor wash
                // covers the full width (panes AND the `│` between them), like `render_gap_row`.
                if is_cursor {
                    frame.buffer_mut().set_string(
                        div_area.x,
                        y,
                        "│",
                        Style::default().fg(FG_DIM).bg(BG_CURSOR),
                    );
                }
            }
        }
    }
}

/// Right-align `n` in a field of width `w`, or blank it out (`w` spaces) when there's no lineno
/// for this side — used by the inline gutter, which always reserves both the old and new lineno
/// columns even though a `Del`/`Add` row only fills one of them.
fn gutter_field(n: Option<usize>, w: usize) -> String {
    match n {
        Some(n) => format!("{n:>w$}"),
        None => " ".repeat(w),
    }
}

/// Build a single rendered line for the inline layout's one full-width pane at a given
/// [`InlineRow`]. Context rows show BOTH the old and new lineno (there's a real line on each
/// side to number, and showing both matches the familiar unified-diff gutter convention); `Del`
/// rows show only the old-side column, `Add` rows only the new-side column — the other column is
/// blank rather than reused for anything, so a scan down the gutter reads as two honest,
/// independent line-number tracks.
fn build_inline_line(
    view: &FileView,
    row: &InlineRow,
    word_spans: &[WordSpan],
    old_gutter_w: usize,
    new_gutter_w: usize,
) -> Line<'static> {
    let (old_opt, new_opt, text, hl, kind) = match *row {
        InlineRow::Context { old, new } => (
            Some(old),
            Some(new),
            view.new_line(new),
            view.new_hl.as_ref().and_then(|v| v.get(new - 1)),
            CellKind::Context,
        ),
        InlineRow::Del { old, .. } => (
            Some(old),
            None,
            view.old_line(old),
            view.old_hl.as_ref().and_then(|v| v.get(old - 1)),
            CellKind::Del,
        ),
        InlineRow::Add { new, .. } => (
            None,
            Some(new),
            view.new_line(new),
            view.new_hl.as_ref().and_then(|v| v.get(new - 1)),
            CellKind::Add,
        ),
        InlineRow::Gap { .. } => {
            unreachable!("gap rows render via render_gap_row, not build_inline_line")
        }
    };

    let gutter = format!(
        "{} {} ",
        gutter_field(old_opt, old_gutter_w),
        gutter_field(new_opt, new_gutter_w)
    );
    let mut spans = vec![TSpan::styled(gutter, Style::default().fg(FG_GUTTER))];

    let is_word_pair = row.is_word_diff_pair();
    // `kind` is always Del/Add/Context here — inline has no Filler rows.
    let emphasis = match kind {
        CellKind::Del => Some((BG_DEL_SUBTLE, BG_DEL_STRONG)),
        CellKind::Add => Some((BG_ADD_SUBTLE, BG_ADD_STRONG)),
        CellKind::Context | CellKind::Filler => None,
    };
    spans.extend(content_spans(text, hl, emphasis, word_spans, is_word_pair));
    Line::from(spans)
}

/// Render one inline pane of `role`'s view for file `idx` into `area`, scrolled to `scroll`. See
/// [`render_pane_sbs`] for the `cursor`/highlight contract; this is its inline-coordinate-space
/// analog.
fn render_pane_inline(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    idx: usize,
    role: Role,
    scroll: usize,
    cursor: Option<usize>,
) {
    let Some(view) = app.role_view_ref(idx, role) else {
        frame.render_widget(Paragraph::new("(failed to load file)"), area);
        return;
    };
    let old_gutter_w = gutter_width(view.old_line_count());
    let new_gutter_w = gutter_width(view.new_line_count());
    let end = (scroll + area.height as usize).min(view.inline.len());

    // Same two-phase mutable/immutable dance as `render_pane_sbs`, over the inline coordinate
    // space instead.
    if let Some(view) = app.role_view_mut(idx, role) {
        for row_idx in scroll..end {
            if matches!(view.inline.get(row_idx), Some(r) if r.is_word_diff_pair()) {
                view.inline_word_spans_for_row(row_idx);
            }
        }
    }

    let Some(view) = app.role_view_ref(idx, role) else {
        return;
    };

    for (i, row_idx) in (scroll..end).enumerate() {
        let y = area.y + i as u16;
        let is_cursor = cursor == Some(row_idx);
        match &view.inline[row_idx] {
            InlineRow::Gap { skipped } => {
                render_gap_row(frame.buffer_mut(), area, y, *skipped, is_cursor);
            }
            row => {
                let (old_spans, new_spans) = if row.is_word_diff_pair() {
                    view.peek_inline_word_spans(row_idx)
                } else {
                    (Vec::new(), Vec::new())
                };
                let word_spans: &[WordSpan] = match row {
                    InlineRow::Del { .. } => &old_spans,
                    InlineRow::Add { .. } => &new_spans,
                    _ => &[],
                };
                let line = build_inline_line(view, row, word_spans, old_gutter_w, new_gutter_w);
                let line = if is_cursor {
                    apply_cursor_row(line, area.width)
                } else {
                    line
                };
                frame.buffer_mut().set_line(area.x, y, &line, area.width);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    use git_workon_fixture::prelude::*;

    use super::render;
    use crate::align::{DisplayRow, Row};
    use crate::app::test_support::app_from_fixture;
    use crate::app::App;

    fn render_once(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn cell_text(buf: &Buffer, x: u16, y: u16) -> &str {
        buf.cell((x, y)).unwrap().symbol()
    }

    fn buf_lines(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| cell_text(buf, x, y)).collect())
            .collect()
    }

    #[test]
    fn small_modified_file_shows_gap_hunk_and_word_diff() {
        // 12 lines of context around a single changed word, with more than 2*CONTEXT_LINES of
        // untouched lines both before and after so a gap collapses on both edges.
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nold word here\nl10\nl11\nl12\nl13\nl14\n";
        let new = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nnew word here\nl10\nl11\nl12\nl13\nl14\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", old, new)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        // `open_current` jumps the viewport straight to the first hunk row (the initial scroll
        // behavior CS4 requires), so the leading gap before the hunk scrolls out of view — only
        // the trailing gap (after the hunk, before EOF) stays visible at the top of a
        // full-height render.
        app.open_current();
        let buf = render_once(&mut app, 60, 20);

        let content = buf_lines(&buf);

        assert!(
            content.iter().any(|line| line.contains("unchanged lines")),
            "expected a collapsed gap row, got:\n{}",
            content.join("\n")
        );
        assert!(
            content.iter().any(|line| line.contains("old word here")),
            "expected the old-side changed line, got:\n{}",
            content.join("\n")
        );
        assert!(
            content.iter().any(|line| line.contains("new word here")),
            "expected the new-side changed line, got:\n{}",
            content.join("\n")
        );

        // Word-diff emphasis: the changed word ("old"/"new") on the paired row should carry a
        // strong background distinct from the rest of the line's subtle background.
        let changed_row_y = content
            .iter()
            .position(|line| line.contains("old word here"))
            .expect("changed row present") as u16;
        // Gutter width 3 + 1 space = column 4 is where "old" starts.
        let word_cell = buf.cell((4, changed_row_y)).unwrap();
        // l10/l11/l12 are the kept-context lines immediately after the hunk (before the
        // trailing gap collapses l13/l14).
        let ctx_row_y = content
            .iter()
            .position(|line| line.contains("l10 "))
            .expect("context row present") as u16;
        let ctx_cell = buf.cell((4, ctx_row_y)).unwrap();
        assert_ne!(
            word_cell.style().bg,
            ctx_cell.style().bg,
            "expected the word-diff row to carry a background style distinct from plain context"
        );

        // The changed word ("old", bytes 0..3 → columns 4..7) must carry the STRONG emphasis
        // while the unchanged remainder of the same paired line ("word here", from column 8)
        // stays subtle — three distinct backgrounds: strong word, subtle line, unstyled
        // context. This pins the compositor's span precedence (specific-over-whole-line); a
        // first-match lookup renders the whole line subtle and only the ctx comparison above
        // would still pass.
        let rest_cell = buf.cell((8, changed_row_y)).unwrap();
        assert_ne!(
            word_cell.style().bg,
            rest_cell.style().bg,
            "expected the changed word's strong bg to differ from the line's subtle bg"
        );
        assert_ne!(
            rest_cell.style().bg,
            ctx_cell.style().bg,
            "expected the paired line's subtle bg to differ from plain context"
        );
    }

    #[test]
    fn binary_file_shows_placeholder() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("bin.dat", "hello\n")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();
        std::fs::write(repo.workdir().unwrap().join("bin.dat"), [0u8, 1, 2, 0, 3]).unwrap();

        let mut app = app_from_fixture(&fixture);
        let buf = render_once(&mut app, 60, 10);

        let content = buf_lines(&buf);
        assert!(
            content
                .iter()
                .any(|line| line.contains("[Binary file: bin.dat]")),
            "expected binary placeholder, got:\n{}",
            content.join("\n")
        );
    }

    #[test]
    fn deleted_file_renders_one_sided() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .deleted_file("gone.txt", "line one\nline two\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let buf = render_once(&mut app, 60, 10);

        let content = buf_lines(&buf);
        assert!(
            content.iter().any(|line| line.contains("line one")),
            "expected old-side deleted content, got:\n{}",
            content.join("\n")
        );
        // New (right) pane has nothing to show for a wholly deleted file: every visible row is
        // filler on that side. Filler renders as a repeated '╱' run — assert the right half of
        // at least one changed row is filler, not "line one"/"line two" text.
        let left_w = (buf.area.width.saturating_sub(1)) / 2;
        let right_x = left_w + 1;
        let row_with_content = content
            .iter()
            .position(|line| line.contains("line one"))
            .expect("row with old content present");
        let right_cell = cell_text(&buf, right_x, row_with_content as u16);
        assert_eq!(
            right_cell, "╱",
            "expected filler on the new-side pane for a deleted file"
        );
    }

    #[test]
    fn renamed_file_header_shows_old_path_and_base() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("old_name.txt", "same content\n", "same content\n")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();
        let workdir = repo.workdir().unwrap();
        std::fs::rename(workdir.join("old_name.txt"), workdir.join("new_name.txt")).unwrap();

        let mut app = app_from_fixture(&fixture);
        let buf = render_once(&mut app, 80, 10);

        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        assert!(
            header.contains("old_name.txt @ HEAD -> new_name.txt"),
            "expected renamed header with old_path @ base -> new_path, got: {header:?}"
        );
    }

    #[test]
    fn toggling_layout_reflows_the_same_fixture_and_toggling_back_restores_sbs() {
        use crate::app::Layout;

        let old = "l1\nl2\nl3\nl4\nl5\nold word here\nl7\nl8\nl9\nl10\n";
        let new = "l1\nl2\nl3\nl4\nl5\nnew word here\nl7\nl8\nl9\nl10\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", old, new)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();

        // SBS: old and new side by side on the SAME row.
        let sbs_buf = render_once(&mut app, 60, 20);
        let sbs_content = buf_lines(&sbs_buf);
        let sbs_row = sbs_content
            .iter()
            .position(|line| line.contains("old word here"))
            .expect("SBS row pairs old and new on one row");
        assert!(
            sbs_content[sbs_row].contains("new word here"),
            "expected SBS to show del and add on the same row, got:\n{}",
            sbs_content.join("\n")
        );

        app.toggle_layout();
        assert_eq!(app.layout, Layout::Inline);

        let inline_buf = render_once(&mut app, 60, 20);
        let inline_content = buf_lines(&inline_buf);
        let del_row = inline_content
            .iter()
            .position(|line| line.contains("old word here"))
            .expect("inline shows the deleted line");
        let add_row = inline_content
            .iter()
            .position(|line| line.contains("new word here"))
            .expect("inline shows the added line");
        assert!(
            del_row < add_row,
            "expected the inline del line above its paired add line, got:\n{}",
            inline_content.join("\n")
        );
        assert_ne!(
            del_row, add_row,
            "del and add must be on separate rows in inline layout"
        );

        // Toggling back re-renders SBS (single row again) rather than staying stuck in inline.
        app.toggle_layout();
        assert_eq!(app.layout, Layout::Sbs);
        let sbs_again = render_once(&mut app, 60, 20);
        let sbs_again_content: Vec<String> = (0..sbs_again.area.height)
            .map(|y| {
                (0..sbs_again.area.width)
                    .map(|x| cell_text(&sbs_again, x, y))
                    .collect::<String>()
            })
            .collect();
        let row = sbs_again_content
            .iter()
            .position(|line| line.contains("old word here"))
            .expect("SBS (again) row pairs old and new on one row");
        assert!(
            sbs_again_content[row].contains("new word here"),
            "expected toggling back to re-render SBS with del/add on one row, got:\n{}",
            sbs_again_content.join("\n")
        );
    }

    #[test]
    fn cursor_row_carries_a_distinct_bg_tint_in_both_sbs_panes() {
        // Two plain context rows (l10, l11) well clear of the hunk's own del/add emphasis — the
        // cursor tint must be visible on its own, not riding on top of an already-colored row.
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nold word here\nl10\nl11\nl12\nl13\nl14\n";
        let new = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nnew word here\nl10\nl11\nl12\nl13\nl14\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", old, new)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();

        // Move the cursor onto the context row that will render as "l10 " — its display index
        // is the row whose old-side line number is 10.
        let cursor_row = app
            .current_view_ref()
            .unwrap()
            .display
            .iter()
            .position(|row| matches!(row, DisplayRow::Row(r) if r.old == Row::Line(10)))
            .expect("l10 row present in the display vector");
        app.cursor = cursor_row;

        let buf = render_once(&mut app, 60, 20);
        let content = buf_lines(&buf);

        let cursor_y = content
            .iter()
            .position(|line| line.contains("l10 "))
            .expect("cursor row (l10) visible") as u16;
        // A DIFFERENT context row, not under the cursor, for comparison — same row "kind"
        // (plain context) so any bg difference is attributable to the cursor tint alone.
        let other_y = content
            .iter()
            .position(|line| line.contains("l11 "))
            .expect("comparison context row (l11) visible") as u16;

        // Left (old) pane: gutter column (x=1) is well inside both the gutter and content.
        let left_cursor_bg = buf.cell((1, cursor_y)).unwrap().style().bg;
        let left_other_bg = buf.cell((1, other_y)).unwrap().style().bg;
        assert_ne!(
            left_cursor_bg, left_other_bg,
            "expected the cursor row's LEFT pane to carry a background distinct from a \
             non-cursor context row"
        );

        // Right (new) pane: same check, at a column past the divider.
        let left_w = (buf.area.width.saturating_sub(1)) / 2;
        let right_x = left_w + 2; // skip the divider column, land inside the right pane's gutter
        let right_cursor_bg = buf.cell((right_x, cursor_y)).unwrap().style().bg;
        let right_other_bg = buf.cell((right_x, other_y)).unwrap().style().bg;
        assert_ne!(
            right_cursor_bg, right_other_bg,
            "expected the cursor row's RIGHT pane to carry a background distinct from a \
             non-cursor context row too"
        );

        // The single `│` divider column between the panes must ALSO carry the cursor wash —
        // otherwise a dark seam splits the highlight down the middle of every SBS cursor row.
        let divider_x = left_w;
        assert_eq!(
            cell_text(&buf, divider_x, cursor_y),
            "│",
            "sanity: located the divider column between the two panes"
        );
        assert_eq!(
            buf.cell((divider_x, cursor_y)).unwrap().style().bg,
            Some(super::BG_CURSOR),
            "expected the cursor row's DIVIDER cell to carry the cursor background, not the \
             default — otherwise the highlight has a seam through the middle"
        );
    }

    #[test]
    fn cursor_row_tint_composites_with_word_diff_emphasis_rather_than_replacing_it() {
        // The cursor starts on the file's first hunk (a word-diff paired row) after
        // `open_current` — confirm the strong word-level bg and the whole-line subtle bg on that
        // SAME row both stay visually distinct from each other even with the cursor tint
        // layered on top, i.e. the tint composites rather than flattening the existing emphasis.
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nold word here\nl10\nl11\nl12\nl13\nl14\n";
        let new = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nnew word here\nl10\nl11\nl12\nl13\nl14\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", old, new)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let buf = render_once(&mut app, 60, 20);
        let content = buf_lines(&buf);
        let changed_row_y = content
            .iter()
            .position(|line| line.contains("old word here"))
            .expect("changed row present") as u16;

        // Gutter width 3 + 1 space = column 4 is where "old" (the changed word) starts; column 8
        // is the unchanged remainder of the same line ("word here").
        let word_bg = buf.cell((4, changed_row_y)).unwrap().style().bg;
        let rest_bg = buf.cell((8, changed_row_y)).unwrap().style().bg;
        assert_ne!(
            word_bg, rest_bg,
            "the cursor tint must not flatten the word-diff strong/subtle distinction on its \
             own row"
        );
    }

    #[test]
    fn split_renders_both_role_captions_stacked_with_content_in_each_pane() {
        // A partially-staged file has both a staged (HEAD ↔ index) and an unstaged (index ↔
        // worktree) sub-diff, so the default split renders two stacked panes.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .partially_staged_file(
                "f.txt",
                "alpha\nbeta\ngamma\n",
                "alpha\nBETAEDIT\ngamma\n",
                "alpha\nBETAEDIT\nGAMMAEDIT\n",
            )
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let buf = render_once(&mut app, 80, 24);
        let content = buf_lines(&buf);

        let unstaged_cap = content
            .iter()
            .position(|line| line.contains("UNSTAGED"))
            .expect("unstaged caption present");
        let staged_cap = content
            .iter()
            .position(|line| line.contains("STAGED") && !line.contains("UNSTAGED"))
            .expect("staged caption present");
        assert!(
            unstaged_cap < staged_cap,
            "the unstaged pane's caption must sit above the staged pane's, got:\n{}",
            content.join("\n")
        );

        // Both panes actually render their file (the shared context line `alpha` shows up once
        // per pane) — one below each caption.
        assert!(
            content[unstaged_cap + 1..staged_cap]
                .iter()
                .any(|line| line.contains("alpha")),
            "expected file content under the unstaged caption, got:\n{}",
            content.join("\n")
        );
        assert!(
            content[staged_cap + 1..]
                .iter()
                .any(|line| line.contains("alpha")),
            "expected file content under the staged caption, got:\n{}",
            content.join("\n")
        );
    }

    #[test]
    fn single_pane_zoom_is_identical_to_combined_for_an_unstaged_only_file() {
        // The common case: a dirty-but-unstaged file. The default split gate downgrades it to a
        // single unstaged pane, whose view is byte-for-byte the combined view (index == HEAD when
        // nothing is staged) — so a user who never presses `z` sees exactly the pre-zoom app.
        let old = "l1\nl2\nl3\nl4\nl5\nold word here\nl7\nl8\nl9\nl10\n";
        let new = "l1\nl2\nl3\nl4\nl5\nnew word here\nl7\nl8\nl9\nl10\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", old, new)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let default_buf = render_once(&mut app, 60, 20);

        // No split chrome leaks into the single-pane render.
        for line in buf_lines(&default_buf) {
            assert!(
                !line.contains("UNSTAGED") && !line.contains("STAGED"),
                "single-pane render must not show a split caption, got line: {line:?}"
            );
        }

        // Explicitly zoom to Combined and re-render — must be pixel-identical.
        app.cycle_zoom();
        assert_eq!(app.zoom, crate::app::Zoom::Combined);
        let combined_buf = render_once(&mut app, 60, 20);
        assert_eq!(
            default_buf, combined_buf,
            "the default (downgraded-to-unstaged) render must match the combined-zoom render \
             cell-for-cell for an unstaged-only file"
        );
    }
}
