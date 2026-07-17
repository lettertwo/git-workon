//! Frame rendering: per-pane headers, side-by-side diff body, footer.
//!
//! Ported from the `review-tui-spike` prototype's `ui.rs`, adapted to render [`App`]'s
//! gap-collapsed [`crate::align::DisplayRow`]s instead of a flat aligned-row list, and extended
//! with a full-width `Gap` row (the collapsed-context marker is the same on both sides, so it
//! spans the whole body rather than living in one pane).

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::align::{CellKind, DisplayRow, InlineRow, Row};
use crate::app::{
    App, EffectiveZoom, FileView, Layout as AppLayout, Notice, Region, Role, Severity, Summary,
};
use crate::attribute::Attribution;
use crate::config::View;
use crate::highlight::FgSpan;
use crate::icons::IconMode;
use crate::keymap::{footer_hint, help_sections, Keymap};
use crate::model::FileStatus;
use crate::outline::OutlineItem;
use crate::summary::{ChangesetSummary, DirSummary, SummaryFileRow};
use crate::theme::Palette;
use crate::wordiff::Span as WordSpan;

// The on-tint colors (diff add/del gradient + staged variants, cursor/selection washes, and syntax
// foreground) come from a [`Palette`] threaded through render (ADR-029). The canvas background and
// default/dim/gutter chrome foreground ALSO now come from the palette (`theme.background`/
// `theme.foreground`/`theme.dim`/`theme.gutter`), as does the semantic chrome that used to be
// const here — error/warn/current-marker are now `theme.error_fg`/`theme.warn_fg`/
// `theme.current_fg` (CS2, revising ADR-029's hybrid boundary) — see the theme module's revised
// hybrid-boundary doc comment. A curated theme now fully controls the look; nothing in this
// module hardcodes a semantic color anymore.

// CS3's nerd-mode status/header/summary glyphs (gated on `IconMode::Nerd`; the plain unicode
// defaults below stay byte-identical when `icons = none` — see icons.rs's module doc for why no
// auto-detection ever picks Nerd for the user). Picked from the classic BMP nerd-font sets
// (`fa`/`oct`) rather than devicons' broader (partly supplementary-plane) table, for wider
// font compatibility — see `icons.rs`'s v3 doc note.
/// Nerd-mode "this is the current changeset" marker, replacing the plain `•` (U+2022).
const NERD_CURRENT_MARKER: char = '\u{f444}'; // nf-oct-dot-fill
/// Nerd-mode needs-restack marker, replacing the plain `⚠` (U+26A0).
const NERD_WARN_MARKER: char = '\u{f071}'; // nf-fa-warning
/// Nerd-mode failed-changeset marker, replacing the plain `✗` (U+2717).
const NERD_ERROR_MARKER: char = '\u{f00d}'; // nf-fa-times
/// Nerd-mode loading marker, replacing the plain `…` (U+2026).
const NERD_LOADING_MARKER: char = '\u{f141}'; // nf-fa-ellipsis-h
/// Nerd-mode branch glyph prepended to a changeset header row's title (both the outline's Header
/// row and the summary panel's changeset title) — purely decorative (dim-colored), so it carries
/// no semantic color of its own.
const NERD_BRANCH_ICON: char = '\u{f418}'; // nf-oct-git-branch
/// Nerd-mode diffstat glyph for the summary panel's added-lines count, replacing the plain `+`.
const NERD_DIFF_ADDED: char = '\u{f457}'; // nf-oct-diff-added
/// Nerd-mode diffstat glyph for the summary panel's deleted-lines count, replacing the plain `-`.
const NERD_DIFF_REMOVED: char = '\u{f458}'; // nf-oct-diff-removed

/// The current-changeset marker for the active icon strategy. These four one-switch helpers are
/// the single source of each semantic marker's glyph pair — the outline's Header arm, the summary
/// panel, and the diff/outline pane headers (CS1, `pane-headers`) deliberately draw the SAME
/// markers, so the selection lives in one place instead of a hand-synced `match` per call site.
fn current_marker(icons: IconMode) -> char {
    match icons {
        IconMode::Nerd => NERD_CURRENT_MARKER,
        IconMode::None => '\u{2022}',
    }
}

/// The needs-restack marker for the active icon strategy (see [`current_marker`]).
fn warn_marker(icons: IconMode) -> char {
    match icons {
        IconMode::Nerd => NERD_WARN_MARKER,
        IconMode::None => '\u{26A0}',
    }
}

/// The failed-changeset marker for the active icon strategy (see [`current_marker`]).
fn error_marker(icons: IconMode) -> char {
    match icons {
        IconMode::Nerd => NERD_ERROR_MARKER,
        IconMode::None => '\u{2717}',
    }
}

/// The loading marker for the active icon strategy (see [`current_marker`]).
fn loading_marker(icons: IconMode) -> char {
    match icons {
        IconMode::Nerd => NERD_LOADING_MARKER,
        IconMode::None => '\u{2026}',
    }
}

/// The diffstat `+`/`-` prefixes for the active icon strategy (nerd: the oct diff glyphs) —
/// shared by the summary panel's totals line and any other diffstat surface.
fn diffstat_prefixes(icons: IconMode) -> (String, String) {
    match icons {
        IconMode::Nerd => (
            format!("{NERD_DIFF_ADDED} "),
            format!("{NERD_DIFF_REMOVED} "),
        ),
        IconMode::None => ("+".to_string(), "-".to_string()),
    }
}

/// The pane headers' bold `  +A -D` diffstat span run (leading two-space spacer included) —
/// single-sourced for [`render_outline_header`] (changeset total) and [`file_segment_spans`]
/// (per-file), so the styling (spacing, boldness, glyph prefixes) can't drift between the two.
/// The summary panel's totals line deliberately keeps its own non-bold variant
/// ([`push_summary_body`]).
fn diffstat_spans(
    adds: usize,
    dels: usize,
    theme: &Palette,
    icons: IconMode,
) -> Vec<TSpan<'static>> {
    let (added_prefix, removed_prefix) = diffstat_prefixes(icons);
    vec![
        TSpan::styled("  ".to_string(), Style::default().fg(theme.foreground)),
        TSpan::styled(
            format!("{added_prefix}{adds}"),
            Style::default()
                .fg(theme.add_strong)
                .add_modifier(Modifier::BOLD),
        ),
        TSpan::styled(" ".to_string(), Style::default().fg(theme.foreground)),
        TSpan::styled(
            format!("{removed_prefix}{dels}"),
            Style::default()
                .fg(theme.del_strong)
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

/// The shared changeset-title span run — `[current-marker] [branch-icon] ([i/n] )label
/// [warn-marker]` — drawn by both `build_outline_line`'s Header arm and
/// [`changeset_summary_lines`]. **The two call sites no longer render identically** (CS1,
/// `outline-header-polish`): `counter` is `Some((cs_idx + 1, n))` for the outline's Header row
/// only, and its presence ALSO switches the label from the plain [`Palette::foreground`] look to
/// [`Palette::heading_fg`] + bold — the summary panel passes `None` and keeps the original
/// foreground-bold label with no counter, matching its pre-CS1 appearance exactly. Failed/loading
/// markers are still NOT included: the two call sites place them differently (trailing spans on
/// the header row vs. a line of their own in the summary).
fn changeset_title_spans(
    label: &str,
    current: bool,
    needs_restack: bool,
    theme: &Palette,
    icons: IconMode,
    counter: Option<(usize, usize)>,
) -> Vec<TSpan<'static>> {
    let mut spans = Vec::new();
    if current {
        spans.push(TSpan::styled(
            format!("{} ", current_marker(icons)),
            Style::default().fg(theme.current_fg),
        ));
    }
    if icons == IconMode::Nerd {
        spans.push(TSpan::styled(
            format!("{NERD_BRANCH_ICON} "),
            Style::default().fg(theme.dim),
        ));
    }
    // The `[i/n]` counter and the accented label are outline-only (`counter.is_some()`) — see
    // this fn's doc comment for why the summary panel's `None` call site is unaffected.
    let label_fg = if counter.is_some() {
        theme.heading_fg
    } else {
        theme.foreground
    };
    if let Some((i, n)) = counter {
        spans.push(TSpan::styled(
            format!("[{i}/{n}] "),
            Style::default().fg(theme.dim),
        ));
    }
    spans.push(TSpan::styled(
        label.to_string(),
        Style::default().fg(label_fg).add_modifier(Modifier::BOLD),
    ));
    if needs_restack {
        spans.push(TSpan::styled(
            format!(" {}", warn_marker(icons)),
            Style::default().fg(theme.warn_fg),
        ));
    }
    spans
}

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

/// Blend `tint` into an already-built line's background: mix it into every span's bg (see
/// [`blend_bg`]), then pad out to `width` with solid tint so the highlight covers the full row even
/// past the line's own rendered content (a short line, or one pane of a filler/deleted-file row,
/// would otherwise leave the tail of the row unhighlighted). Shared by the cursor and selection
/// row washes — they differ only in the tint color.
fn apply_row_tint(mut line: Line<'static>, width: u16, tint: Color) -> Line<'static> {
    for span in &mut line.spans {
        let bg = blend_bg(span.style.bg, tint);
        span.style = span.style.bg(bg);
    }
    let used = line.width() as u16;
    if used < width {
        line.spans.push(TSpan::styled(
            " ".repeat((width - used) as usize),
            Style::default().bg(tint),
        ));
    }
    line
}

/// The cursor row's tint — full [`Palette::cursor_bg`] when `focused` is true (this pane holds
/// focus), or the dimmer [`Palette::cursor_unfocused_bg`] otherwise. Shared by [`apply_cursor_row`]
/// and `render_pane_sbs`'s divider-cell re-tint so the row wash and the divider it crosses never
/// drift apart.
fn cursor_tint(theme: &Palette, focused: bool) -> Color {
    if focused {
        theme.cursor_bg
    } else {
        theme.cursor_unfocused_bg
    }
}

/// Wash the cursor row with the theme's cursor tint — full [`Palette::cursor_bg`] when `focused`
/// is true (this pane holds focus), or the dimmer [`Palette::cursor_unfocused_bg`] otherwise (CS1,
/// `unfocused-cursor-wash`: the uniform model every pane's remembered cursor row now follows,
/// matching the outline's pre-existing focused/unfocused split).
fn apply_cursor_row(
    line: Line<'static>,
    width: u16,
    theme: &Palette,
    focused: bool,
) -> Line<'static> {
    apply_row_tint(line, width, cursor_tint(theme, focused))
}

/// Wash a selected (line-selection) row with the theme's selection tint.
fn apply_selection_row(line: Line<'static>, width: u16, theme: &Palette) -> Line<'static> {
    apply_row_tint(line, width, theme.selection_bg)
}

/// Horizontal-scroll right-edge marker (decision #7): if `line` (as already blitted into `area`
/// by the caller's `set_line`) is wider than `area`'s content width, overwrite the pane's last
/// cell with a dim `…` so a panned-right line still signals there's more to the right. Applied
/// AFTER `set_line` (and after any cursor/selection wash, which paints its own background first)
/// so the marker survives on a cursor row — `Buffer::set_string`'s `Cell::set_style` only
/// overwrites `fg` when the given style sets it (leaves `bg` untouched when it doesn't, per
/// ratatui's `Style::patch` semantics), so this only ever changes the glyph + foreground, never
/// erasing the wash underneath.
fn apply_right_edge_marker(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    line: &Line<'static>,
    theme: &Palette,
) {
    if area.width == 0 {
        return;
    }
    if line.width() > area.width as usize {
        let x = area.x + area.width - 1;
        buf.set_string(x, y, HSCROLL_MARKER, Style::default().fg(theme.dim));
    }
}

/// One resolved (bg, fg) pair for a byte range of a line.
struct Segment {
    start: usize,
    end: usize,
    bg: Option<Color>,
    fg: Color,
}

/// Merge background-role spans and syntax fg spans into a flat list of non-overlapping
/// segments covering `[0, len)`. A syntax span carries only its capture index; its color is
/// resolved HERE against `theme` (ADR-029's render-time resolution) — a segment with no covering
/// syntax span falls back to [`Palette::foreground`].
fn compose_segments(
    len: usize,
    bg_spans: &[(usize, usize, Color)],
    fg_spans: Option<&Vec<FgSpan>>,
    theme: &Palette,
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
            .map(|s| theme.syntax(s.capture))
            .unwrap_or(theme.foreground);
        segments.push(Segment { start, end, bg, fg });
    }
    segments
}

fn gutter_width(max_lineno: usize) -> usize {
    max_lineno.to_string().len().max(3)
}

/// How a rendered pane resolves a changed cell's (subtle, strong) background pair — one per
/// [`Role`] (locked decision #7): the combined view is the only one that needs a per-cell lookup,
/// since it's the only role that fuses staged and unstaged content into one set of rows.
#[derive(Clone, Copy)]
enum AttributionMode<'a> {
    /// Combined view: look up each cell's staged-ness in the given [`Attribution`], built fresh
    /// for the current file this frame (see [`combined_attribution`]).
    Attributed(&'a Attribution),
    /// Unstaged zoom pane: every changed cell IS the not-yet-staged set — render bright,
    /// unconditionally (today's plain colors).
    Plain,
    /// Staged zoom pane (single-zoom or the split's bottom pane): every changed cell IS already
    /// staged — render dim, unconditionally.
    StagedUniform,
}

/// Build the current file's [`Attribution`] when rendering the combined role, `None` for the
/// unstaged/staged roles (which don't need a per-cell lookup — see [`AttributionMode`]). Computed
/// fresh from the sub-models on every call rather than cached on `App`: cheap (O(hunk lines) on
/// one file) and always correct even if the index changes between frames (the M4 watcher's
/// concern, not this one's, but the cost of getting it wrong is a stale color).
fn combined_attribution(app: &App, idx: usize, role: Role) -> Option<Attribution> {
    // A committed changeset's combined role is the whole `base..head` range, not a fusion of
    // staged/unstaged sets — there's nothing to attribute (locked decision #2's "skip
    // attribution" guard). Every cell renders as plain, undifferentiated change.
    if role != Role::Combined || app.is_committed() {
        return None;
    }
    let unstaged = app.role_change(idx, Role::Unstaged);
    let staged = app.role_change(idx, Role::Staged);
    Some(Attribution::build(unstaged, staged))
}

/// Resolve the [`AttributionMode`] to render `role` with, given the (possibly absent)
/// [`Attribution`] built by [`combined_attribution`] — absent for a non-combined role, OR for a
/// committed changeset's combined role (see that function's doc comment), in which case combined
/// renders [`AttributionMode::Plain`] rather than panicking.
fn attribution_mode(role: Role, attribution: &Option<Attribution>) -> AttributionMode<'_> {
    match (role, attribution) {
        (Role::Combined, Some(a)) => AttributionMode::Attributed(a),
        (Role::Combined, None) => AttributionMode::Plain,
        (Role::Unstaged, _) => AttributionMode::Plain,
        (Role::Staged, _) => AttributionMode::StagedUniform,
    }
}

/// The (subtle, strong) background pair for a Del cell at `old_lnum`, given `mode`, resolved from
/// `theme`'s bright vs. staged Del tints.
fn del_bg_pair(mode: AttributionMode, old_lnum: u32, theme: &Palette) -> (Color, Color) {
    let bright = (theme.del_subtle, theme.del_strong);
    let staged = (theme.del_staged_subtle, theme.del_staged_strong);
    match mode {
        AttributionMode::Plain => bright,
        AttributionMode::StagedUniform => staged,
        AttributionMode::Attributed(attribution) => {
            if attribution.del_is_staged(old_lnum) {
                staged
            } else {
                bright
            }
        }
    }
}

/// The (subtle, strong) background pair for an Add cell at `new_lnum`, given `mode`, resolved from
/// `theme`'s bright vs. staged Add tints.
fn add_bg_pair(mode: AttributionMode, new_lnum: u32, theme: &Palette) -> (Color, Color) {
    let bright = (theme.add_subtle, theme.add_strong);
    let staged = (theme.add_staged_subtle, theme.add_staged_strong);
    match mode {
        AttributionMode::Plain => bright,
        AttributionMode::StagedUniform => staged,
        AttributionMode::Attributed(attribution) => {
            if attribution.add_is_unstaged(new_lnum) {
                bright
            } else {
                staged
            }
        }
    }
}

/// Which side of the aligned pair a pane line is being built for — determines which of
/// [`FileView`]'s two parallel (text, highlight) sources to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

/// Horizontal-scroll left-edge marker (decision #7): replaces the first visible content column
/// whenever a line actually had content panned off to the left. Dim-styled like the gap-row/
/// filler markers — no new color, just `theme.dim` on the existing `…` glyph.
const HSCROLL_MARKER: &str = "…";

/// Find the byte offset that cuts `text` at display column `col` (0 for `col == 0`), for
/// [`content_spans`]'s horizontal-scroll slicing. Column, not byte, is the unit `App::hscroll`
/// counts in, so this walks chars accumulating [`UnicodeWidthChar`] widths rather than indexing
/// `text` directly — indexing by column count would panic on a non-char-boundary byte offset for
/// any multibyte UTF-8 line.
///
/// Returns `(byte_offset, pad)`: `pad` is `true` when a wide (2-column) char straddles the cut —
/// e.g. `col` lands mid-CJK-glyph — in which case that char is dropped entirely (skipping it
/// half-visible would misalign every column after it) and the caller should prepend a one-column
/// space to keep alignment. `col` at or beyond the line's total width returns `(text.len(), false)`
/// (nothing left to show).
fn hscroll_cut(text: &str, col: usize) -> (usize, bool) {
    if col == 0 {
        return (0, false);
    }
    let mut acc = 0usize;
    for (i, c) in text.char_indices() {
        if acc >= col {
            return (i, false);
        }
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if acc + w > col {
            return (i + c.len_utf8(), true);
        }
        acc += w;
    }
    (text.len(), false)
}

/// Pan an already-built line of styled spans (diff content, or — as of the mouse/outline
/// follow-up — an outline row) `cols` display columns to the left. The shared core
/// [`content_spans`]/`render::render_outline` both build their spans at FULL width first, then
/// apply this — never the other way around — so every existing style/segment computation
/// (word-diff spans, syntax highlight, outline icon/label coloring) stays untouched by hscroll;
/// this function only ever drops or re-slices spans, never recolors one.
///
/// Walks `spans` in order with a running column budget (`cols`, plus one extra reserved for the
/// left-edge marker below): a per-span [`hscroll_cut`] call consumes as much of that budget as
/// the span's own display width allows, carrying any remainder into the next span — exactly as
/// if `hscroll_cut` had been called once over the whole line's concatenated text, since spans
/// partition that text contiguously and in original order. A wide char straddling the cut is
/// dropped whole (never half-rendered) and compensated with a one-column space pad, same as
/// [`hscroll_cut`]'s own doc comment describes for a single string. Once the budget reaches `0`,
/// every remaining span is pushed through unchanged.
///
/// When `cols == 0`, or the line has no content at all to cut, `spans` passes through unchanged
/// (no marker, no pad) — matching [`hscroll_cut`]'s own "nothing to show" cases.
fn pan_spans(spans: Vec<TSpan<'static>>, cols: usize, theme: &Palette) -> Vec<TSpan<'static>> {
    if cols == 0 {
        return spans;
    }
    if spans.iter().all(|s| s.content.is_empty()) {
        return spans;
    }

    // Reserve one extra column for the left-edge marker (decision #7's affordance) — mirrors the
    // pre-refactor `content_spans`' own "cut at `hscroll`, then one column further for the
    // marker" two-step.
    let mut skip = cols + 1;
    let mut out = Vec::with_capacity(spans.len() + 2);
    out.push(TSpan::styled(
        HSCROLL_MARKER.to_string(),
        Style::default().fg(theme.dim),
    ));

    for span in spans {
        if skip == 0 {
            out.push(span);
            continue;
        }
        let text = span.content.as_ref();
        let (cut, straddled) = hscroll_cut(text, skip);
        if straddled || cut < text.len() {
            // The remaining budget was fully spent inside this span — everything from `cut`
            // onward (possibly nothing) survives, unchanged in style.
            skip = 0;
            if straddled {
                out.push(TSpan::styled(
                    " ".to_string(),
                    Style::default().fg(theme.foreground),
                ));
            }
            if cut < text.len() {
                out.push(TSpan::styled(text[cut..].to_string(), span.style));
            }
        } else {
            // The whole span fit inside the remaining budget — drop it and keep consuming.
            skip = skip.saturating_sub(UnicodeWidthStr::width(text));
        }
    }
    out
}

/// Build the styled content spans (everything after the gutter) for one line of text, shared by
/// [`build_pane_line`] (SBS) and [`build_inline_line`] (inline) — the two differ only in how they
/// resolve `text`/`hl`/`emphasis` from a [`Row`] vs an [`InlineRow`] and in their gutter, not in
/// how a resolved line gets colored.
///
/// `emphasis` is `Some((subtle, strong))` for a `Del`/`Add` line (whole-line subtle background,
/// plus per-`word_spans` strong background when `is_word_pair`; whole-line strong when not paired
/// — an unpaired excess line) and `None` for `Context`/`Filler` (no background emphasis at all).
///
/// `hscroll` (display columns, [`App::hscroll`]) pans the returned spans via [`pan_spans`] — the
/// segments below are always composed over the FULL, unsliced `text` first (byte-identical to the
/// pre-hscroll behavior), and [`pan_spans`] applies the cut/pad/marker afterward.
#[allow(clippy::too_many_arguments)]
fn content_spans(
    text: &str,
    hl: Option<&Vec<FgSpan>>,
    emphasis: Option<(Color, Color)>,
    word_spans: &[WordSpan],
    is_word_pair: bool,
    theme: &Palette,
    hscroll: usize,
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

    let segments = compose_segments(text.len(), &bg_spans, hl, theme);
    let mut spans = Vec::with_capacity(segments.len().max(1));
    if segments.is_empty() && !text.is_empty() {
        spans.push(TSpan::styled(
            text.to_string(),
            Style::default().fg(theme.foreground),
        ));
    }
    for seg in segments {
        let mut style = Style::default().fg(seg.fg);
        if let Some(bg) = seg.bg {
            style = style.bg(bg);
        }
        spans.push(TSpan::styled(text[seg.start..seg.end].to_string(), style));
    }
    pan_spans(spans, hscroll, theme)
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
    mode: AttributionMode,
    gutter_w: usize,
    content_w: usize,
    theme: &Palette,
    hscroll: usize,
) -> Line<'static> {
    match row {
        Row::Filler => {
            let pattern: String = "╱".repeat(content_w + gutter_w + 1);
            Line::from(TSpan::styled(pattern, Style::default().fg(theme.dim)))
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
            let mut spans = vec![TSpan::styled(gutter, Style::default().fg(theme.gutter))];

            let emphasis = match kind {
                CellKind::Del => Some(del_bg_pair(mode, n as u32, theme)),
                CellKind::Add => Some(add_bg_pair(mode, n as u32, theme)),
                CellKind::Context | CellKind::Filler => None,
            };
            spans.extend(content_spans(
                text,
                hl,
                emphasis,
                word_spans,
                is_word_pair,
                theme,
                hscroll,
            ));
            Line::from(spans)
        }
    }
}

/// Render one frame: SBS body (each pane painting its own 1-row header — CS1, `pane-headers`;
/// there's no more global header/winbar row), footer, and (when [`App::help_visible`]) the `?`
/// overlay on top of everything else. `keymap` is the resolved, possibly-rebound keymap — the
/// footer hint and help overlay render its ACTUAL bindings (see [`crate::keymap::footer_hint`]/
/// [`crate::keymap::help_sections`]), never a hardcoded key string. `theme` is the resolved
/// on-tint palette — see [`crate::theme`]; the diff body, syntax foreground, and cursor/selection
/// washes all resolve their colors against it at paint time, as do the canvas background and the
/// default/dim/gutter chrome foreground (ADR-029, revised).
pub fn render(frame: &mut Frame, app: &mut App, keymap: &Keymap, theme: &Palette) {
    let area = frame.area();

    // CS10: reset every recorded hit region at the start of the frame — a region only survives
    // this frame if one of the panes below actually painted it again. Prevents a stale rect from
    // an earlier frame's layout (e.g. the outline just closed) from staying hit-testable.
    app.hit_regions = Default::default();

    // Paint the whole screen with the theme's background FIRST — a curated theme (light/dark)
    // controls the canvas outright; `auto` leaves `paint_canvas` false so the terminal's own
    // background (and any transparency) shows through instead. Everything drawn below only sets
    // `fg` (never `bg`) unless it's specifically painting a tint, so this base coat survives under
    // plain text and is overridden cleanly by the diff-tint/cursor/selection washes.
    if theme.paint_canvas {
        frame.render_widget(
            Block::default().style(Style::default().bg(theme.background)),
            area,
        );
    }

    // CS1 (`pane-headers`): no more standalone header row — the outline pane and the diff pane
    // each paint their own 1-row header at the top of their own rect (`render_outline`/
    // `render_body`), so `body_area` now claims the row the old global header/winbar used to
    // occupy. Every content row below keeps its exact prior y-coordinate: the row that moved out
    // of the top-level layout reappears as the per-pane header carve-out inside `body_area`.
    let vlayout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let body_area = vlayout[0];
    let footer_area = vlayout[1];

    render_footer(frame, app, footer_area, keymap, theme);

    if app.outline_open() {
        let hlayout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(app.outline_width()),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(body_area);
        let outline_area = hlayout[0];
        let div_area = hlayout[1];
        let diff_area = hlayout[2];
        render_outline(frame, app, outline_area, theme);
        // Spans the FULL body height, including row 0 — it now divides the two pane headers
        // (outline header vs. diff header) as well as the content rows below them; this reads
        // fine in practice (CS1 risk noted, revisit if it looks heavy at review).
        for y in div_area.y..div_area.y + div_area.height {
            frame
                .buffer_mut()
                .set_string(div_area.x, y, "│", Style::default().fg(theme.dim));
        }
        render_body(frame, app, diff_area, theme);
    } else {
        // Closed: the diff takes the full body width — the exact M4 look (locked design).
        render_body(frame, app, body_area, theme);
    }

    if app.help_visible {
        render_help_overlay(frame, app, keymap, area);
    }
}

/// Convert a ratatui [`Rect`] into the [`Region`] shape [`App::hit_regions`] stores (CS10) —
/// `app.rs` has no ratatui dependency, so every write into `hit_regions` goes through this.
fn region_from(area: Rect) -> Region {
    Region {
        x: area.x,
        y: area.y,
        w: area.width,
        h: area.height,
    }
}

/// Compute a centered `percent_x` × `percent_y` sub-rect of `area` — the standard ratatui popup
/// pattern (two nested percentage splits).
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// The `?` help overlay (CS3): a centered, bordered modal listing the focused view's + global
/// bindings, from the resolved `keymap` (never hardcoded — see [`crate::keymap::help_sections`]).
/// Focused view = outline when the outline pane has focus, else diff. [`Clear`] wipes the popup
/// area first so the diff content underneath doesn't show through the gaps between glyphs.
fn render_help_overlay(frame: &mut Frame, app: &App, keymap: &Keymap, area: Rect) {
    let focused = if app.outline_focused() {
        View::Outline
    } else {
        View::Diff
    };
    let sections = help_sections(keymap, focused);

    let mut lines: Vec<Line> = Vec::new();
    for section in &sections {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(TSpan::styled(
            section.title,
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for entry in &section.entries {
            lines.push(Line::from(format!(
                "  {:<10} {}",
                entry.keys, entry.description
            )));
        }
    }

    let popup_area = centered_rect(60, 60, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help (?/q/Esc to close) ");
    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

/// The style for a pane header/caption LABEL word (CS1, `focused-pane-header`), and — since
/// `header-chrome-follows-focus` — the structural "identity" chrome that travels with it: the
/// outline header's `[i/n]` counter, the diff header's `[fidx/nfiles]` counter, and the
/// changeset-prefix segment's `[i/n] {title}` text. The SEMANTIC spans (diffstats, the
/// needs-restack `⚠`, the current-changeset `●` marker, the pan-offset indicator) never use this
/// style — they keep their own colors regardless of focus (locked decision #2). `focused` selects
/// between [`Palette::pane_header_focused_fg`] with a structural, unconditional BOLD (locked
/// decision #3 — under [`Palette::mono`], where that color and `theme.dim` both collapse to
/// `Color::Reset`, this BOLD is the only thing that still marks the focused label) and the plain
/// [`Palette::dim`] every unfocused label already used before this changeset. Exactly one
/// header/caption across a frame's outline header / diff header / split captions should ever
/// receive `focused == true` (the exactly-one-lit-label invariant — see the module's
/// `focused-pane-header` handoff); since `header-chrome-follows-focus` that one header may style
/// several spans (counter + label + changeset-prefix text) through this function with the same
/// flag, so the invariant counts lit headers, not call sites.
fn pane_header_label_style(theme: &Palette, focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(theme.pane_header_focused_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.dim)
    }
}

/// The outline pane's own top row (CS1, `pane-headers`): `[i/n] {display_label}` (the active
/// changeset's TRUE stack position, the counter and display label both styled via
/// [`pane_header_label_style`] (the counter joined the toggle in `header-chrome-follows-focus`) —
/// lit ([`Palette::pane_header_focused_fg`] + bold) while the outline has focus, dim otherwise
/// (CS1, `focused-pane-header` — locked decision #5's "outline focused" case); no current-marker
/// glyph, since this header is always describing the currently-active changeset, a redundant
/// thing to mark), ` {warn_marker} needs restack` (`theme.warn_fg`, full text unlike the diff
/// header's glyph-only prefix — see [`changeset_prefix_spans`]) when
/// [`workon::Changeset::needs_restack`], and a changeset-total `+A -D` diffstat (the fold
/// `render_winbar` used to own, pre-CS1) skipped when [`App::files`] is empty (a Pending/Failed
/// changeset, ADR-031). Truncated to the outline's own width via [`Buffer::set_line`], exactly
/// like every outline item row below it.
///
/// CS1 risk (accepted, not fixed here): in [`crate::outline::OutlineMode::Flat`], the item rows
/// below dedupe a file across every changeset that touches it, with no changeset context of their
/// own — this header still names only the single ACTIVE changeset, so it can read as narrower
/// than what the (deduped, cross-stack) row list actually shows. Acceptable for now; a future
/// changeset could soften this (e.g. suppress the header in Flat mode) if it proves confusing in
/// practice.
fn render_outline_header(frame: &mut Frame, app: &App, area: Rect, theme: &Palette, focused: bool) {
    let cs = app.current_changeset();
    let i = app.current_cs() + 1;
    let n = app.changeset_count();
    let title = crate::app::display_label(cs);
    let icons = app.icon_mode();

    let mut spans = vec![
        TSpan::styled(
            format!("[{i}/{n}] "),
            pane_header_label_style(theme, focused),
        ),
        TSpan::styled(title, pane_header_label_style(theme, focused)),
    ];
    if cs.needs_restack {
        spans.push(TSpan::styled(
            format!("  {} needs restack", warn_marker(icons)),
            Style::default()
                .fg(theme.warn_fg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    // A pending/failed changeset's `files()` is always empty (ADR-031) — skip the diffstat
    // segment entirely rather than show a misleading "+0 -0" (same gate `render_winbar` used).
    if !app.files().is_empty() {
        let (adds, dels) = app
            .files()
            .iter()
            .map(crate::summary::file_diffstat)
            .fold((0, 0), |(a, d), (fa, fd)| (a + fa, d + fd));
        spans.extend(diffstat_spans(adds, dels, theme, icons));
    }
    let line = Line::from(spans);
    frame
        .buffer_mut()
        .set_line(area.x, area.y, &line, area.width);
}

/// Render the outline pane into `area`: row 0 is the pane's own header (CS1, `pane-headers` — see
/// [`render_outline_header`]), skipped only when `area.height < 2` (a degenerate terminal has no
/// room to spare); every row below is an outline item exactly as before this changeset — the
/// header carve-out is why an item's absolute screen row hasn't moved (it used to start one row
/// below the OLD global header, now it starts one row below the pane's OWN header instead).
/// [`OutlineItem::Header`]s (Stack mode only) carry the changeset's position marker (green • for
/// `cs.current`), a `[i/n]` TRUE-stack-position counter, an accented ([`Palette::heading_fg`])
/// bold label (CS1, `outline-header-polish` — see [`changeset_title_spans`]'s doc comment), and
/// needs-restack glyph (amber ⚠, [`crate::theme::Palette::warn_fg`] — locked decision #9's outline
/// half); [`OutlineItem::File`]s carry an
/// indent, a two-column git-porcelain-style status matrix (CS3, `outline-status-xy` — see
/// [`outline_status_spans`]'s doc comment for the X/Y-vs-single-letter split), and
/// the path — Flat/Stack rows (CS2) split it into `basename  dim/dirname` (no suffix for a
/// root-level file); Tree/StackTree rows already carry the directory via ancestor Dir rows, so
/// `path` there is just the bare basename. A COLLAPSED [`OutlineItem::Header`]/[`OutlineItem::Dir`]
/// row (CS5, `outline-fold`) additionally carries a trailing dim ` ▸ N` (`N` = hidden FILE rows
/// only), from [`App::outline_items_with_hidden_counts`]'s per-row marker count — an expanded row
/// gets no chevron at all. The cursor row (the outline's OWN cursor — a separate coordinate space from the
/// diff's [`App::cursor`]) gets the theme's cursor tint while the outline has focus, or the dimmer
/// [`Palette::cursor_unfocused_bg`] while it's merely open (so the remembered position stays
/// legible even after focus returns to the diff). `&mut App` (CS2, precedent: [`render_body`]
/// writing [`App::pane_height`]) — writes [`App::outline_height`] and re-derives
/// [`App::derive_outline_scroll`] before painting from `app.outline.scroll`, giving the outline
/// the same stateful scrolloff-margined viewport the diff panes already have, instead of the old
/// transient bottom-anchor scroll computed fresh each frame.
fn render_outline(frame: &mut Frame, app: &mut App, area: Rect, theme: &Palette) {
    // CS1 risk: this `>= 2` guard must exist in BOTH pane renderers (see `render_body`'s matching
    // carve-out) — a 1-row (or shorter) terminal has no room to spare for a header at all.
    let area = if area.height >= 2 {
        render_outline_header(frame, app, area, theme, app.outline_focused());
        Rect::new(area.x, area.y + 1, area.width, area.height - 1)
    } else {
        area
    };
    app.outline_height = area.height as usize;
    app.hit_regions.outline = Some(region_from(area));
    let (items, hidden_counts) = app.outline_items_with_hidden_counts();
    // Bounds-clamp only — NOT a cursor-following derive: under the wheel's peek model a
    // scrolled-away viewport must survive the frame; cursor ops re-derive on their own.
    app.clamp_outline_scroll(items.len());

    let cursor = app.outline_cursor();
    let focused = app.outline_focused();
    let scroll = app.outline_scroll();
    let icons = app.icon_mode();

    // Render-side upper clamp of the outline's own pan offset (mirroring `clamp_outline_scroll`
    // just above) — from EVERY item's built line width, not just the visible rows: outlines are
    // small (file trees, not file contents), so re-measuring the whole thing here is cheap.
    let max_line_width = items
        .iter()
        .zip(&hidden_counts)
        .map(|(item, &hidden)| build_outline_line(item, theme, icons, hidden).width())
        .max()
        .unwrap_or(0);
    app.clamp_outline_hscroll(max_line_width);
    let hscroll = app.outline_hscroll();

    let buf = frame.buffer_mut();
    for row in 0..area.height {
        let item_idx = scroll + row as usize;
        let y = area.y + row;
        let Some(item) = items.get(item_idx) else {
            continue;
        };
        let hidden = hidden_counts.get(item_idx).copied().unwrap_or(0);
        let is_cursor = item_idx == cursor;
        let line = build_outline_line(item, theme, icons, hidden);
        let line = Line::from(pan_spans(line.spans, hscroll, theme));
        let line = if is_cursor {
            apply_cursor_row(line, area.width, theme, focused)
        } else {
            line
        };
        buf.set_line(area.x, y, &line, area.width);
        apply_right_edge_marker(buf, area, y, &line, theme);
    }
}

/// Render a tree-guide prefix from an [`OutlineItem::Dir`]/[`OutlineItem::File`] `guides`
/// vector: every element but the last draws a continuing `│` (if that ancestor level was NOT
/// its parent's last child) or blank space (if it was), and the last element draws the row's own
/// `╰─`/`├─` connector — CS4 rounds the last-child corner (`╰`, U+2570) from the square `└`
/// (U+2514); there's no widely-supported rounded "tee" glyph, so the non-last `├─` connector is
/// unchanged. CS2 tightens indent to 2 cols/level: continuation is `│ ` (bar + space, no third
/// column), and connectors (`├─`/`╰─`) carry no trailing space — the glyph that follows hugs the
/// connector directly.
fn tree_prefix(guides: &[bool]) -> String {
    let mut s = String::new();
    let Some((&is_last, ancestors)) = guides.split_last() else {
        return s;
    };
    for &last in ancestors {
        s.push_str(if last { "  " } else { "\u{2502} " });
    }
    s.push_str(if is_last {
        "\u{2570}\u{2500}"
    } else {
        "\u{251C}\u{2500}"
    });
    s
}

/// Placeholder glyph for an empty XY status column (CS3, `outline-status-xy`) — U+00B7 middle
/// dot, always `theme.dim`, standing in for "nothing to report on this axis." Deliberately not a
/// space: the two-column matrix should read as a grid even when one side is empty, not look like
/// a ragged single-letter row.
const STATUS_PLACEHOLDER: char = '\u{b7}';

/// A committed changeset's single-letter status color (CS3): A green (`add_strong`), D red
/// (`del_strong`), M/R/C (a change to EXISTING content, not a create/destroy) the dedicated amber
/// [`Palette::modified_fg`], and `?`/`U` dim (Untracked never reaches here — see
/// [`outline_status_spans`]'s doc comment — and Unmerged is a worktree-only conflict state a
/// committed changeset can't carry; both fold to `dim` only so this match stays exhaustive).
fn committed_letter_color(change: FileStatus, theme: &Palette) -> Color {
    match change {
        FileStatus::Added => theme.add_strong,
        FileStatus::Deleted => theme.del_strong,
        FileStatus::Modified | FileStatus::Renamed | FileStatus::Copied => theme.modified_fg,
        FileStatus::Untracked | FileStatus::Unmerged => theme.dim,
    }
}

/// Build a file row's two-column status matrix (CS3, `outline-status-xy`) — always exactly 2
/// [`TSpan`]s' worth of display columns, in every mode, so committed and uncommitted rows stay
/// aligned (the changeset's Gotcha).
///
/// - `change == FileStatus::Untracked` wins over everything else and renders a dim `??` — noise,
///   not danger, regardless of `status` (see [`crate::outline::StagedStatus`]'s doc comment: an
///   untracked worktree file is always `Unstaged`, but git's own convention for untracked is `??`,
///   not a staged-ness-derived letter).
/// - `StagedStatus::None` is the committed-changeset case (see that type's doc comment for why no
///   special-casing is needed to detect it): a single [`FileStatus::letter`] colored by
///   [`committed_letter_color`], plus a blank pad column.
/// - `Unstaged`/`Staged`/`Partial` render the git-porcelain X/Y matrix: `letter` (from the SAME
///   underlying [`FileStatus`] — there's only one change kind per file, not separate staged/
///   unstaged kinds) in whichever column(s) that axis has a change, [`STATUS_PLACEHOLDER`] in the
///   other; X (staged/index) is `add_strong` green, Y (worktree) is `del_strong` red, matching
///   git's own status convention.
fn outline_status_spans(
    status: crate::outline::StagedStatus,
    change: FileStatus,
    theme: &Palette,
) -> Vec<TSpan<'static>> {
    use crate::outline::StagedStatus;

    if change == FileStatus::Untracked {
        return vec![TSpan::styled(
            "??".to_string(),
            Style::default().fg(theme.dim),
        )];
    }
    match status {
        StagedStatus::None => {
            let letter = change.letter();
            vec![
                TSpan::styled(
                    letter.to_string(),
                    Style::default().fg(committed_letter_color(change, theme)),
                ),
                TSpan::styled(" ".to_string(), Style::default().fg(theme.foreground)),
            ]
        }
        StagedStatus::Unstaged | StagedStatus::Staged | StagedStatus::Partial => {
            let letter = change.letter();
            let staged = matches!(status, StagedStatus::Staged | StagedStatus::Partial);
            let unstaged = matches!(status, StagedStatus::Unstaged | StagedStatus::Partial);
            let x_char = if staged { letter } else { STATUS_PLACEHOLDER };
            let y_char = if unstaged { letter } else { STATUS_PLACEHOLDER };
            let x_color = if staged { theme.add_strong } else { theme.dim };
            let y_color = if unstaged {
                theme.del_strong
            } else {
                theme.dim
            };
            vec![
                TSpan::styled(x_char.to_string(), Style::default().fg(x_color)),
                TSpan::styled(y_char.to_string(), Style::default().fg(y_color)),
            ]
        }
    }
}

/// CS5 (`outline-fold`): a collapsed Header/Dir row's trailing marker — dim ` ▸ N`, `N` being the
/// count of hidden FILE rows (not dirs) [`App::outline_items_with_hidden_counts`] attached to that
/// row. `None` for `hidden == 0` (an EXPANDED Header/Dir — or a File row, which never carries a
/// hidden count at all) — the locked "no chevron when expanded" rule reads a zero count as "don't
/// draw a marker" rather than "draw ` ▸ 0`".
fn fold_marker(hidden: usize, theme: &Palette) -> Option<TSpan<'static>> {
    (hidden > 0).then(|| {
        TSpan::styled(
            format!(" \u{25b8} {hidden}"),
            Style::default().fg(theme.dim),
        )
    })
}

/// Build one outline row's rendered [`Line`] — see [`render_outline`]'s doc comment for the
/// marker rules. `icons` (CS5, `workon.review.icons`) is [`IconMode::None`] by
/// default, which reproduces the pre-CS5 row text exactly (no icon glyph, no extra space); only
/// [`IconMode::Nerd`] inserts an icon before the name/path. `hidden` (CS5, `outline-fold`) is the
/// row's collapsed hidden-file count from [`App::outline_items_with_hidden_counts`] — `0` for
/// every row that isn't a collapsed Header/Dir; see [`fold_marker`].
fn build_outline_line(
    item: &OutlineItem,
    theme: &Palette,
    icons: IconMode,
    hidden: usize,
) -> Line<'static> {
    match item {
        OutlineItem::Header {
            cs_idx,
            n,
            label,
            current,
            needs_restack,
            loading,
            failed,
        } => {
            let mut spans = changeset_title_spans(
                label,
                *current,
                *needs_restack,
                theme,
                icons,
                Some((cs_idx + 1, *n)),
            );
            // ADR-031: a Failed changeset's marker wins over Pending's (a slot is never both,
            // but Failed is the more actionable state to surface if it somehow were).
            if *failed {
                spans.push(TSpan::styled(
                    format!(" {}", error_marker(icons)),
                    Style::default().fg(theme.error_fg),
                ));
            } else if *loading {
                spans.push(TSpan::styled(
                    format!(" {}", loading_marker(icons)),
                    Style::default().fg(theme.dim),
                ));
            }
            spans.extend(fold_marker(hidden, theme));
            Line::from(spans)
        }
        OutlineItem::Dir { name, guides, .. } => {
            let icon = match icons {
                IconMode::Nerd => format!("{} ", crate::icons::DIR_ICON),
                IconMode::None => String::new(),
            };
            let text = format!("{}{icon}{name}/", tree_prefix(guides));
            let mut spans = vec![TSpan::styled(
                text,
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
            )];
            spans.extend(fold_marker(hidden, theme));
            Line::from(spans)
        }
        OutlineItem::File {
            path,
            status,
            change,
            guides,
            ..
        } => {
            // Empty `guides` (Flat/Stack modes) keeps the original two-space indent; a
            // non-empty `guides` (Tree/StackTree modes) draws tree connectors instead — see
            // `OutlineItem`'s doc comment for why emptiness is the mode signal. CS4: a non-empty
            // prefix (real tree connectors) gets its own `theme.dim`-styled span — matching the
            // Dir row's already-dim guides — so the guide lines read as quiet structure, not part
            // of the file's own status column. The status matrix itself (CS3,
            // `outline_status_spans`) is always exactly 2 display columns, same width the old
            // glyph+letter pair occupied, so this swap doesn't shift anything after it.
            let mut spans = Vec::new();
            if guides.is_empty() {
                spans.push(TSpan::styled(
                    "  ".to_string(),
                    Style::default().fg(theme.foreground),
                ));
            } else {
                spans.push(TSpan::styled(
                    tree_prefix(guides),
                    Style::default().fg(theme.dim),
                ));
            }
            spans.extend(outline_status_spans(*status, *change, theme));
            spans.push(TSpan::styled(
                " ".to_string(),
                Style::default().fg(theme.foreground),
            ));
            if icons == IconMode::Nerd {
                let (icon, color) = crate::icons::icon_for_path(
                    path,
                    crate::theme::is_light_background(theme.background),
                );
                // Nerd-font icon colors are palette-external (hardcoded per-filetype `Rgb` from
                // `icons::icon_for_path`, not a `Palette` field), so a colorless (NO_COLOR) theme
                // must collapse them to `foreground` itself — see `Palette::colorless`'s doc
                // comment.
                let icon_fg = if theme.colorless {
                    theme.foreground
                } else {
                    color.unwrap_or(theme.foreground)
                };
                spans.push(TSpan::styled(
                    format!("{icon} "),
                    Style::default().fg(icon_fg),
                ));
            }
            // Flat/Stack rows (empty `guides`) split `path` at render time into `basename  dim/
            // dirname` — basename first (bright, matching the tree modes' bare-name leaves) so
            // truncation eats the dim dirname before the name a user is scanning for (CS2
            // gotcha). Tree/StackTree rows (non-empty `guides`) already carry the path via
            // ancestor Dir rows, so `path` there is already just the basename — render it as-is.
            if guides.is_empty() {
                match path.rsplit_once('/') {
                    Some((dir, base)) => {
                        spans.push(TSpan::styled(
                            base.to_string(),
                            Style::default().fg(theme.foreground),
                        ));
                        spans.push(TSpan::styled(
                            format!("  {dir}"),
                            Style::default().fg(theme.dim),
                        ));
                    }
                    None => spans.push(TSpan::styled(
                        path.clone(),
                        Style::default().fg(theme.foreground),
                    )),
                }
            } else {
                spans.push(TSpan::styled(
                    path.clone(),
                    Style::default().fg(theme.foreground),
                ));
            }
            Line::from(spans)
        }
    }
}

/// The current file's label for the diff pane header: its path, or a rename's `old @ base ->
/// path` form — shared by every diff-header state ([`file_segment_spans`]) and the summary
/// panel's own current-changeset-independent uses.
fn current_file_label(app: &App) -> String {
    match app.files().get(app.current) {
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
    }
}

/// While [`App::hscroll`] is panned, a small dim `»42` (the column offset) appended to the diff
/// pane header (locked decision #8) — `None` at column `0`, matching the diffstat span's own
/// present-or-absent pattern above/below.
fn hscroll_indicator_span(app: &App, theme: &Palette) -> Option<TSpan<'static>> {
    if app.hscroll == 0 {
        return None;
    }
    Some(TSpan::styled(
        format!("  »{}", app.hscroll),
        Style::default().fg(theme.dim),
    ))
}

/// CS1 (`pane-headers`)'s changeset-position prefix, prepended to the diff pane header only when
/// the outline is CLOSED and the stack has more than one changeset (see [`diff_header_line`]) —
/// with the outline open, the outline pane's own header ([`render_outline_header`]) already
/// carries this information, so showing it twice would be redundant. `[i/n] {display_label}`,
/// plus a glyph-ONLY (no "needs restack" text — that's the outline header's fuller treatment) `⚠`
/// in `theme.warn_fg` when [`workon::Changeset::needs_restack`]. Ported verbatim from the old
/// `render_winbar`'s equivalent prefix (locked decisions #8 + #9), minus the diffstat/path/icon
/// tail that moved into [`file_segment_spans`]. `focused` (CS1, `header-chrome-follows-focus`)
/// is the same flag [`diff_header_line`]'s own label receives — the `[i/n] {title}` text lights
/// and dims with it via [`pane_header_label_style`], while the warn glyph keeps its semantic
/// `theme.warn_fg` regardless (locked decision #2).
fn changeset_prefix_spans(
    app: &App,
    theme: &Palette,
    icons: IconMode,
    focused: bool,
) -> Vec<TSpan<'static>> {
    let cs = app.current_changeset();
    let i = app.current_cs() + 1;
    let n = app.changeset_count();
    let title = crate::app::display_label(cs);

    let mut spans = vec![TSpan::styled(
        format!("[{i}/{n}] {title}"),
        pane_header_label_style(theme, focused),
    )];
    // A boolean-driven glyph + color (locked decision #9), not a title-string suffix — distinct
    // from the plain title so a stale-parent changeset reads as a heads-up at a glance.
    if cs.needs_restack {
        spans.push(TSpan::styled(
            format!(" {}", warn_marker(icons)),
            Style::default()
                .fg(theme.warn_fg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

/// The diff pane header's shared "current file" segment (CS1, `pane-headers`): `[fidx/nfiles] `
/// and [`current_file_label`] both styled via [`pane_header_label_style`] (lit while `focused`,
/// dim otherwise — CS1, `focused-pane-header`; the counter joined the label's lit/dim toggle in
/// `header-chrome-follows-focus`, having previously stayed unconditionally bold), an optional
/// nerd devicons file icon, a tight `+N -M` per-file diffstat (new: the old winbar only ever
/// showed a CHANGESET-total
/// diffstat, never a per-file one — [`crate::summary::file_diffstat`] gives the same recorded
/// counts for a binary file as a text one, so this segment needs no binary special-case), and the
/// pan-offset indicator. Used verbatim whether the outline is open, closed+lone, or closed+multi
/// (with the changeset prefix ahead of it) — see [`diff_header_line`]'s state table. `focused` is
/// resolved by the caller from [`EffectiveZoom`] + focus state, not computed here (locked
/// decision #5: this segment is the diff pane header's own label, lit only when the diff has
/// focus AND the effective zoom is [`EffectiveZoom::Single`] — under [`EffectiveZoom::Split`] a
/// caption is the lit label instead, so this stays dim, EXCEPT when `render_body_split`'s own
/// short-area fallback drops both captions, in which case this label lights up instead).
fn file_segment_spans(
    app: &App,
    theme: &Palette,
    icons: IconMode,
    focused: bool,
) -> Vec<TSpan<'static>> {
    let idx = app.current + 1;
    let n = app.files().len();
    let mut spans = vec![TSpan::styled(
        format!("[{idx}/{n}] "),
        pane_header_label_style(theme, focused),
    )];
    if icons == IconMode::Nerd {
        if let Some(f) = app.files().get(app.current) {
            let (icon, color) = crate::icons::icon_for_path(
                &f.path,
                crate::theme::is_light_background(theme.background),
            );
            // Same palette-external collapse as the outline's icon paint site above — see
            // `Palette::colorless`'s doc comment.
            let icon_fg = if theme.colorless {
                theme.foreground
            } else {
                color.unwrap_or(theme.foreground)
            };
            spans.push(TSpan::styled(
                format!("{icon} "),
                Style::default().fg(icon_fg).add_modifier(Modifier::BOLD),
            ));
        }
    }
    spans.push(TSpan::styled(
        current_file_label(app),
        pane_header_label_style(theme, focused),
    ));
    if let Some(f) = app.files().get(app.current) {
        let (adds, dels) = crate::summary::file_diffstat(f);
        spans.extend(diffstat_spans(adds, dels, theme, icons));
    }
    if let Some(span) = hscroll_indicator_span(app, theme) {
        spans.push(span);
    }
    spans
}

/// The diff pane's own top-row header (CS1, `pane-headers` — replacing the old global
/// header/winbar row; see `render_body`'s header carve-out). Only covers the NON-summary states —
/// [`render_body`] handles the summary-panel title separately, since that title comes from
/// [`App::summary_for`] (called once per frame, not re-derived here). State table:
///
/// - Outline open: [`file_segment_spans`] alone (the outline pane's own header already carries
///   the changeset-position context, so this stays file-focused).
/// - Outline closed + `changeset_count() > 1`: [`changeset_prefix_spans`], then a bold `  —  `
///   separator, then [`file_segment_spans`] — the closed outline hides `]c`/`[c`'s (Diff-view
///   bindings, `keymap.rs`) changeset-nav feedback, so this prefix keeps it visible.
/// - Outline closed + lone changeset: [`file_segment_spans`] alone (the pre-CS1 M4 look, now with
///   a per-file diffstat it never had before).
/// - Pending/failed/empty `files()` (ADR-031): the changeset prefix alone if
///   `changeset_count() > 1 && !outline_open()`, else a blank row — never a misleading `[1/0]`.
///   "Blank" still carries an explicit `theme.foreground`-styled space (not a zero-span [`Line`])
///   — an empty span list leaves the row's cells at whatever style predates this frame's paint
///   (`Style::default()`'s `Reset` fg, even under a painted canvas, since [`Buffer::set_line`]
///   writes nothing for zero-width content) rather than the theme's own baseline (regression:
///   `header_text_carries_the_theme_foreground_not_the_terminal_default`).
///
/// `focused` is `true` only when the diff pane's OWN header label should be lit — the caller
/// ([`render_body`]) resolves this from the diff's focus state AND [`EffectiveZoom`] (locked
/// decision #5): a Split zoom lights a caption instead (see [`render_body_split`]), so this stays
/// dim even while the diff has focus in that case.
fn diff_header_line(app: &App, theme: &Palette, icons: IconMode, focused: bool) -> Line<'static> {
    let show_prefix = app.changeset_count() > 1 && !app.outline_open();

    if app.current_failure().is_some() || app.is_current_pending() || app.files().is_empty() {
        return if show_prefix {
            Line::from(changeset_prefix_spans(app, theme, icons, focused))
        } else {
            Line::from(TSpan::styled(
                " ".to_string(),
                Style::default().fg(theme.foreground),
            ))
        };
    }

    let mut spans = Vec::new();
    if show_prefix {
        spans.extend(changeset_prefix_spans(app, theme, icons, focused));
        spans.push(TSpan::styled(
            "  —  ".to_string(),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.extend(file_segment_spans(app, theme, icons, focused));
    Line::from(spans)
}

/// Footer priority: a pending discard confirm's prompt (warn-toned) wins over a transient notice,
/// which wins over the curated hint line (CS3) — a notice TEMPORARILY REPLACES the hint rather
/// than adding a second row; it clears on the user's next keypress (`tui::update`).
fn render_footer(frame: &mut Frame, app: &App, area: Rect, keymap: &Keymap, theme: &Palette) {
    if let Some(confirm) = &app.pending_confirm {
        frame.render_widget(
            Paragraph::new(confirm.prompt.as_str()).style(Style::default().fg(theme.error_fg)),
            area,
        );
        return;
    }
    match &app.notice {
        Some(Notice { text, severity }) => {
            let fg = match severity {
                Severity::Error => theme.error_fg,
                Severity::Info => theme.foreground,
            };
            frame.render_widget(
                Paragraph::new(text.as_str()).style(Style::default().fg(fg)),
                area,
            );
        }
        None => {
            // While the outline has focus, only outline-relevant keys act (locked design) — the
            // diff-editing hint would be actively misleading, so show the outline's own curated
            // hint instead. Built from the resolved `keymap`, never a hardcoded key string, so a
            // rebind shows here too (see [`crate::keymap::footer_hint`]).
            let focused = if app.outline_focused() {
                View::Outline
            } else {
                View::Diff
            };
            let text = footer_hint(keymap, focused, app.outline_mode());
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(theme.dim)),
                area,
            );
        }
    }
}

/// Write a gap row's `··· N unchanged lines (Enter to expand) ···` marker across the FULL body
/// width (both panes and the divider column) — unlike a per-pane content row, a gap hides the
/// same span on both sides, so it isn't "about" one side or the other.
#[allow(clippy::too_many_arguments)]
fn render_gap_row(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    skipped: usize,
    is_cursor: bool,
    is_selected: bool,
    theme: &Palette,
    focused: bool,
) {
    let msg = format!("··· {skipped} unchanged lines (Enter to expand) ···");
    let line = Line::from(TSpan::styled(msg, Style::default().fg(theme.dim)));
    // Cursor wins over selection on the same row.
    let line = if is_cursor {
        apply_cursor_row(line, area.width, theme, focused)
    } else if is_selected {
        apply_selection_row(line, area.width, theme)
    } else {
        line
    };
    buf.set_line(area.x, y, &line, area.width);
}

/// Whether file `idx` needs CS4's deferred-load placeholder instead of its real diff: either the
/// current open is still pending (set by [`App::open_current`] in defer mode — see its doc
/// comment), or it isn't pending but the view(s) its effective zoom needs haven't been loaded yet
/// (e.g. a force-completed OTHER file's load left this one's cache untouched). Under
/// [`EffectiveZoom::Split`] the placeholder shows only when NEITHER pane is loaded: a role with
/// no change for the file stays legitimately `None` forever (see `ensure_role_loaded`), so
/// gating on both panes would placeholder a one-role file for good. Once
/// [`App::complete_pending_open`] runs, every loadable pane is loaded, and a role-less pane
/// renders empty exactly as it did pre-CS4.
fn needs_deferred_placeholder(app: &App, idx: usize) -> bool {
    if app.open_pending() {
        return true;
    }
    match app.effective_zoom_for(idx) {
        EffectiveZoom::Single(role) => app.role_view_ref(idx, role).is_none(),
        EffectiveZoom::Split => {
            app.role_view_ref(idx, Role::Unstaged).is_none()
                && app.role_view_ref(idx, Role::Staged).is_none()
        }
    }
}

/// Render CS4's deferred-load placeholder: a dim one-line paragraph naming the file, matching the
/// existing binary-file placeholder's style (see `render_body`'s binary arm) so the two read as
/// the same kind of "nothing to show yet" message.
fn render_loading_placeholder(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    idx: usize,
    theme: &Palette,
) {
    let msg = format!("{} — loading…", app.files()[idx].path);
    frame.render_widget(
        Paragraph::new(msg).style(Style::default().fg(theme.dim)),
        area,
    );
}

/// Push a `"path  +N -M"` file row's spans onto `lines`: the path in the theme foreground, the
/// add/del counts tinted with the theme's own diff-add/diff-del colors (the strong variants — the
/// same tint a hunk's `+`/`-` gutter itself uses, see [`Palette::add_strong`]/
/// [`Palette::del_strong`]) so the panel's diffstat reads consistently with the diff body it's
/// standing in for.
fn push_summary_file_row(lines: &mut Vec<Line<'static>>, row: &SummaryFileRow, theme: &Palette) {
    lines.push(Line::from(vec![
        TSpan::styled(row.path.clone(), Style::default().fg(theme.foreground)),
        TSpan::raw("  "),
        TSpan::styled(
            format!("+{}", row.adds),
            Style::default().fg(theme.add_strong),
        ),
        TSpan::raw(" "),
        TSpan::styled(
            format!("-{}", row.dels),
            Style::default().fg(theme.del_strong),
        ),
    ]));
}

/// Append `rows`' file lines to `lines`, truncated to leave room for `budget` more rows within the
/// panel's height — the last line becomes `"… and N more"` (dim) when the list overflows instead
/// of silently clipping.
fn push_summary_file_rows(
    lines: &mut Vec<Line<'static>>,
    rows: &[SummaryFileRow],
    budget: usize,
    theme: &Palette,
) {
    if rows.len() <= budget {
        for row in rows {
            push_summary_file_row(lines, row, theme);
        }
        return;
    }
    // Reserve the last visible row for the "… and N more" marker.
    let shown = budget.saturating_sub(1);
    for row in &rows[..shown] {
        push_summary_file_row(lines, row, theme);
    }
    let remaining = rows.len() - shown;
    lines.push(Line::from(TSpan::styled(
        format!("\u{2026} and {remaining} more"),
        Style::default().fg(theme.dim),
    )));
}

/// Append the shared summary body — spacer, height-budgeted per-file rows, and the
/// `"{N} files  +A -D"` totals line — used verbatim by both [`changeset_summary_lines`] and
/// [`dir_summary_lines`], which differ only in their title line and early-return states.
fn push_summary_body(
    lines: &mut Vec<Line<'static>>,
    files: &[SummaryFileRow],
    total_adds: usize,
    total_dels: usize,
    height: usize,
    theme: &Palette,
    icons: IconMode,
) {
    lines.push(Line::from(""));
    let footer_budget = 1; // the totals line always shows
    let file_budget = height.saturating_sub(lines.len() + footer_budget);
    push_summary_file_rows(lines, files, file_budget, theme);
    let (added_prefix, removed_prefix) = diffstat_prefixes(icons);
    lines.push(Line::from(vec![
        TSpan::styled(
            format!("{} files", files.len()),
            Style::default().fg(theme.foreground),
        ),
        TSpan::raw("  "),
        TSpan::styled(
            format!("{added_prefix}{total_adds}"),
            Style::default().fg(theme.add_strong),
        ),
        TSpan::raw(" "),
        TSpan::styled(
            format!("{removed_prefix}{total_dels}"),
            Style::default().fg(theme.del_strong),
        ),
    ]));
}

/// Build a [`ChangesetSummary`]'s title spans (the same current/needs-restack markers
/// `build_outline_line`'s Header arm draws, structurally shared via [`changeset_title_spans`] —
/// but passing `None` for that fn's `counter` param, so this title keeps its pre-CS1 plain-
/// foreground look with no `[i/n]` counter; see [`changeset_title_spans`]'s doc comment) and its
/// body lines: a loading/failed line OR the per-file list + totals line. CS1 (`pane-headers`)
/// split the return into `(title, body)` — the title now paints the diff pane's header row
/// ([`render_body`]), and the body no longer duplicates it as its own first line.
fn changeset_summary_lines(
    summary: &ChangesetSummary,
    height: usize,
    theme: &Palette,
    icons: IconMode,
) -> (Vec<TSpan<'static>>, Vec<Line<'static>>) {
    let title = changeset_title_spans(
        &summary.label,
        summary.current,
        summary.needs_restack,
        theme,
        icons,
        None,
    );

    let mut lines = Vec::new();
    if summary.failed {
        let msg = summary
            .failure_message
            .as_deref()
            .unwrap_or("(no error message)");
        lines.push(Line::from(TSpan::styled(
            format!("{} {msg}", error_marker(icons)),
            Style::default().fg(theme.error_fg),
        )));
        return (title, lines);
    }
    if summary.loading {
        lines.push(Line::from(TSpan::styled(
            format!("Loading{}", loading_marker(icons)),
            Style::default().fg(theme.dim),
        )));
        return (title, lines);
    }

    push_summary_body(
        &mut lines,
        &summary.files,
        summary.total_adds,
        summary.total_dels,
        height,
        theme,
        icons,
    );
    (title, lines)
}

/// Build a [`DirSummary`]'s title spans (a bold path line — no current/restack/loading/failed
/// markers, a directory carries none of those; the title gets [`crate::icons::DIR_ICON`] in
/// [`IconMode::Nerd`] mode, matching the outline's own [`OutlineItem::Dir`] row
/// (`build_outline_line`)) and its body lines (the per-file list + totals line). CS1
/// (`pane-headers`): see [`changeset_summary_lines`]'s doc comment for why this returns a
/// `(title, body)` tuple now instead of one combined line list.
fn dir_summary_lines(
    summary: &DirSummary,
    height: usize,
    theme: &Palette,
    icons: IconMode,
) -> (Vec<TSpan<'static>>, Vec<Line<'static>>) {
    let dir_icon = match icons {
        IconMode::Nerd => format!("{} ", crate::icons::DIR_ICON),
        IconMode::None => String::new(),
    };
    let title = vec![TSpan::styled(
        format!("{dir_icon}{}/", summary.path),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    )];
    let mut lines = Vec::new();
    push_summary_body(
        &mut lines,
        &summary.files,
        summary.total_adds,
        summary.total_dels,
        height,
        theme,
        icons,
    );
    (title, lines)
}

/// CS4's summary panel: renders in place of the diff body while the outline is open and focused
/// with its cursor on a Header/Dir row (see [`App::summary_target`]) — per-file `"path  +N -M"`
/// rows (truncated to the pane height) and a totals line, painted into `area` (the diff pane's
/// header row is carved out by the caller, [`render_body`], before this ever runs — CS1,
/// `pane-headers`). Returns the title [`Line`] so the caller can paint it into that header row;
/// this fn itself paints only the body. A loading/failed Header shows its own inline state
/// instead of a file list (see [`changeset_summary_lines`]).
fn render_summary(
    frame: &mut Frame,
    summary: &Summary,
    area: Rect,
    theme: &Palette,
    icons: IconMode,
) -> Line<'static> {
    let height = area.height as usize;
    let (title, lines) = match summary {
        Summary::Changeset(cs) => changeset_summary_lines(cs, height, theme, icons),
        Summary::Dir(dir) => dir_summary_lines(dir, height, theme, icons),
    };
    frame.render_widget(Paragraph::new(lines), area);
    Line::from(title)
}

fn render_body(frame: &mut Frame, app: &mut App, area: Rect, theme: &Palette) {
    let icons = app.icon_mode();

    // CS1 (`pane-headers`): row 0 of the diff pane's own rect is its header — every branch below
    // (summary panel, pending/failed/empty, binary, normal file) shares this same carve-out, so
    // it happens once, up front. CS1 risk: this `>= 2` guard must exist in BOTH pane renderers
    // (see `render_outline`'s matching carve-out) — a 1-row (or shorter) terminal has no room to
    // spare for a header at all, so `header_area` is `None` and `area` (shadowed below) stays the
    // full rect. Every content row keeps its exact prior y-coordinate: the row that moved out of
    // `render`'s top-level layout reappears as this per-pane carve-out.
    let (header_area, area) = if area.height >= 2 {
        (
            Some(Rect::new(area.x, area.y, area.width, 1)),
            Rect::new(area.x, area.y + 1, area.width, area.height - 1),
        )
    } else {
        (None, area)
    };

    // CS4: the outline is open AND focused, and its cursor rests on a Header/Dir row — show that
    // row's summary instead of a file's diff. Checked before every other body gate below (an
    // unfocused open outline, or the cursor on a File row, falls straight through to the usual
    // diff-body rendering; `summary_target` returns `None` in both cases).
    if let Some(target) = app.summary_target() {
        // Built exactly once per frame (CS1 risk: never call `summary_for` twice) — its title
        // spans paint the header row below, its body-only lines paint `render_summary`'s content.
        let summary = app.summary_for(target);
        let title = render_summary(frame, &summary, area, theme, icons);
        if let Some(header_area) = header_area {
            frame
                .buffer_mut()
                .set_line(header_area.x, header_area.y, &title, header_area.width);
        }
        return;
    }

    if let Some(header_area) = header_area {
        // CS1 (`focused-pane-header`, locked decision #5): the diff header label lights up only
        // when the diff has focus AND its effective (not requested) zoom is `Single` — a `Split`
        // zoom lights the focused half's caption instead (see `render_body_split`), and the
        // outline holding focus dims every diff-side label. `effective_zoom_for` is cheap and
        // already re-derived every frame elsewhere in this fn (locked decision #3), so no caching
        // concern here either.
        //
        // Exception: `render_body_split`'s own short-area fallback (`area.height < 4`) renders
        // only the focused pane and returns before either caption is drawn — no split caption
        // survives to be the frame's lit label. `area` here is the exact same rect that fallback
        // gates on (both derive from the header carve-out above), so this branch mirrors that
        // check and lights the diff header instead, preserving the exactly-one-lit-label
        // invariant.
        let diff_header_focused = !app.outline_focused()
            && match app.effective_zoom_for(app.current) {
                EffectiveZoom::Single(_) => true,
                EffectiveZoom::Split => area.height < 4,
            };
        let line = diff_header_line(app, theme, icons, diff_header_focused);
        frame
            .buffer_mut()
            .set_line(header_area.x, header_area.y, &line, header_area.width);
    }
    // ADR-031: the active changeset's diff hasn't been acquired (or failed to acquire) yet —
    // both cases have an empty `files()` list, so they must be checked BEFORE the "(no changes)"
    // fallback below, which would otherwise misreport a Pending/Failed changeset as an
    // intentionally empty one.
    if let Some(message) = app.current_failure() {
        let msg = format!("Failed to load this changeset: {message}");
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(theme.error_fg)),
            area,
        );
        return;
    }
    if app.is_current_pending() {
        frame.render_widget(
            Paragraph::new("Loading\u{2026}").style(Style::default().fg(theme.dim)),
            area,
        );
        return;
    }
    if app.files().is_empty() {
        frame.render_widget(Paragraph::new("(no changes)"), area);
        return;
    }

    let idx = app.current;
    if app.files()[idx].is_binary {
        let msg = format!("[Binary file: {}]", app.files()[idx].path);
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(theme.dim)),
            area,
        );
        return;
    }

    // CS4: in defer mode, selection changes never load — the diff body shows a placeholder until
    // the event loop's idle window (`tui.rs`'s `OPEN_DEBOUNCE`) runs `complete_pending_open`
    // between frames. Do NOT call `ensure_loaded` from this path in defer mode; outside defer mode
    // (the default), behavior is unchanged.
    if app.defer_loads() && needs_deferred_placeholder(app, idx) {
        render_loading_placeholder(frame, app, area, idx, theme);
        return;
    }
    if !app.defer_loads() {
        app.ensure_loaded(idx);
    }

    // The gate re-evaluates the effective zoom for the current file every frame (no caching —
    // ratatui relayout is free, per locked decision #3).
    match app.effective_zoom_for(idx) {
        EffectiveZoom::Single(role) => {
            app.pane_height = area.height as usize;
            app.hit_regions.single = Some(region_from(area));
            let scroll = app.scroll;
            let cursor = Some(app.cursor);
            // The single pane is the focused one, so it shows any active selection.
            let selection = app.selection_range();
            // CS1 (`unfocused-cursor-wash`, locked decision #1): the single/combined diff body's
            // cursor dims to the unfocused wash while the outline holds focus instead — it never
            // holds real focus itself in that state.
            let focused = !app.outline_focused();
            match app.layout {
                AppLayout::Sbs => render_pane_sbs(
                    frame, app, area, idx, role, scroll, cursor, selection, theme, focused,
                ),
                AppLayout::Inline => render_pane_inline(
                    frame, app, area, idx, role, scroll, cursor, selection, theme, focused,
                ),
            }
        }
        EffectiveZoom::Split => render_body_split(frame, app, area, idx, theme),
    }
}

/// Render the two-pane split: unstaged pane on top, staged on the bottom, each with a dim role
/// caption, each rendering its role view in the current [`AppLayout`] with its OWN cursor+scroll —
/// the cursor highlight draws only in the focused pane. The body area splits caption(1) +
/// unstaged-content + caption(1) + staged-content, with the remainder halved between the two
/// content panes (even split).
fn render_body_split(frame: &mut Frame, app: &mut App, area: Rect, idx: usize, theme: &Palette) {
    // CS1 (`focused-pane-header`, locked decision #5's split case): the outline holding focus
    // dims BOTH captions (the outline header is the frame's one lit label); otherwise exactly the
    // focused half's caption lights up, matching `split_focus_role()` — never derived from the
    // requested `Zoom`, since this fn only ever runs once `effective_zoom_for` has already
    // resolved to `Split` (see `render_body`'s caller).
    let outline_focused = app.outline_focused();
    // Too short to fit two captions plus a content line each: fall back to the focused pane alone,
    // rendered over the whole area, so the user still sees SOMETHING navigable.
    if area.height < 4 {
        let role = app.split_focus_role();
        app.pane_height = area.height as usize;
        let (scroll, cursor) = app.pane_render_state(role);
        let selection = app.selection_range();
        // `split_focus_role()`'s pane is only the frame's REAL focus while the outline doesn't
        // hold it (same rule as the split's two-caption branch below).
        let focused = !outline_focused;
        match app.layout {
            AppLayout::Sbs => render_pane_sbs(
                frame, app, area, idx, role, scroll, cursor, selection, theme, focused,
            ),
            AppLayout::Inline => render_pane_inline(
                frame, app, area, idx, role, scroll, cursor, selection, theme, focused,
            ),
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
    app.hit_regions.unstaged = Some(region_from(unstaged_content));
    app.hit_regions.staged = Some(region_from(staged_content));
    // Bounds-clamp only (peek model — see render_outline's identical note); this also brings
    // the split arm in line with the Single arm, which never re-derived at render time.
    app.clamp_scroll();
    app.clamp_alt_scroll();

    // Each half's REAL focus (CS1, `unfocused-cursor-wash` — locked decisions #1/#5): the outline
    // holding focus means neither half does. Computed once here and reused by both
    // `render_caption` calls below and the pane render calls further down; a selection lives in
    // the focused pane only, so it gates on `focused` too (not on `cursor`, which the unfocused
    // half now always carries — its remembered position, per `App::pane_render_state`'s updated
    // doc comment).
    let u_focused = !outline_focused && app.split_focus_role() == Role::Unstaged;
    let s_focused = !outline_focused && app.split_focus_role() == Role::Staged;

    render_caption(
        frame.buffer_mut(),
        unstaged_caption,
        "UNSTAGED",
        theme,
        u_focused,
    );
    render_caption(
        frame.buffer_mut(),
        staged_caption,
        "STAGED",
        theme,
        s_focused,
    );

    let (u_scroll, u_cursor) = app.pane_render_state(Role::Unstaged);
    let (s_scroll, s_cursor) = app.pane_render_state(Role::Staged);
    let range = app.selection_range();
    let u_selection = if u_focused { range } else { None };
    let s_selection = if s_focused { range } else { None };
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
                u_selection,
                theme,
                u_focused,
            );
            render_pane_sbs(
                frame,
                app,
                staged_content,
                idx,
                Role::Staged,
                s_scroll,
                s_cursor,
                s_selection,
                theme,
                s_focused,
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
                u_selection,
                theme,
                u_focused,
            );
            render_pane_inline(
                frame,
                app,
                staged_content,
                idx,
                Role::Staged,
                s_scroll,
                s_cursor,
                s_selection,
                theme,
                s_focused,
            );
        }
    }
}

/// Write a split pane's role caption (`── LABEL ────…`) across the FULL pane width — the rule
/// runs to the right edge so the staged pane's caption row doubles as the horizontal divider
/// between the split's two panes, matching the outline↔diff and side-by-side `│` rules (same
/// `theme.dim`) without spending a dedicated divider row. The `──` rule characters always stay
/// `theme.dim` (locked decision #4, `focused-pane-header` — label text only); only the label
/// word itself takes [`pane_header_label_style`], lit while `focused`.
fn render_caption(buf: &mut Buffer, area: Rect, label: &str, theme: &Palette, focused: bool) {
    let rule_style = Style::default().fg(theme.dim);
    let used = 3 + label.chars().count() + 1; // "── " + label + " "
    let fill = (area.width as usize).saturating_sub(used);
    let line = Line::from(vec![
        TSpan::styled("── ", rule_style),
        TSpan::styled(label.to_string(), pane_header_label_style(theme, focused)),
        TSpan::styled(format!(" {}", "─".repeat(fill)), rule_style),
    ]);
    buf.set_line(area.x, area.y, &line, area.width);
}

/// Render one SBS pane of `role`'s view for file `idx` into `area`, scrolled to `scroll`. The
/// cursor-row highlight draws whenever `cursor` is `Some` and matches a visible row — this now
/// includes an unfocused split half's REMEMBERED cursor (CS1, `unfocused-cursor-wash`; previously
/// unfocused passed `None` and drew no cursor at all). `focused` says which wash that row gets:
/// full [`Palette::cursor_bg`] when this pane holds real focus, the dim
/// [`Palette::cursor_unfocused_bg`] otherwise — resolved by the caller from app state
/// (`outline_focused`, `split_focus_role`), never guessed here from `cursor`/`selection` alone.
#[allow(clippy::too_many_arguments)]
fn render_pane_sbs(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    idx: usize,
    role: Role,
    scroll: usize,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    theme: &Palette,
    focused: bool,
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
    // One offset shared by every content pane (locked decision #1) — read once, before any of
    // the `app` borrows below.
    let hscroll = app.hscroll;

    let Some(view) = app.role_view_ref(idx, role) else {
        frame.render_widget(Paragraph::new("(failed to load file)"), old_area);
        return;
    };
    let old_gutter_w = gutter_width(view.old_line_count());
    let new_gutter_w = gutter_width(view.new_line_count());
    let end = (scroll + area.height as usize).min(view.display.len());

    // Built once per frame, not per row/cached on `App` — see `combined_attribution`'s doc
    // comment. `None` for non-combined roles, which don't need it.
    let attribution = combined_attribution(app, idx, role);
    let mode = attribution_mode(role, &attribution);

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
            .set_string(div_area.x, y, "│", Style::default().fg(theme.dim));
    }

    for (i, row_idx) in (scroll..end).enumerate() {
        let y = area.y + i as u16;
        let is_cursor = cursor == Some(row_idx);
        let is_selected = selection.is_some_and(|(lo, hi)| row_idx >= lo && row_idx <= hi);
        match &view.display[row_idx] {
            DisplayRow::Gap { skipped, .. } => {
                render_gap_row(
                    frame.buffer_mut(),
                    area,
                    y,
                    *skipped,
                    is_cursor,
                    is_selected,
                    theme,
                    focused,
                );
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
                    mode,
                    old_gutter_w,
                    old_area.width as usize,
                    theme,
                    hscroll,
                );
                let new_line = build_pane_line(
                    view,
                    Side::New,
                    row.new,
                    row.new_kind,
                    &new_words,
                    is_pair,
                    mode,
                    new_gutter_w,
                    new_area.width as usize,
                    theme,
                    hscroll,
                );
                // Cursor wins over selection on the same row (see [`Palette::selection_bg`]).
                let (old_line, new_line) = if is_cursor {
                    (
                        apply_cursor_row(old_line, old_area.width, theme, focused),
                        apply_cursor_row(new_line, new_area.width, theme, focused),
                    )
                } else if is_selected {
                    (
                        apply_selection_row(old_line, old_area.width, theme),
                        apply_selection_row(new_line, new_area.width, theme),
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
                // Right-edge hscroll marker (decision #7) — applied AFTER `set_line` (and thus
                // after the cursor/selection wash above, which already painted the background)
                // so it survives on a cursor/selected row; `apply_right_edge_marker` only sets
                // `fg`, leaving whatever background the wash left in place.
                apply_right_edge_marker(frame.buffer_mut(), old_area, y, &old_line, theme);
                apply_right_edge_marker(frame.buffer_mut(), new_area, y, &new_line, theme);
                // The divider column was painted once for the whole pane height above, with the
                // default background; re-tint just this row's divider cell so the cursor wash
                // covers the full width (panes AND the `│` between them), like `render_gap_row`.
                // Must carry whichever wash the row actually got — full when `focused`, dim
                // otherwise — or the divider cell stays bright on a dimmed row.
                if is_cursor {
                    let tint = cursor_tint(theme, focused);
                    frame.buffer_mut().set_string(
                        div_area.x,
                        y,
                        "│",
                        Style::default().fg(theme.dim).bg(tint),
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
#[allow(clippy::too_many_arguments)]
fn build_inline_line(
    view: &FileView,
    row: &InlineRow,
    word_spans: &[WordSpan],
    mode: AttributionMode,
    old_gutter_w: usize,
    new_gutter_w: usize,
    theme: &Palette,
    hscroll: usize,
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
    let mut spans = vec![TSpan::styled(gutter, Style::default().fg(theme.gutter))];

    let is_word_pair = row.is_word_diff_pair();
    // `kind` is always Del/Add/Context here — inline has no Filler rows. `old_opt`/`new_opt`
    // carry the exact lineno each kind is documented to have (see this fn's own match above).
    let emphasis = match kind {
        CellKind::Del => old_opt.map(|n| del_bg_pair(mode, n as u32, theme)),
        CellKind::Add => new_opt.map(|n| add_bg_pair(mode, n as u32, theme)),
        CellKind::Context | CellKind::Filler => None,
    };
    spans.extend(content_spans(
        text,
        hl,
        emphasis,
        word_spans,
        is_word_pair,
        theme,
        hscroll,
    ));
    Line::from(spans)
}

/// Render one inline pane of `role`'s view for file `idx` into `area`, scrolled to `scroll`. See
/// [`render_pane_sbs`] for the `cursor`/highlight contract; this is its inline-coordinate-space
/// analog.
#[allow(clippy::too_many_arguments)]
fn render_pane_inline(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    idx: usize,
    role: Role,
    scroll: usize,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    theme: &Palette,
    focused: bool,
) {
    // One offset shared by every content pane (locked decision #1) — read once, before any of
    // the `app` borrows below.
    let hscroll = app.hscroll;

    let Some(view) = app.role_view_ref(idx, role) else {
        frame.render_widget(Paragraph::new("(failed to load file)"), area);
        return;
    };
    let old_gutter_w = gutter_width(view.old_line_count());
    let new_gutter_w = gutter_width(view.new_line_count());
    let end = (scroll + area.height as usize).min(view.inline.len());

    // See `render_pane_sbs`'s identical comment — built once per frame, not cached on `App`.
    let attribution = combined_attribution(app, idx, role);
    let mode = attribution_mode(role, &attribution);

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
        let is_selected = selection.is_some_and(|(lo, hi)| row_idx >= lo && row_idx <= hi);
        match &view.inline[row_idx] {
            InlineRow::Gap { skipped, .. } => {
                render_gap_row(
                    frame.buffer_mut(),
                    area,
                    y,
                    *skipped,
                    is_cursor,
                    is_selected,
                    theme,
                    focused,
                );
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
                let line = build_inline_line(
                    view,
                    row,
                    word_spans,
                    mode,
                    old_gutter_w,
                    new_gutter_w,
                    theme,
                    hscroll,
                );
                // Cursor wins over selection on the same row (see [`Palette::selection_bg`]).
                let line = if is_cursor {
                    apply_cursor_row(line, area.width, theme, focused)
                } else if is_selected {
                    apply_selection_row(line, area.width, theme)
                } else {
                    line
                };
                frame.buffer_mut().set_line(area.x, y, &line, area.width);
                // Right-edge hscroll marker (decision #7) — see `render_pane_sbs`'s identical
                // comment on ordering relative to the cursor/selection wash above.
                apply_right_edge_marker(frame.buffer_mut(), area, y, &line, theme);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::Span as TSpan;
    use ratatui::Terminal;

    use git_workon_fixture::prelude::*;
    use unicode_width::UnicodeWidthChar;

    use super::{
        changeset_prefix_spans, hscroll_cut, pan_spans, pane_header_label_style, render,
        STATUS_PLACEHOLDER,
    };
    use crate::align::{DisplayRow, Row};
    use crate::app::test_support::app_from_fixture;
    use crate::app::{App, EffectiveZoom, Role};
    use crate::keymap::Keymap;
    use crate::outline::OutlineItem;
    use crate::theme::Palette;

    /// Render one frame against the default (unrebound) keymap and the dark theme — the vast
    /// majority of `render.rs` tests don't care about keybindings and only ever ran dark. Tests
    /// that DO care about bindings (the footer/overlay content tests) build their own [`Keymap`]
    /// and call [`render`] directly instead. Color assertions resolve through [`Palette::dark`], so
    /// they pin the exact dark values the refactor must preserve (ADR-029's pixel-identity gate).
    fn render_once(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = Keymap::defaults();
        let theme = Palette::dark();
        terminal.draw(|f| render(f, app, &keymap, &theme)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Like [`render_once`] but with a caller-chosen theme — for the canvas-paint tests, which
    /// need to compare `light` vs `dark` (not just always-dark).
    fn render_once_themed(app: &mut App, width: u16, height: u16, theme: &Palette) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let keymap = Keymap::defaults();
        terminal.draw(|f| render(f, app, &keymap, theme)).unwrap();
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

    /// Find the row (by line index) whose caption reads `label` — e.g. "UNSTAGED" or "STAGED".
    /// The `label != "STAGED" || !line.contains("UNSTAGED")` guard disambiguates the two: the
    /// UNSTAGED caption row's tail can itself contain the substring "STAGED".
    fn caption_row(content: &[String], label: &str) -> usize {
        content
            .iter()
            .position(|line| {
                line.contains(label) && (label != "STAGED" || !line.contains("UNSTAGED"))
            })
            .unwrap_or_else(|| panic!("{label} caption present"))
    }

    /// Find the first row in `start..end` whose text (columns `x0..buf.area.width`, so callers can
    /// exclude an outline/gutter to the left) contains `text`. Used by the split-half cursor-wash
    /// tests to locate each pane's cursor row bounded to that pane's own row range, disambiguating
    /// text that appears once per pane.
    fn find_row(buf: &Buffer, x0: u16, start: usize, end: usize, text: &str) -> u16 {
        (start..end)
            .find(|&y| {
                (x0..buf.area.width)
                    .map(|x| cell_text(buf, x, y as u16))
                    .collect::<String>()
                    .contains(text)
            })
            .unwrap_or_else(|| panic!("row containing {text:?} not found in {start}..{end}"))
            as u16
    }

    /// Find `label`'s starting display COLUMN within `row` — a `chars()` window search (not
    /// `String::find`'s byte offset), matching the convention several outline/summary-header tests
    /// already use for a row that may carry multi-byte glyphs (`•`/`⚠`) ahead of the label; every
    /// rendered cell here is exactly one column wide, so a `chars()` position IS the display
    /// column, as long as `row` starts at buffer column 0 (true for every `buf_lines` row).
    fn find_label_x(row: &str, label: &str) -> u16 {
        let label_chars: Vec<char> = label.chars().collect();
        let row_chars: Vec<char> = row.chars().collect();
        row_chars
            .windows(label_chars.len())
            .position(|w| w == label_chars.as_slice())
            .unwrap_or_else(|| panic!("label {label:?} not found in row {row:?}")) as u16
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

    // ── CS4: idle-deferred loads ──────────────────────────────────────────────

    #[test]
    fn defer_mode_shows_placeholder_and_does_not_load() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("tracked.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.set_defer_loads(true);
        app.open_current(); // marks pending; does not load

        let buf = render_once(&mut app, 60, 10);
        let content = buf_lines(&buf);
        assert!(
            content
                .iter()
                .any(|line| line.contains("tracked.txt") && line.contains("loading")),
            "expected the CS4 loading placeholder, got:\n{}",
            content.join("\n")
        );
        assert!(
            app.current_view_ref().is_none(),
            "rendering in defer mode must not have triggered a load"
        );
    }

    #[test]
    fn non_defer_mode_still_loads_from_the_render_path() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("tracked.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        // `defer_loads` defaults off — render must still load eagerly, exactly like before CS4.
        let _ = render_once(&mut app, 60, 10);
        assert!(
            app.current_view_ref().is_some(),
            "non-defer mode must still load from the render path"
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
            Some(Palette::dark().cursor_bg),
            "expected the cursor row's DIVIDER cell to carry the cursor background, not the \
             default — otherwise the highlight has a seam through the middle"
        );
    }

    #[test]
    fn selected_rows_carry_the_selection_tint_distinct_from_cursor_and_plain_rows() {
        // Anchor at l10 and put the cursor at l12, so the selection covers l10..=l12. The cursor
        // (always one endpoint of the range) sits on l12 and wins the wash there; l10 and l11 are
        // selected-but-not-cursor, showing the pure selection tint over plain context.
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nold word here\nl10\nl11\nl12\nl13\nl14\n";
        let new = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nnew word here\nl10\nl11\nl12\nl13\nl14\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", old, new)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();

        let row_for = |app: &mut App, lineno: usize| {
            app.current_view_ref()
                .unwrap()
                .display
                .iter()
                .position(|row| matches!(row, DisplayRow::Row(r) if r.old == Row::Line(lineno)))
                .unwrap_or_else(|| panic!("l{lineno} row present"))
        };
        let l10 = row_for(&mut app, 10);
        let l12 = row_for(&mut app, 12);

        app.selection_anchor = Some(l10);
        app.cursor = l12;
        assert_eq!(app.selection_range(), Some((l10, l12)));

        let buf = render_once(&mut app, 60, 20);
        let content = buf_lines(&buf);
        let y_of = |needle: &str| {
            content
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle} visible")) as u16
        };
        let sel_y = y_of("l10 "); // selected, not cursor
        let other_y = y_of("l11 "); // selected, not cursor — same tint as l10
        let cursor_y = y_of("l12 "); // cursor endpoint of the range — cursor tint wins here

        let bg = |x: u16, y: u16| buf.cell((x, y)).unwrap().style().bg;
        assert_eq!(
            bg(1, sel_y),
            bg(1, other_y),
            "both selected rows carry the same selection tint"
        );
        assert_ne!(
            bg(1, sel_y),
            bg(1, cursor_y),
            "the selection tint must differ from the cursor row's tint"
        );
        // And the selection tint is specifically BG_SELECTION blended over plain context (which
        // has no bg) — i.e. the raw tint, since blend_bg(None, tint) == tint.
        assert_eq!(
            bg(1, sel_y),
            Some(Palette::dark().selection_bg),
            "a selected plain-context row shows the raw selection tint"
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
    fn split_captions_rule_runs_the_full_pane_width_as_the_pane_divider() {
        // The staged caption row is the only seam between the split's two panes — its rule must
        // reach the right edge to read as a divider (dogfood feedback: the split lacked a rule
        // like the outline↔diff and side-by-side ones).
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

        for label in ["UNSTAGED", "STAGED"] {
            let cap = content
                .iter()
                .position(|line| {
                    line.contains(label) && (label != "STAGED" || !line.contains("UNSTAGED"))
                })
                .expect("caption present");
            let row = content[cap].trim_end();
            assert_eq!(
                row.chars().count(),
                80,
                "{label} caption must span the full pane width, got: {row:?}"
            );
            assert_eq!(
                row.chars().last(),
                Some('─'),
                "{label} caption must end in the rule glyph, got: {row:?}"
            );
        }
    }

    #[test]
    fn single_pane_zoom_is_identical_to_combined_for_an_unstaged_only_file() {
        // The common case: a dirty-but-unstaged file. The default split gate downgrades it to a
        // single unstaged pane, whose view is byte-for-byte the combined view (index == HEAD when
        // nothing is staged) — so a user who never presses `Z` sees exactly the pre-zoom app.
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

    #[test]
    fn combined_view_colors_a_staged_change_dim_and_an_unstaged_change_bright() {
        // A partially-staged file with two independent word changes: line 2 was already staged
        // (committed -> staged both carry the change), line 4 is still only in the worktree
        // (staged -> workdir carries it, index doesn't). The combined view (HEAD <-> worktree)
        // fuses both into one set of rows — attribution must tell them apart: line 2's change
        // should render with the dim (staged) pair, line 4's with the bright (not-yet-staged)
        // pair, on BOTH the Del (old) and Add (new) side of each row (the add/del asymmetry:
        // Del keys off the staged sub-diff, Add off the unstaged sub-diff).
        let committed = "l1\nold word here\nl3\nold4 word four\nl5\n";
        let staged = "l1\nnew word here\nl3\nold4 word four\nl5\n";
        let workdir = "l1\nnew word here\nl3\nnew4 word four\nl5\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .partially_staged_file("f.txt", committed, staged, workdir)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.cycle_zoom(); // Split -> Combined
        assert_eq!(app.zoom, crate::app::Zoom::Combined);
        // Park the cursor on the file's first (context) row so its highlight tint doesn't blend
        // into either changed row's background and muddy the color comparison below.
        app.cursor = 0;
        app.derive_scroll();

        let buf = render_once(&mut app, 60, 20);
        let content = buf_lines(&buf);

        let staged_row = content
            .iter()
            .position(|line| line.contains("old word here"))
            .expect("staged change's old-side text visible");
        let unstaged_row = content
            .iter()
            .position(|line| line.contains("old4 word four"))
            .expect("unstaged change's old-side text visible");

        // Old (left) pane, first content column after the gutter — always carries SOME del
        // emphasis on a changed row, subtle or strong depending on the word-diff split, but
        // always from the dim family for a staged row and the bright family for an unstaged one.
        let old_content_x = 4; // gutter width 3 + 1 space, same convention as the other tests
        let staged_del_bg = buf
            .cell((old_content_x, staged_row as u16))
            .unwrap()
            .style()
            .bg;
        let unstaged_del_bg = buf
            .cell((old_content_x, unstaged_row as u16))
            .unwrap()
            .style()
            .bg;

        let t = Palette::dark();
        let dim_dels = [Some(t.del_staged_subtle), Some(t.del_staged_strong)];
        let bright_dels = [Some(t.del_subtle), Some(t.del_strong)];
        assert!(
            dim_dels.contains(&staged_del_bg),
            "expected the staged row's Del side to use the dim pair, got {staged_del_bg:?}"
        );
        assert!(
            bright_dels.contains(&unstaged_del_bg),
            "expected the unstaged row's Del side to use the bright pair, got {unstaged_del_bg:?}"
        );
        assert_ne!(
            staged_del_bg, unstaged_del_bg,
            "staged and unstaged Del rows must render with visibly distinct backgrounds"
        );

        // New (right) pane: same rows carry "new word here" / "new4 word four" respectively.
        let left_w = (buf.area.width.saturating_sub(1)) / 2;
        let new_content_x = left_w + 1 + 4; // divider + gutter width 3 + 1 space
        let staged_add_bg = buf
            .cell((new_content_x, staged_row as u16))
            .unwrap()
            .style()
            .bg;
        let unstaged_add_bg = buf
            .cell((new_content_x, unstaged_row as u16))
            .unwrap()
            .style()
            .bg;

        let dim_adds = [Some(t.add_staged_subtle), Some(t.add_staged_strong)];
        let bright_adds = [Some(t.add_subtle), Some(t.add_strong)];
        assert!(
            dim_adds.contains(&staged_add_bg),
            "expected the staged row's Add side to use the dim pair, got {staged_add_bg:?}"
        );
        assert!(
            bright_adds.contains(&unstaged_add_bg),
            "expected the unstaged row's Add side to use the bright pair, got {unstaged_add_bg:?}"
        );
        assert_ne!(
            staged_add_bg, unstaged_add_bg,
            "staged and unstaged Add rows must render with visibly distinct backgrounds"
        );
    }

    #[test]
    fn footer_shows_hint_string_when_no_notice_is_set() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        assert!(app.notice.is_none());

        let buf = render_once(&mut app, 80, 10);
        let footer_y = buf.area.height - 1;
        let footer: String = (0..buf.area.width)
            .map(|x| cell_text(&buf, x, footer_y))
            .collect();
        assert!(
            footer.contains("j/k move") && footer.contains("? help"),
            "expected the curated diff hint string in the footer, got: {footer:?}"
        );
    }

    #[test]
    fn footer_shows_the_outline_hint_when_the_outline_has_focus() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        // A lone uncommitted changeset never auto-opens the outline (M4 default) — force it open
        // + focused so `render_footer` takes the outline-focused branch.
        app.toggle_outline();
        assert!(app.outline_focused());

        let buf = render_once(&mut app, 80, 10);
        let footer_y = buf.area.height - 1;
        let footer: String = (0..buf.area.width)
            .map(|x| cell_text(&buf, x, footer_y))
            .collect();
        assert!(
            footer.contains("open")
                && footer.contains(&format!(
                    "i \u{2192}{}",
                    crate::outline::OutlineMode::StackTree.label()
                ))
                && footer.contains("? help"),
            "expected the curated outline hint string, with CS4's dynamic next-mode label \
             (Stack's default -> StackTree), in the footer, got: {footer:?}"
        );
    }

    #[test]
    fn footer_outline_hint_next_mode_label_updates_as_the_mode_cycles() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.toggle_outline();
        assert!(app.outline_focused());
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Stack);

        let footer_text = |app: &mut App| {
            let buf = render_once(app, 80, 10);
            let footer_y = buf.area.height - 1;
            (0..buf.area.width)
                .map(|x| cell_text(&buf, x, footer_y))
                .collect::<String>()
        };

        let footer = footer_text(&mut app);
        assert!(
            footer.contains("i \u{2192}stack-tree"),
            "Stack's next mode is StackTree; got: {footer:?}"
        );

        app.outline_cycle_mode();
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::StackTree);
        let footer = footer_text(&mut app);
        assert!(
            footer.contains("i \u{2192}flat"),
            "StackTree's next mode is Flat; got: {footer:?}"
        );
    }

    #[test]
    fn footer_renders_a_rebound_key_not_the_default() {
        use crate::config::RawBinding;
        use crate::config::View as CfgView;
        use crate::keymap::Keymap;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        assert!(app.notice.is_none());

        let keymap = Keymap::from_bindings(&[RawBinding {
            view: CfgView::Diff,
            action: "stage-hunk".to_string(),
            keys: "x".to_string(),
        }]);

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Palette::dark();
        terminal
            .draw(|f| render(f, &mut app, &keymap, &theme))
            .unwrap();
        let buf = terminal.backend().buffer().clone();

        let footer_y = buf.area.height - 1;
        let footer: String = (0..buf.area.width)
            .map(|x| cell_text(&buf, x, footer_y))
            .collect();
        assert!(
            footer.contains("x stage") && !footer.contains("s stage"),
            "expected the REBOUND key in the footer, got: {footer:?}"
        );
    }

    #[test]
    fn footer_shows_an_error_notice_in_the_error_fg_color() {
        use crate::app::Severity;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.notify("cannot discard: nothing staged", Severity::Error);

        let buf = render_once(&mut app, 80, 10);
        let footer_y = buf.area.height - 1;
        let footer: String = (0..buf.area.width)
            .map(|x| cell_text(&buf, x, footer_y))
            .collect();
        assert!(
            footer.contains("cannot discard: nothing staged"),
            "expected the notice text in the footer, got: {footer:?}"
        );
        assert_eq!(
            buf.cell((0, footer_y)).unwrap().style().fg,
            Some(Palette::dark().error_fg),
            "expected the error notice to render in the error fg color"
        );
    }

    #[test]
    fn footer_shows_a_pending_confirm_prompt_over_any_notice() {
        use crate::app::{PendingOp, Severity};

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        // A notice is set, but a pending confirm outranks it on the footer surface.
        app.notify("some earlier notice", Severity::Info);
        app.request_confirm(
            "Discard this hunk from the worktree? (y/n)",
            PendingOp::DiscardFile { file_idx: 0 },
        );

        let buf = render_once(&mut app, 80, 10);
        let footer_y = buf.area.height - 1;
        let footer: String = (0..buf.area.width)
            .map(|x| cell_text(&buf, x, footer_y))
            .collect();
        assert!(
            footer.contains("Discard this hunk from the worktree?"),
            "expected the confirm prompt in the footer, got: {footer:?}"
        );
        assert!(
            !footer.contains("some earlier notice"),
            "the confirm prompt must take priority over the notice, got: {footer:?}"
        );
    }

    // ── CS1 (`pane-headers`): outline header + diff header, replacing the old global winbar ────

    /// Build a two-committed-changeset stack for the pane-header tests, hand-built the same way as
    /// `app.rs`'s M5 CS1 tests (`Changeset` literal + `diff_changeset` +
    /// `ChangesetView::from_changeset_diff`): `cs-a` (`root..mid`, one file) then `cs-b`
    /// (`mid..head`, one file, `current` + `needs_restack`).
    fn two_committed_changesets_app(fixture: &Fixture) -> App {
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};

        use crate::app::ChangesetView;

        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let mid = fixture
            .commit("main")
            .file("a.txt", "a\n")
            .create("mid")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("b.txt", "b\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs_a = Changeset {
            name: "cs-a".to_string(),
            span: ChangesetSpan::Committed {
                base: root,
                head: mid,
            },
            title: Some("Add a".to_string()),
            current: false,
            needs_restack: false,
        };
        let cs_b = Changeset {
            name: "cs-b".to_string(),
            span: ChangesetSpan::Committed { base: mid, head },
            title: None,
            current: true,
            needs_restack: true,
        };

        let view_a = ChangesetView::from_changeset_diff(
            cs_a.clone(),
            crate::acquire::diff_changeset(repo, &cs_a).unwrap(),
        );
        let view_b = ChangesetView::from_changeset_diff(
            cs_b.clone(),
            crate::acquire::diff_changeset(repo, &cs_b).unwrap(),
        );

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        app.open_current();
        app
    }

    #[test]
    fn outline_header_shows_changeset_position_title_and_restack_marker() {
        // CS1: with the outline open (a two-changeset stack's default), the changeset-position
        // context lives in the OUTLINE pane's own header, not the diff pane's — the outline
        // columns are x 0..35 at this width (see `OUTLINE_TEST_WIDTH`'s doc comment below).
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(app.outline_open(), "a two-changeset stack default-opens");

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..35).map(|x| cell_text(&buf, x, 0)).collect();

        assert!(
            header.contains("[2/2]"),
            "expected the changeset position counter, got: {header:?}"
        );
        assert!(
            header.contains("cs-b"),
            "expected the active changeset's name (no title set), got: {header:?}"
        );
        assert!(
            header.contains("needs restack"),
            "expected the needs-restack marker, got: {header:?}"
        );
    }

    #[test]
    fn diff_header_shows_the_active_files_position_diffstat_and_path_when_outline_open() {
        // CS1: with the outline open, the diff header shows ONLY the file segment (no changeset
        // prefix — the outline's own header already carries that) — diff columns are x 36.. at
        // this width.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(app.outline_open());

        let buf = render_once(&mut app, 80, 20);
        let header: String = (36..buf.area.width)
            .map(|x| cell_text(&buf, x, 0))
            .collect();

        assert!(
            header.contains("[1/1]") && header.contains("b.txt"),
            "expected the active file's position and path, got: {header:?}"
        );
        // CS4: a tight '+A -D' diffstat for the ACTIVE FILE (b.txt, one-line file, committed
        // with no prior content, adds one line and deletes nothing) — CS1 is what made this
        // PER-FILE (the old winbar only ever showed a changeset-total diffstat).
        assert!(
            header.contains("+1") && header.contains("-0"),
            "expected a tight '+N -M' per-file diffstat fragment, got: {header:?}"
        );
        assert!(
            !header.contains("[2/2]"),
            "outline open: the diff header must not repeat the changeset-position prefix, \
             got: {header:?}"
        );
    }

    #[test]
    fn diff_header_carries_the_changeset_prefix_when_outline_closed() {
        // CS1: closing the outline removes the pane that carried changeset-position context, so
        // the diff header grows a `[i/n] <title-or-name> <restack-glyph>  —  ` prefix ahead of
        // the file segment — this is what the old winbar used to show unconditionally.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline();
        assert!(!app.outline_open());

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();

        assert!(
            header.contains("[2/2]") && header.contains("cs-b"),
            "expected the changeset position counter and active changeset's name, \
             got: {header:?}"
        );
        // The diff header's changeset prefix is glyph-ONLY (no "needs restack" text — that
        // fuller treatment is the outline header's, see `changeset_prefix_spans`'s doc comment).
        assert!(
            header.contains('⚠'),
            "expected the needs-restack glyph, got: {header:?}"
        );
        assert!(
            header.contains("[1/1]") && header.contains("b.txt"),
            "expected the active file's position and path, got: {header:?}"
        );
        assert!(
            header.contains("+1") && header.contains("-0"),
            "expected the per-file diffstat fragment, got: {header:?}"
        );
    }

    #[test]
    fn diff_header_restack_marker_carries_the_warning_color_when_outline_closed() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline();

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        let marker_x = header.find('⚠').expect("restack glyph present") as u16;
        assert_eq!(
            buf.cell((marker_x, 0)).unwrap().style().fg,
            Some(Palette::dark().warn_fg),
            "expected the restack glyph to carry the warning color, not the plain header color"
        );
    }

    #[test]
    fn diff_header_nerd_mode_swaps_the_restack_marker_and_diffstat_glyphs_and_shows_a_file_icon() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture); // cs-b: current + needs_restack
        app.toggle_outline();
        app.set_icon_mode(crate::icons::IconMode::Nerd);

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        assert!(
            header.contains(super::NERD_WARN_MARKER) && !header.contains('\u{26A0}'),
            "expected the nerd restack marker, not the plain unicode one, got: {header:?}"
        );
        assert!(
            header.contains(super::NERD_DIFF_ADDED) && header.contains(super::NERD_DIFF_REMOVED),
            "expected nerd diffstat glyphs in the diff header, got: {header:?}"
        );
        assert!(
            header.contains(crate::icons::icon_for_path("b.txt", false).0),
            "expected the active file's (b.txt) devicons icon in the diff header, got: {header:?}"
        );
    }

    #[test]
    fn diff_header_uses_title_when_present() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.prev_changeset();
        app.toggle_outline();

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        assert!(
            header.contains("Add a"),
            "expected the changeset's title, not its bare name, got: {header:?}"
        );
        assert!(
            !header.contains("needs restack"),
            "cs-a is not stale, so no restack marker should show, got: {header:?}"
        );
    }

    #[test]
    fn diff_header_lone_changeset_shows_file_counter_and_no_changeset_chrome() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        assert!(
            header.contains("[1/1]"),
            "a lone changeset keeps the M4 `[fidx/nfiles]` file counter, got: {header:?}"
        );
        assert!(
            !header.contains('⚠'),
            "a lone changeset must not render the changeset-prefix chrome, got: {header:?}"
        );
    }

    #[test]
    fn diff_header_shows_a_per_file_diffstat_for_a_lone_changeset() {
        // CS1: new behavior — pre-CS1, the lone-changeset header never showed a diffstat at all
        // (only the multi-changeset winbar did, and only a CHANGESET total). The file segment now
        // carries a per-file diffstat in every state, including this one.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        // The fixture only ADDS a line ("CHANGED", appended after the unchanged "one") — nothing
        // is deleted, so the per-file diffstat is `+1 -0`.
        assert!(
            header.contains("+1") && header.contains("-0"),
            "expected a per-file '+N -M' diffstat fragment on the lone-changeset header, \
             got: {header:?}"
        );
    }

    #[test]
    fn pending_changeset_diff_header_shows_no_file_counter() {
        // ADR-031 + CS1: a Pending changeset's `files()` is always empty — the diff header must
        // never show a misleading `[1/0]` file counter, whether the outline is open (a blank
        // row) or closed (the changeset prefix alone, still no file counter).
        use crate::app::ChangesetView;
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let mid = fixture
            .commit("main")
            .file("a.txt", "a\n")
            .create("mid")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs_a = Changeset {
            name: "cs-a".to_string(),
            span: ChangesetSpan::Committed {
                base: root,
                head: mid,
            },
            title: None,
            current: false,
            needs_restack: false,
        };
        let cs_b = Changeset {
            name: "cs-b".to_string(),
            span: ChangesetSpan::Committed {
                base: mid,
                head: mid,
            },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view_a = ChangesetView::from_changeset_diff(
            cs_a.clone(),
            crate::acquire::diff_changeset(repo, &cs_a).unwrap(),
        );
        let view_b = ChangesetView::pending(cs_b);

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        assert!(app.is_current_pending());

        // Outline open (this stack's default): a blank diff-header row, never "[1/0]".
        assert!(app.outline_open());
        let buf = render_once(&mut app, 80, 20);
        let header: String = (36..buf.area.width)
            .map(|x| cell_text(&buf, x, 0))
            .collect();
        assert!(
            !header.contains("[1/0]"),
            "must never show a misleading file counter, got: {header:?}"
        );

        // Outline closed: the changeset prefix alone, still no file counter.
        app.toggle_outline();
        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        assert!(
            header.contains("cs-b"),
            "expected the changeset prefix naming the pending changeset, got: {header:?}"
        );
        assert!(
            !header.contains("[1/0]"),
            "must never show a misleading file counter, got: {header:?}"
        );
    }

    #[test]
    fn committed_changeset_combined_view_skips_attribution_and_renders_plain() {
        // A committed changeset's combined role has no staged/unstaged split to attribute
        // against (`DiffState::from_committed` leaves both sub-models empty) — without the
        // `is_committed` skip in `combined_attribution`, `Attribution::build(None, None)` would
        // still run and its empty `unstaged_adds` set would make EVERY Add cell read as
        // "already staged" (the dim pair), which is wrong: nothing here was staged from
        // anything, it's a committed range. Assert the fix: the Add side renders the plain
        // (bright) pair.
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};

        use crate::app::ChangesetView;

        let committed = "l1\nold word here\nl3\n";
        let head_content = "l1\nnew word here\nl3\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let base = fixture
            .commit("main")
            .file("f.txt", committed)
            .create("base")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("f.txt", head_content)
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs = Changeset {
            name: "main".to_string(),
            span: ChangesetSpan::Committed { base, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let diff = crate::acquire::diff_changeset(repo, &cs).unwrap();
        let view = ChangesetView::from_changeset_diff(cs, diff);
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();
        assert!(app.is_committed());
        // Park the cursor off the changed row so its highlight tint doesn't blend into the Add
        // cell's background and muddy the color comparison below (same convention as
        // `combined_view_colors_a_staged_change_dim_and_an_unstaged_change_bright`).
        app.cursor = 0;
        app.derive_scroll();

        let buf = render_once(&mut app, 60, 20);
        let content = buf_lines(&buf);
        let row_y = content
            .iter()
            .position(|line| line.contains("new word here"))
            .expect("new-side text visible") as u16;

        let left_w = (buf.area.width.saturating_sub(1)) / 2;
        let new_content_x = left_w + 1 + 4; // divider + gutter width 3 + 1 space
        let add_bg = buf.cell((new_content_x, row_y)).unwrap().style().bg;

        let t = Palette::dark();
        let bright_adds = [Some(t.add_subtle), Some(t.add_strong)];
        let dim_adds = [Some(t.add_staged_subtle), Some(t.add_staged_strong)];
        assert!(
            bright_adds.contains(&add_bg),
            "expected a committed changeset's Add cell to render the plain (bright) pair, \
             got {add_bg:?}"
        );
        assert!(
            !dim_adds.contains(&add_bg),
            "a committed changeset has no staged/unstaged split to color by — it must never \
             render the dim 'already staged' pair, got {add_bg:?}"
        );
    }

    // ── diff-hscroll ─────────────────────────────────────────────────────────────

    #[test]
    fn hscroll_cut_ascii() {
        // "hello world" — cutting at column 6 lands right after the space, before "world".
        assert_eq!(hscroll_cut("hello world", 6), (6, false));
        assert_eq!(hscroll_cut("hello world", 0), (0, false));
    }

    #[test]
    fn hscroll_cut_multibyte_narrow() {
        // "café" — 'é' is a single (narrow, non-ASCII) column, so cutting at column 3 lands
        // exactly at its 2-byte UTF-8 start.
        let text = "café";
        assert_eq!(UnicodeWidthChar::width('é'), Some(1));
        let (cut, pad) = hscroll_cut(text, 3);
        assert_eq!(&text[cut..], "é");
        assert!(!pad);
    }

    #[test]
    fn hscroll_cut_wide_cjk_straddling_the_cut_skips_it_and_pads() {
        // "a漢b" — 'a' (col 0), '漢' (cols 1-2, a wide CJK glyph), 'b' (col 3). Cutting at column
        // 2 lands mid-glyph: the whole wide char is dropped and `pad` signals the caller to
        // insert a one-column space to keep the remaining columns aligned.
        let text = "a漢b";
        assert_eq!(UnicodeWidthChar::width('漢'), Some(2));
        let (cut, pad) = hscroll_cut(text, 2);
        assert!(
            pad,
            "a wide char straddling the cut must request a pad column"
        );
        assert_eq!(&text[cut..], "b");
    }

    #[test]
    fn hscroll_cut_emoji() {
        // Most terminal-emulator-relevant emoji are wide (2 columns), like CJK.
        let text = "a🎉b";
        let w = UnicodeWidthChar::width('🎉').unwrap_or(0);
        let (cut, _pad) = hscroll_cut(text, 1 + w);
        assert_eq!(&text[cut..], "b");
    }

    #[test]
    fn hscroll_cut_beyond_line_width_yields_empty() {
        let (cut, pad) = hscroll_cut("short", 100);
        assert_eq!(cut, "short".len());
        assert!(!pad);
        assert_eq!(&"short"[cut..], "");
    }

    // ── mouse h-wheel + outline hscroll follow-up: `pan_spans` ─────────────────────

    fn spans_text(spans: &[TSpan<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn span(text: &str) -> TSpan<'static> {
        TSpan::styled(text.to_string(), Style::default())
    }

    #[test]
    fn pan_spans_at_zero_columns_is_a_pass_through() {
        let theme = Palette::dark();
        let spans = vec![span("hello "), span("world")];
        let panned = pan_spans(spans.clone(), 0, &theme);
        assert_eq!(spans_text(&panned), "hello world");
        assert_eq!(panned.len(), spans.len(), "unchanged, span for span");
    }

    #[test]
    fn pan_spans_cuts_mid_span() {
        // "hello world" panned 3 columns — the cut (plus the marker's reserved column) lands
        // inside the FIRST span ("hello "), leaving its tail attached to the second span.
        let theme = Palette::dark();
        let spans = vec![span("hello "), span("world")];
        let panned = pan_spans(spans, 3, &theme);
        assert_eq!(spans_text(&panned), "…o world");
    }

    #[test]
    fn pan_spans_cuts_exactly_at_a_span_boundary() {
        // "abcdef" as three 2-char spans, panned 2 columns — the cut (plus the marker's reserved
        // column) lands exactly on the boundary between the first and second span.
        let theme = Palette::dark();
        let spans = vec![span("ab"), span("cd"), span("ef")];
        let panned = pan_spans(spans, 2, &theme);
        assert_eq!(spans_text(&panned), "…def");
    }

    #[test]
    fn pan_spans_wide_char_straddling_a_span_edge_drops_and_pads() {
        // "a漢b" as two spans ("a", "漢b"), panned 1 column — the cut (plus the marker's reserved
        // column) straddles the wide CJK glyph at the start of the second span: it's dropped
        // whole and compensated with a one-column space.
        let theme = Palette::dark();
        assert_eq!(UnicodeWidthChar::width('漢'), Some(2));
        let spans = vec![span("a"), span("漢b")];
        let panned = pan_spans(spans, 1, &theme);
        assert_eq!(spans_text(&panned), "… b");
    }

    #[test]
    fn pan_spans_beyond_total_width_yields_just_the_marker() {
        let theme = Palette::dark();
        let spans = vec![span("ab"), span("cd")];
        let panned = pan_spans(spans, 100, &theme);
        assert_eq!(spans_text(&panned), "…");
    }

    #[test]
    fn pan_spans_on_empty_content_is_a_pass_through() {
        let theme = Palette::dark();
        let spans = vec![span("")];
        let panned = pan_spans(spans, 5, &theme);
        assert_eq!(
            spans_text(&panned),
            "",
            "an empty line has nothing to cut, so no marker either"
        );
    }

    /// Build a single unstaged-file `App` with one long line, for the hscroll rendering tests —
    /// long enough that panning by [`crate::app::HSCROLL_STEP`]-sized steps has real room to move
    /// (the tests don't reference that constant directly since it's private to `app.rs`; `200`
    /// just needs to comfortably exceed a test pane's width either way).
    fn app_with_a_long_line() -> App {
        let long_line = "x".repeat(200);
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("long.txt", "short\n", &format!("{long_line}\n"))
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app
    }

    #[test]
    fn panning_right_shows_the_left_edge_marker_and_shifted_content() {
        let mut app = app_with_a_long_line();
        app.hscroll_right();
        assert!(
            app.hscroll > 0,
            "the long line must give hscroll room to pan"
        );

        let buf = render_once(&mut app, 60, 20);
        // divider (1) + new-side gutter ("{n:>3} ", 4 chars) — the new pane's first content
        // column.
        let left_w = buf.area.width.saturating_sub(1) / 2;
        let content_x = left_w + 1 + 4;
        let row_y = (0..buf.area.height)
            .find(|&y| cell_text(&buf, content_x, y) == "…")
            .expect("the panned long line's first visible content column must show the marker");
        assert_eq!(
            cell_text(&buf, content_x + 1, row_y),
            "x",
            "content immediately after the marker must be the (shifted) line body"
        );
    }

    #[test]
    fn a_line_wider_than_the_pane_shows_the_right_edge_marker() {
        let mut app = app_with_a_long_line();
        // At `hscroll == 0` the long line already overflows a narrow pane's content width.
        assert_eq!(app.hscroll, 0);

        let buf = render_once(&mut app, 60, 20);
        let right_x = buf.area.width - 1;
        assert!(
            (0..buf.area.height).any(|y| cell_text(&buf, right_x, y) == "…"),
            "a line wider than the pane must show the right-edge marker"
        );
    }

    #[test]
    fn diff_header_shows_the_pan_offset_indicator_once_panned() {
        // CS1: the pan indicator lives in the file segment, which the diff header always shows
        // (outline open or closed) — with the outline open (this stack's default), that's the
        // diff columns (x 36..) at this width.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert_eq!(app.hscroll, 0);

        let buf_unpanned = render_once(&mut app, 80, 20);
        let header_unpanned: String = (36..buf_unpanned.area.width)
            .map(|x| cell_text(&buf_unpanned, x, 0))
            .collect();
        assert!(
            !header_unpanned.contains('»'),
            "no indicator at hscroll 0, got: {header_unpanned:?}"
        );

        // The fixture files are tiny (`a\n`/`b\n`) — nowhere near wide enough for
        // `hscroll_right` to actually move `hscroll` off `0`. This checks the indicator's own
        // render logic, not the pan mechanics (covered separately in `app.rs`), so setting the
        // field directly is the more honest test: the indicator must key off `App::hscroll`
        // exactly, with no dependency on how it got there.
        app.hscroll = 42;
        let buf = render_once(&mut app, 80, 20);
        let header: String = (36..buf.area.width)
            .map(|x| cell_text(&buf, x, 0))
            .collect();
        assert!(
            header.contains("»42"),
            "expected the pan offset indicator, got: {header:?}"
        );
    }

    #[test]
    fn single_changeset_header_shows_the_pan_offset_indicator_once_panned() {
        let mut app = app_with_a_long_line();
        app.hscroll_right();

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        assert!(
            header.contains(&format!("»{}", app.hscroll)),
            "expected the pan offset indicator on the lone-changeset header, got: {header:?}"
        );
    }

    // ── M5 CS3: outline side pane ───────────────────────────────────────────────

    /// Every outline test renders at this width so the pane's fixed 35-col + 1-col-divider
    /// layout is unambiguous: columns `0..35` are the outline, `35` the divider, `36..` the
    /// diff.
    const OUTLINE_TEST_WIDTH: u16 = 80;

    fn outline_row(buf: &Buffer, y: u16) -> String {
        (0..35).map(|x| cell_text(buf, x, y)).collect()
    }

    #[test]
    fn outline_pane_renders_headers_when_open_and_disappears_when_closed() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(
            app.outline_open(),
            "a two-changeset stack must default-open the outline"
        );

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        assert!(
            content.iter().any(|row| row.contains("Add a")),
            "expected the outline's Stack-mode header row for cs-a (rendered by its title), \
             got:\n{}",
            content.join("\n")
        );

        // Default state is open+unfocused, so a single `o` closes it (see
        // `App::toggle_outline`'s cycle).
        app.toggle_outline();
        assert!(!app.outline_open());

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        assert!(
            !content.iter().any(|row| row.contains("Add a")),
            "closing the outline must stop rendering its rows, got:\n{}",
            content.join("\n")
        );
    }

    #[test]
    fn outline_absent_for_a_lone_changeset() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        assert!(!app.outline_open());

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // The diff's own content (the M4 full-width look) must reach all the way to the left
        // edge — column 0 — rather than starting past a 36-column outline+divider offset.
        let row1: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 1)).collect();
        assert!(
            row1.contains("a.txt") || row1.trim().is_empty() || row1.contains("one"),
            "sanity: body row must be diff content, not outline chrome, got: {row1:?}"
        );
        assert_ne!(
            cell_text(&buf, 35, 1),
            "│",
            "a closed outline must not draw its divider column"
        );
    }

    #[test]
    fn outline_header_current_marker_uses_the_current_color() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture); // cs-b is `current`
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);

        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row = content
            .iter()
            .position(|r| r.contains('\u{2022}'))
            .expect("current marker present in the outline");
        let marker_x = content[row].find('\u{2022}').unwrap() as u16;
        assert_eq!(
            buf.cell((marker_x, row as u16)).unwrap().style().fg,
            Some(Palette::dark().current_fg),
            "expected the outline's current marker to carry Palette::dark().current_fg"
        );
    }

    #[test]
    fn outline_header_restack_marker_carries_the_warning_color() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture); // cs-b needs_restack: true
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);

        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row = content
            .iter()
            .position(|r| r.contains('\u{26A0}'))
            .expect("restack marker present in the outline");
        let marker_x = content[row].find('\u{26A0}').unwrap() as u16;
        assert_eq!(
            buf.cell((marker_x, row as u16)).unwrap().style().fg,
            Some(Palette::dark().warn_fg),
            "expected the outline's restack glyph to carry Palette::dark().warn_fg"
        );
    }

    #[test]
    fn outline_header_shows_true_position_counter_regardless_of_display_order() {
        // CS1 (`outline-header-polish`): the `[i/n]` counter is the TRUE stack position
        // (`cs_idx + 1`), never a display-order index — HeadFirst (the default) paints cs-b
        // (true index 1) before cs-a (true index 0), so the counter must read `[2/2]` on cs-b's
        // row and `[1/2]` on cs-a's, in that display order, not `[1/2]` then `[2/2]`.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);

        // Skip y=0: the full-width winbar also renders a `[i/n] <title-or-name>` fragment for the
        // CURRENT changeset (cs-b) — an unskipped search for "[2/2]" would false-positive on it.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row_b = content
            .iter()
            .position(|r| r.contains("[2/2]"))
            .expect("cs-b (true index 1) must show counter [2/2]");
        let row_a = content
            .iter()
            .position(|r| r.contains("[1/2]"))
            .expect("cs-a (true index 0) must show counter [1/2]");
        assert!(
            row_b < row_a,
            "HeadFirst shows cs-b's header before cs-a's, but the counter stays the true stack \
             position, not a display-order index — got:\n{}",
            content.join("\n")
        );
    }

    #[test]
    fn outline_header_label_carries_the_heading_accent_color() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);

        // Skip y=0: the full-width winbar ALSO names cs-b (it's `current`) via its own
        // `[i/n] <title-or-name>` fragment — in plain foreground, not the outline's heading
        // accent — so an unskipped search for "cs-b" would false-positive onto the winbar's own
        // label instead of the outline header row this test means to inspect. `content`'s index
        // `i` is buffer row `i + 1` (the skip), so every `buf` query below adds 1 back.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row = content
            .iter()
            .position(|r| r.contains("cs-b"))
            .expect("cs-b's header row present (it has no title, so falls back to its name)");
        let label_x = find_label_x(&content[row], "cs-b");
        assert_eq!(
            buf.cell((label_x, row as u16 + 1)).unwrap().style().fg,
            Some(Palette::dark().heading_fg),
            "expected the outline header's label to carry Palette::dark().heading_fg"
        );
    }

    #[test]
    fn outline_header_truncates_to_the_pane_width() {
        // CS1: `render_outline_header` writes via `Buffer::set_line(.., area.width)`, exactly
        // like every outline item row below it — a long changeset label must not bleed past the
        // outline's own width into the divider column (x=35 at `OUTLINE_TEST_WIDTH`).
        use crate::app::ChangesetView;
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let mid = fixture
            .commit("main")
            .file("a.txt", "a\n")
            .create("mid")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("b.txt", "b\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs_a = Changeset {
            name: "cs-a".to_string(),
            span: ChangesetSpan::Committed {
                base: root,
                head: mid,
            },
            title: None,
            current: false,
            needs_restack: false,
        };
        let cs_b = Changeset {
            name: "x".repeat(100),
            span: ChangesetSpan::Committed { base: mid, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view_a = ChangesetView::from_changeset_diff(
            cs_a.clone(),
            crate::acquire::diff_changeset(repo, &cs_a).unwrap(),
        );
        let view_b = ChangesetView::from_changeset_diff(
            cs_b.clone(),
            crate::acquire::diff_changeset(repo, &cs_b).unwrap(),
        );

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        app.open_current();
        assert!(app.outline_open());

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        assert_ne!(
            cell_text(&buf, 34, 0),
            " ",
            "expected the truncated label to reach all the way to the outline's last column"
        );
        assert_eq!(
            cell_text(&buf, 35, 0),
            "│",
            "the outline header must truncate to the pane's own width, not bleed into the \
             divider column"
        );
    }

    #[test]
    fn outline_items_still_start_at_y_1_below_the_outline_headers_own_row() {
        // CS1 invariant: carving out row 0 for the outline's own header must not shift outline
        // ITEM rows at all — they already started at y=1 pre-CS1 (below the OLD global header),
        // and they still do now (below the outline's OWN header instead). The pane header itself
        // never shows the current-changeset marker (only an outline ITEM row does — see
        // `render_outline_header`'s doc comment), which makes the marker a clean signal for where
        // items actually start.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(app.outline_open());

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let row0 = outline_row(&buf, 0);
        assert!(
            !row0.contains('\u{2022}'),
            "the outline's OWN header never shows the current-changeset marker, got: {row0:?}"
        );
        let row1 = outline_row(&buf, 1);
        assert!(
            row1.contains('\u{2022}'),
            "the first outline ITEM row (cs-b's Header row, which IS current) must start at \
             y=1, got: {row1:?}"
        );
    }

    #[test]
    fn summary_panel_title_has_no_counter_and_keeps_the_plain_foreground_look() {
        // CS1's Gotcha: the counter + accent are outline-only — the summary panel's title (shared
        // via `changeset_title_spans`, `counter: None`) must render exactly as it did pre-CS1.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline();
        app.toggle_outline();
        let header_idx = app
            .outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::Header { cs_idx: 1, .. }))
            .expect("cs-b's header row present in Stack mode") as i64;
        let delta = header_idx - app.outline_cursor() as i64;
        app.outline_move_by(delta);
        app.focus_outline();

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // CS1: the summary panel's title now paints the diff pane's OWN header row (y=0, x
        // 36..) instead of the body's first line — include y=0 in the scan (no skip needed).
        // The OUTLINE pane's header (x <35) also shows a `[i/n]` counter for the same active
        // changeset, so this scan stays scoped to the diff-header/body slice (x 36..) to avoid
        // that false-positive, same as the outline tests above.
        let body_rows: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (36..buf.area.width)
                    .map(|x| cell_text(&buf, x, y))
                    .collect::<String>()
            })
            .collect();
        let joined = body_rows.join("\n");
        assert!(
            !joined.contains("[2/2]") && !joined.contains("[1/2]"),
            "the outline-only counter must not leak into the summary panel's title, got:\n{joined}"
        );
        let row = body_rows
            .iter()
            .position(|r| r.contains("cs-b"))
            .expect("summary panel's title (cs-b's label) present");
        assert_eq!(
            row, 0,
            "the summary panel's title now paints the diff pane's header row (y=0), got row \
             {row} instead:\n{joined}"
        );
        // `find_label_x` returns a column offset within the 36.. slice it's given (every cell here
        // is one column wide, so a `chars()` position IS the display column) — add the slice's own
        // start column (36) back to get the absolute buffer column.
        let label_x = find_label_x(&body_rows[row], "cs-b") + 36;
        assert_eq!(
            buf.cell((label_x, row as u16)).unwrap().style().fg,
            Some(Palette::dark().foreground),
            "the summary panel's title must keep its plain foreground look, not the outline's \
             heading accent"
        );
    }

    #[test]
    fn render_preserves_a_wheel_scrolled_outline_viewport() {
        // The peek model's load-bearing render change: `render_outline` bounds-CLAMPS the
        // outline scroll instead of re-deriving it from the cursor, so a wheel-scrolled
        // viewport (cursor left outside it) survives the frame instead of snapping back.
        use crate::app::Region;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(app.outline_open());
        // A 5-row frame leaves a 3-row outline viewport over this fixture's 4 outline rows
        // (2 headers + 2 files): max scroll = 1.
        app.outline_height = 3;
        app.hit_regions.outline = Some(Region {
            x: 0,
            y: 1,
            w: 34,
            h: 3,
        });
        let cursor_before = app.outline_cursor();

        app.handle_wheel(2, 2, 3); // clamps to max scroll = 1
        assert_eq!(app.outline_scroll(), 1, "the wheel scrolled the viewport");
        assert_eq!(
            app.outline_cursor(),
            cursor_before,
            "peek model: the wheel never moves the outline cursor"
        );

        render_once(&mut app, OUTLINE_TEST_WIDTH, 5);
        assert_eq!(
            app.outline_scroll(),
            1,
            "a frame must not re-derive the wheeled scroll back to the cursor"
        );
    }

    #[test]
    fn outline_cursor_row_carries_cursor_background_when_focused() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        // Default is open+unfocused; two toggles: close, then reopen (which focuses).
        app.toggle_outline();
        app.toggle_outline();
        assert!(app.outline_open() && app.outline_focused());

        let cursor_y = 1 + app.outline_cursor() as u16;
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        assert_eq!(
            buf.cell((2, cursor_y)).unwrap().style().bg,
            Some(Palette::dark().cursor_bg),
            "expected the outline's cursor row to carry the cursor tint while focused"
        );
    }

    #[test]
    fn outline_flat_mode_dedupes_paths_across_the_stack() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.outline_cycle_mode(); // Stack -> StackTree
        app.outline_cycle_mode(); // StackTree -> Flat
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Flat);

        let items = app.outline_items();
        let paths: Vec<&str> = items
            .iter()
            .map(|it| match it {
                crate::outline::OutlineItem::File { path, .. } => path.as_str(),
                crate::outline::OutlineItem::Header { .. }
                | crate::outline::OutlineItem::Dir { .. } => {
                    panic!("Flat mode must not emit header or dir rows")
                }
            })
            .collect();
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            paths.len(),
            sorted.len(),
            "every path must appear exactly once in Flat mode, got: {paths:?}"
        );
    }

    /// A single committed changeset touching a nested path (`src/a.txt`) and a top-level path
    /// (`top.txt`), for the Tree-mode render test — the outline test fixtures above are
    /// deliberately flat and never produce a directory row.
    fn changeset_with_nested_paths(fixture: &Fixture) -> App {
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};

        use crate::app::ChangesetView;

        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("top.txt", "t\n")
            .file("src/a.txt", "a\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();

        let cs = Changeset {
            name: "cs".to_string(),
            span: ChangesetSpan::Committed { base: root, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );

        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();
        app
    }

    #[test]
    fn outline_tree_mode_renders_directory_rows_with_tree_guides() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture);
        // A lone changeset defaults the outline closed (locked design) — force it open so this
        // render test can inspect its rows.
        if !app.outline_open() {
            app.toggle_outline();
        }
        app.outline_cycle_mode(); // Stack -> StackTree
        app.outline_cycle_mode(); // StackTree -> Flat
        app.outline_cycle_mode(); // Flat -> Tree
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();

        // Row order per the CS3 dirs-before-files/alpha-within-group rule, one outline row per
        // buffer row starting at y=1 (y=0 is the winbar): `src/` (dir, root, NOT the root's last
        // child — `top.txt` follows), `a.txt` nested one level under `src/` (the only — hence
        // last — child of `src/`), then `top.txt` (file, root, IS the root's last child).
        //
        // CS2 tightens `tree_prefix` to 2 cols/level with no trailing space on the connector, so
        // these are exact-column checks (not just `contains`) — every rendered cell here is one
        // column wide, so `chars()` (not byte) indexing IS the display column (the guide glyphs
        // themselves are multi-byte, which is exactly why byte indexing would be wrong).
        let row1: Vec<char> = content[1].chars().collect();
        assert_eq!(
            row1[0..6],
            ['\u{251C}', '\u{2500}', 's', 'r', 'c', '/'],
            "expected row 1 to be a tight '├─src/' (2-col connector, no trailing space), got:\n{}",
            content.join("\n")
        );
        let row2: Vec<char> = content[2].chars().collect();
        assert_eq!(
            row2[0..4],
            ['\u{2502}', ' ', '\u{2570}', '\u{2500}'],
            "expected row 2's guide to be a tight '│ ╰─' (continuation + last-child connector, \
             both 2 cols), got:\n{}",
            content.join("\n")
        );
        assert_eq!(
            row2[7..12],
            ['a', '.', 't', 'x', 't'],
            "expected src/a.txt's basename to start immediately after the 4-col guide + 1-col \
             glyph + 1-col letter + 1-col space, got:\n{}",
            content.join("\n")
        );
        let row3: Vec<char> = content[3].chars().collect();
        assert_eq!(
            row3[0..2],
            ['\u{2570}', '\u{2500}'],
            "expected row 3's guide to be a tight '╰─' (root-level last-child, 2 cols, no \
             trailing space), got:\n{}",
            content.join("\n")
        );
        assert_eq!(
            row3[5..12],
            ['t', 'o', 'p', '.', 't', 'x', 't'],
            "expected top.txt to start immediately after the 2-col guide + 1-col glyph + 1-col \
             letter + 1-col space, got:\n{}",
            content.join("\n")
        );
    }

    #[test]
    fn outline_file_row_tree_guide_carries_the_dim_color() {
        // CS4: a File row's tree-guide connector (distinct from its status glyph, which keeps
        // `theme.foreground`) is styled `theme.dim`, matching the Dir row's already-dim guides.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture);
        if !app.outline_open() {
            app.toggle_outline();
        }
        app.outline_cycle_mode(); // Stack -> StackTree
        app.outline_cycle_mode(); // StackTree -> Flat
        app.outline_cycle_mode(); // Flat -> Tree
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // Row 3 is top.txt (see the test above) — a File row with a non-empty guide vector.
        let row = outline_row(&buf, 3);
        // `String::find` returns a BYTE offset, not a display column — the rounded guide glyph is
        // multi-byte, so a `chars()` (not byte) position is what actually lines up with the
        // column-indexed `buf.cell` lookup below (every rendered cell here is one column wide).
        let row_chars: Vec<char> = row.chars().collect();
        let guide_x = row_chars
            .iter()
            .position(|&c| c == '\u{2570}')
            .expect("rounded guide present") as u16;
        assert_eq!(
            buf.cell((guide_x, 3)).unwrap().style().fg,
            Some(Palette::dark().dim),
            "expected the File row's tree-guide connector to carry theme.dim, got: {row:?}"
        );
    }

    // ── CS5 (`outline-fold`): collapse/expand marker ────────────────────────────────

    #[test]
    fn outline_collapsed_header_renders_a_trailing_dim_hidden_file_marker() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.set_outline_order(crate::outline::OutlineOrder::BaseFirst);
        app.focus_outline();
        app.outline_top(); // cs-a's header row (BaseFirst: cs-a's header renders first)
        app.outline_confirm(); // toggle fold — collapses cs-a, hiding its single file (a.txt)

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // Skip y=0 (the winbar) — it names the current file too (e.g. `[i/n] path`), which can
        // false-positive a bare `contains` search, same gotcha `render_outline_file_row` already
        // documents.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let (row_idx, header_row) = content
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains("Add a"))
            .map(|(i, r)| (i, r.clone()))
            .expect("cs-a's header row present");
        let y = row_idx as u16 + 1; // +1 to undo the y=0 skip above.
        assert!(
            header_row.contains("\u{25b8} 1"),
            "collapsed header must show its 1 hidden file, got: {header_row:?}"
        );
        assert!(
            !content.iter().any(|r| r.contains("a.txt")),
            "a.txt's row must be hidden while its header is collapsed, got:\n{}",
            content.join("\n")
        );

        let row_chars: Vec<char> = header_row.chars().collect();
        let marker_x = row_chars
            .iter()
            .position(|&c| c == '\u{25b8}')
            .expect("marker glyph present") as u16;
        assert_eq!(
            buf.cell((marker_x, y)).unwrap().style().fg,
            Some(Palette::dark().dim),
            "expected the collapsed marker to carry theme.dim, got: {header_row:?}"
        );
    }

    #[test]
    fn outline_expanded_header_renders_no_chevron_marker() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // Skip y=0 (the winbar) — see the gotcha noted above.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        assert!(
            !content.iter().any(|r| r.contains('\u{25b8}')),
            "no row should carry the collapsed marker while every Header/Dir is expanded, got:\n{}",
            content.join("\n")
        );
    }

    #[test]
    fn outline_collapsed_dir_renders_a_trailing_dim_hidden_file_marker() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture);
        if !app.outline_open() {
            app.toggle_outline();
        }
        app.outline_cycle_mode(); // Stack -> StackTree
        app.outline_cycle_mode(); // StackTree -> Flat
        app.outline_cycle_mode(); // Flat -> Tree
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);

        app.focus_outline();
        app.outline_top(); // src/ (dirs-before-files root ordering — see the tree-guide test above)
        app.outline_confirm(); // toggle fold — collapses src/, hiding its one file (a.txt)

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // Skip y=0 (the winbar) — its OWN current-file label can itself contain `src/` (e.g.
        // `[1/1] src/a.txt`) and false-positive the `contains("src/")` search below if included.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let dir_row = content
            .iter()
            .find(|r| r.contains("src/"))
            .expect("src/ row present");
        assert!(
            dir_row.contains("\u{25b8} 1"),
            "collapsed src/ must show its 1 hidden file, got: {dir_row:?}"
        );
        assert!(
            !content.iter().any(|r| r.contains("a.txt")),
            "a.txt must be hidden under collapsed src/, got:\n{}",
            content.join("\n")
        );
        assert!(
            content.iter().any(|r| r.contains("top.txt")),
            "top.txt (a sibling, not nested under src/) must remain visible, got:\n{}",
            content.join("\n")
        );
    }

    // ── CS2 (outline-row-shape): smart path render ─────────────────────────────────

    #[test]
    fn outline_stack_mode_file_row_splits_basename_and_dim_dirname() {
        // Stack mode keeps `guides` empty, so a nested path (`src/a.txt`) must split at render
        // time into basename-first, then the dirname in `theme.dim` — ancestors don't carry the
        // path here (unlike Tree mode), so the row has to spell it out itself.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture);
        if !app.outline_open() {
            app.toggle_outline();
        }
        assert_eq!(
            app.outline_mode(),
            crate::outline::OutlineMode::Stack,
            "sanity: default mode is Stack, so guides stay empty and this exercises CS2's split"
        );

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // Skip y=0: the full-width winbar also names the current file (possibly `src/a.txt`
        // itself), so an unskipped search could false-positive onto it instead of the outline's
        // own row below it. `content`'s index `i` is buffer row `i + 1` (the skip), so every
        // `buf` query below adds 1 back.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row_idx = content
            .iter()
            .position(|r| r.contains("a.txt") && r.contains("src"))
            .expect("src/a.txt's split row present");
        let row_chars: Vec<char> = content[row_idx].chars().collect();
        let buf_y = row_idx as u16 + 1;

        let basename_x = row_chars
            .windows(5)
            .position(|w| w == ['a', '.', 't', 'x', 't'])
            .expect("basename 'a.txt' present in its own row") as u16;
        assert_eq!(
            buf.cell((basename_x, buf_y)).unwrap().style().fg,
            Some(Palette::dark().foreground),
            "expected the basename to carry the plain (bright) foreground, got:\n{}",
            content[row_idx]
        );

        // Two blank columns separate the basename from the dirname (CS2: "basename  dim/
        // dirname"), so the dirname starts right after them.
        let dirname_x = basename_x + 5 + 2;
        assert_eq!(
            row_chars[dirname_x as usize], 's',
            "expected the dirname 'src' to start two columns after the basename, got:\n{}",
            content[row_idx]
        );
        assert_eq!(
            buf.cell((dirname_x, buf_y)).unwrap().style().fg,
            Some(Palette::dark().dim),
            "expected the dirname to carry theme.dim, got:\n{}",
            content[row_idx]
        );
        assert!(
            basename_x < dirname_x,
            "basename must render BEFORE the dim dirname (basename-first ordering is what makes \
             truncation eat the dirname first), got:\n{}",
            content[row_idx]
        );
    }

    #[test]
    fn outline_root_level_file_gets_no_dirname_suffix() {
        // A root-level file (no `/` in its path) gets no suffix at all — no "(root)"
        // placeholder, just the bare basename.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture); // top.txt is root-level
        if !app.outline_open() {
            app.toggle_outline();
        }

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // Skip y=0: see the split test above — the winbar also names the current file.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row = content
            .iter()
            .find(|r| r.contains("top.txt"))
            .expect("top.txt's row present");
        assert!(
            row.trim_end().ends_with("top.txt"),
            "a root-level file must render with no trailing suffix after its basename, got: \
             {row:?}"
        );
    }

    #[test]
    fn outline_flat_row_truncation_eats_the_dim_dirname_first() {
        // A pane-width-exceeding Flat-mode row must truncate the (later, dim) dirname before it
        // ever touches the (earlier, bright) basename — that ordering is the whole point of
        // basename-first rendering (CS2 gotcha).
        let long_dir = "reallyquiteverbosedirectoryname";
        let path = format!("{long_dir}/x.txt");
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file(&path, "content\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        if !app.outline_open() {
            app.toggle_outline();
        }

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // Skip y=0: see the split test above — the winbar also names the current file (the long
        // path itself here), so an unskipped search would false-positive onto it.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row_idx = content
            .iter()
            .position(|r| r.contains("x.txt"))
            .expect("x.txt's row present");
        assert!(
            !content[row_idx].contains(long_dir),
            "the full dirname must NOT fit/appear — truncation should have eaten part of it, \
             got: {:?}",
            content[row_idx]
        );
        // Column 34 is the outline's last column before the divider at 35 (see
        // `OUTLINE_TEST_WIDTH`'s doc comment). `content`'s index is buffer row `+ 1` (the y=0
        // skip above).
        assert_eq!(
            cell_text(&buf, 34, row_idx as u16 + 1),
            "\u{2026}",
            "the truncated row must show the right-edge marker at the pane's last column"
        );
    }

    // ── mouse h-wheel + outline hscroll follow-up: outline panning ─────────────────

    /// A single-changeset `App` with one file whose path is far wider than the outline's fixed
    /// 35-column width, focused into the outline — for the outline hscroll rendering tests.
    fn app_with_a_long_outline_path() -> App {
        let long_path = format!("{}.txt", "a".repeat(80));
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file(&long_path, "content\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.focus_outline(); // opens (a lone changeset defaults closed) and focuses.
        app
    }

    #[test]
    fn outline_panning_shows_the_left_marker_shifted_text_and_the_right_edge_marker() {
        let mut app = app_with_a_long_outline_path();
        app.outline_hscroll_right();
        assert!(
            app.outline_hscroll() > 0,
            "the long path must give outline hscroll room to pan"
        );

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        // The header row's own label is short, but CS1's `[i/n] ` counter widens it enough that a
        // single hscroll step no longer pans it fully off — it can now ALSO show a lone marker
        // plus a stray `a` (from a branch name like `main`), so a bare "contains 'a'" check no
        // longer picks out the PATH row unambiguously. Look for a run of the synthetic path's
        // repeated `a`s instead (`app_with_a_long_outline_path`'s path is 80 `a`s + `.txt`) — no
        // header label plausibly contains four `a`s in a row.
        let row = content
            .iter()
            .position(|r| r.contains('…') && r.contains("aaaa"))
            .expect("the panned path row must show the left-edge marker plus shifted content");
        // Column 34 is the outline's last column before the divider at 35 (see
        // `OUTLINE_TEST_WIDTH`'s doc comment).
        assert_eq!(
            cell_text(&buf, 34, row as u16),
            "…",
            "a row wider than the outline pane must show the right-edge marker too"
        );
    }

    #[test]
    fn outline_render_side_clamp_caps_a_huge_pan_offset() {
        let mut app = app_with_a_long_outline_path();
        for _ in 0..1000 {
            app.outline_hscroll_right();
        }
        assert!(
            app.outline_hscroll() > 1000,
            "sanity: `outline_hscroll_right` itself has no upper clamp"
        );

        render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        assert!(
            app.outline_hscroll() < 1000,
            "render_outline must clamp the huge offset down to the content width, got {}",
            app.outline_hscroll()
        );
    }

    // ── CS3 (`outline-status-xy`): git-style X/Y status matrix ─────────────────────

    /// Render `fixture` (a lone uncommitted changeset with one file at `path`) and return the
    /// buffer row + its char cells for the file row matching `path`. Skips y=0 (the winbar also
    /// names the current file, which can false-positive a `contains(path)` search).
    fn render_outline_file_row(fixture: &Fixture, path: &str) -> (Buffer, u16, Vec<char>) {
        let mut app = app_from_fixture(fixture);
        // A lone changeset defaults the outline closed — force it open so this render test can
        // inspect its rows (same pattern as `outline_tree_mode_renders_directory_rows_with_tree_guides`).
        if !app.outline_open() {
            app.toggle_outline();
        }
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let (row_idx, row) = content
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains(path))
            .map(|(i, r)| (i, r.clone()))
            .unwrap_or_else(|| panic!("{path}'s file row present"));
        let y = row_idx as u16 + 1; // +1 to undo the y=0 skip above.
        (buf, y, row.chars().collect())
    }

    #[test]
    fn outline_unstaged_file_renders_the_y_column_letter_in_del_strong() {
        // Unstaged-only (worktree change, no staged one): X is the placeholder, Y carries the
        // change letter in del_strong (git convention: worktree column is red).
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.rs", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let (buf, y, row) = render_outline_file_row(&fixture, "a.rs");

        let x = row
            .iter()
            .position(|&c| c == STATUS_PLACEHOLDER)
            .expect("expected the X (staged) column placeholder '·'") as u16;
        assert_eq!(
            row[x as usize + 1],
            'M',
            "expected the Y (worktree) column to carry the Modified letter right after the \
             X placeholder, got: {:?}",
            row
        );
        assert_eq!(
            buf.cell((x, y)).unwrap().style().fg,
            Some(Palette::dark().dim),
            "expected the empty X placeholder to carry theme.dim"
        );
        assert_eq!(
            buf.cell((x + 1, y)).unwrap().style().fg,
            Some(Palette::dark().del_strong),
            "expected the Y column's Modified letter to carry theme.del_strong"
        );
    }

    #[test]
    fn outline_fully_staged_file_renders_the_x_column_letter_in_add_strong() {
        // `staged_file` writes+stages a brand-new path (Added, not Modified — there's no prior
        // commit for it to modify). Fully staged (index change, no worktree one): X carries the
        // letter in add_strong, Y is the placeholder.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("a.rs", "new content\n")
            .build()
            .unwrap();
        let (buf, y, row) = render_outline_file_row(&fixture, "a.rs");

        let x = row
            .iter()
            .position(|&c| c == 'A')
            .expect("expected the Added letter in the X (staged) column") as u16;
        assert_eq!(
            row[x as usize + 1],
            STATUS_PLACEHOLDER,
            "expected the Y (worktree) column to be the empty placeholder, got: {:?}",
            row
        );
        assert_eq!(
            buf.cell((x, y)).unwrap().style().fg,
            Some(Palette::dark().add_strong),
            "expected the X column's Added letter to carry theme.add_strong"
        );
        assert_eq!(
            buf.cell((x + 1, y)).unwrap().style().fg,
            Some(Palette::dark().dim),
            "expected the empty Y placeholder to carry theme.dim"
        );
    }

    #[test]
    fn outline_partially_staged_file_renders_mm_with_green_x_and_red_y() {
        // Partially staged (both a staged AND an unstaged change): both columns show the change
        // letter, X in add_strong (green), Y in del_strong (red).
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .partially_staged_file("a.rs", "one\n", "one\nSTAGED\n", "one\nSTAGED\nWORKTREE\n")
            .build()
            .unwrap();
        let (buf, y, row) = render_outline_file_row(&fixture, "a.rs");

        let x = row
            .iter()
            .position(|&c| c == 'M')
            .expect("expected the Modified letter in the X column") as u16;
        assert_eq!(
            row[x as usize + 1],
            'M',
            "expected the Modified letter in the Y column too (partial = both axes), got: {:?}",
            row
        );
        assert_eq!(
            buf.cell((x, y)).unwrap().style().fg,
            Some(Palette::dark().add_strong),
            "expected the X (staged) column's letter to carry theme.add_strong"
        );
        assert_eq!(
            buf.cell((x + 1, y)).unwrap().style().fg,
            Some(Palette::dark().del_strong),
            "expected the Y (worktree) column's letter to carry theme.del_strong"
        );
    }

    #[test]
    fn outline_untracked_file_renders_a_dim_double_question_mark() {
        // Untracked overrides the staged-ness-derived matrix entirely: always a dim `??`, even
        // though an untracked worktree file's StagedStatus is Unstaged under the hood.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("new.txt", "brand new\n")
            .build()
            .unwrap();
        let (buf, y, row) = render_outline_file_row(&fixture, "new.txt");

        let x = row
            .iter()
            .position(|&c| c == '?')
            .expect("expected the untracked '??' marker") as u16;
        assert_eq!(
            row[x as usize + 1],
            '?',
            "expected '??' (both columns), got: {:?}",
            row
        );
        assert_eq!(
            buf.cell((x, y)).unwrap().style().fg,
            Some(Palette::dark().dim),
            "expected the untracked '?' to carry theme.dim, not an add/del tint"
        );
        assert_eq!(
            buf.cell((x + 1, y)).unwrap().style().fg,
            Some(Palette::dark().dim),
            "expected BOTH untracked '?' chars to carry theme.dim"
        );
    }

    #[test]
    fn outline_committed_modified_file_renders_a_single_amber_letter() {
        // A committed changeset's file has StagedStatus::None — single letter + pad column, not
        // the X/Y matrix. M/R/C get the dedicated `modified_fg` amber, distinct from `warn_fg`.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let base = fixture
            .commit("main")
            .file("a.rs", "one\n")
            .create("base")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("a.rs", "one\nCHANGED\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();
        let cs = workon::Changeset {
            name: "cs".to_string(),
            span: workon::ChangesetSpan::Committed { base, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = crate::app::ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );
        let owned = git2::Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();
        if !app.outline_open() {
            app.toggle_outline();
        }

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row_idx = content
            .iter()
            .position(|r| r.contains("a.rs"))
            .expect("a.rs's file row present");
        let row: Vec<char> = content[row_idx].chars().collect();
        let y = row_idx as u16 + 1;

        let x = row
            .iter()
            .position(|&c| c == 'M')
            .expect("expected the Modified letter") as u16;
        assert_eq!(
            row[x as usize + 1],
            ' ',
            "expected the pad column after a committed file's single letter to be a blank space, \
             got: {:?}",
            row
        );
        assert_eq!(
            buf.cell((x, y)).unwrap().style().fg,
            Some(Palette::dark().modified_fg),
            "expected the committed Modified letter to carry theme.modified_fg (amber), got a \
             different color"
        );
        assert_ne!(
            buf.cell((x, y)).unwrap().style().fg,
            Some(Palette::dark().warn_fg),
            "modified_fg must stay a distinct field from warn_fg even though both default to amber"
        );
    }

    #[test]
    fn outline_committed_added_and_deleted_files_render_add_strong_and_del_strong() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let base = fixture
            .commit("main")
            .file("deleted.txt", "keep\n")
            .create("base")
            .unwrap();
        let stage = fixture
            .commit("main")
            .file("deleted.txt", "keep\n")
            .file("added.txt", "new\n")
            .create("stage")
            .unwrap();
        let _ = stage; // only needed to move the branch tip forward before the manual deletion below
        let repo = fixture.repo().unwrap();
        let workdir = repo.workdir().unwrap().to_path_buf();
        std::fs::remove_file(workdir.join("deleted.txt")).unwrap();
        let mut index = repo.index().unwrap();
        // `CommitBuilder::create` wrote the index/commit through its OWN `Repository::open`
        // handle, so `repo`'s cached index is stale until forced to re-read from disk.
        index.read(true).unwrap();
        index
            .remove_path(std::path::Path::new("deleted.txt"))
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Test User", "test@example.com").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let head = repo
            .commit(Some("HEAD"), &sig, &sig, "head", &tree, &[&parent])
            .unwrap();

        let cs = workon::Changeset {
            name: "cs".to_string(),
            span: workon::ChangesetSpan::Committed { base, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = crate::app::ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );
        let owned = git2::Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();
        if !app.outline_open() {
            app.toggle_outline();
        }

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();

        let added_row_idx = content
            .iter()
            .position(|r| r.contains("added.txt"))
            .expect("added.txt's file row present");
        let added_row: Vec<char> = content[added_row_idx].chars().collect();
        let added_x = added_row
            .iter()
            .position(|&c| c == 'A')
            .expect("expected the Added letter") as u16;
        assert_eq!(
            buf.cell((added_x, added_row_idx as u16 + 1))
                .unwrap()
                .style()
                .fg,
            Some(Palette::dark().add_strong),
            "expected a committed Added file's letter to carry theme.add_strong"
        );

        let deleted_row_idx = content
            .iter()
            .position(|r| r.contains("deleted.txt"))
            .expect("deleted.txt's file row present");
        let deleted_row: Vec<char> = content[deleted_row_idx].chars().collect();
        let deleted_x = deleted_row
            .iter()
            .position(|&c| c == 'D')
            .expect("expected the Deleted letter") as u16;
        assert_eq!(
            buf.cell((deleted_x, deleted_row_idx as u16 + 1))
                .unwrap()
                .style()
                .fg,
            Some(Palette::dark().del_strong),
            "expected a committed Deleted file's letter to carry theme.del_strong"
        );
    }

    #[test]
    fn icon_mode_nerd_renders_the_rust_file_icon_and_the_dir_icon() {
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};

        use crate::app::ChangesetView;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("src/main.rs", "fn main() {}\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();
        let cs = Changeset {
            name: "cs".to_string(),
            span: ChangesetSpan::Committed { base: root, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();
        if !app.outline_open() {
            app.toggle_outline();
        }
        app.outline_cycle_mode(); // Stack -> StackTree
        app.outline_cycle_mode(); // StackTree -> Flat
        app.outline_cycle_mode(); // Flat -> Tree, so `src/` renders as its own Dir row
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);
        app.set_icon_mode(crate::icons::IconMode::Nerd);

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();

        // Skip y=0 in both searches: the full-width winbar names the path ("[1/1] src/main.rs"),
        // so an unskipped search would match it instead of the outline's own rows below it.
        let dir_row = content
            .iter()
            .skip(1)
            .find(|r| r.contains("src/"))
            .expect("src/ dir row present");
        assert!(
            dir_row.contains(crate::icons::DIR_ICON),
            "expected the dir icon before src/, got: {dir_row:?}"
        );
        let file_row = content
            .iter()
            .skip(1)
            .find(|r| r.contains("main.rs"))
            .expect("main.rs file row present");
        assert!(
            file_row.contains(crate::icons::icon_for_path("main.rs", false).0),
            "expected the rust file icon before main.rs, got: {file_row:?}"
        );
    }

    #[test]
    fn icon_mode_nerd_collapses_the_file_icon_color_to_foreground_under_a_colorless_theme() {
        // The `no-color-mono` finding this guards: `icons::icon_for_path`'s hardcoded per-filetype
        // `Rgb` is palette-EXTERNAL, so it must be collapsed to `foreground` by the render.rs paint
        // site itself when `Palette::colorless` is set — `mono()`'s own fields (already `Reset`)
        // can't do this for it. Companion to the theme.rs-level `only_mono_sets_colorless` test.
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};

        use crate::app::ChangesetView;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let head = fixture
            .commit("main")
            .file("main.rs", "fn main() {}\n")
            .create("head")
            .unwrap();
        let repo = fixture.repo().unwrap();
        let cs = Changeset {
            name: "cs".to_string(),
            span: ChangesetSpan::Committed { base: root, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view = ChangesetView::from_changeset_diff(
            cs.clone(),
            crate::acquire::diff_changeset(repo, &cs).unwrap(),
        );
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();
        if !app.outline_open() {
            app.toggle_outline();
        }
        app.set_icon_mode(crate::icons::IconMode::Nerd);

        let mono = Palette::mono(false);
        let buf = render_once_themed(&mut app, OUTLINE_TEST_WIDTH, 20, &mono);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();

        let (row_idx, row) = content
            .iter()
            .enumerate()
            .skip(1) // y=0 is the winbar
            .find(|(_, r)| r.contains("main.rs"))
            .expect("main.rs file row present");
        let icon = crate::icons::icon_for_path("main.rs", false).0;
        let icon_x = row
            .chars()
            .position(|c| c == icon)
            .expect("icon glyph present in the file row") as u16;

        assert_eq!(
            buf.cell((icon_x, row_idx as u16)).unwrap().style().fg,
            Some(mono.foreground),
            "icon fg must collapse to `foreground` under a colorless theme, got row: {row:?}"
        );
    }

    #[test]
    fn icon_mode_none_renders_neither_icon() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture);
        if !app.outline_open() {
            app.toggle_outline();
        }
        app.outline_cycle_mode(); // Stack -> StackTree
        app.outline_cycle_mode(); // StackTree -> Flat
        app.outline_cycle_mode(); // Flat -> Tree
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);
        assert_eq!(
            app.icon_mode(),
            crate::icons::IconMode::None,
            "sanity: icons default to None"
        );

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();

        assert!(
            !content.iter().any(|r| r.contains(crate::icons::DIR_ICON)),
            "icons=none must never render the dir icon, got:\n{}",
            content.join("\n")
        );
        assert!(
            !content
                .iter()
                .any(|r| r.contains(crate::icons::DEFAULT_ICON)),
            "icons=none must never render the default file icon, got:\n{}",
            content.join("\n")
        );
    }

    // ── CS3: nerd-mode status/header/summary iconography ────────────────────────

    #[test]
    fn outline_header_nerd_markers_replace_the_unicode_defaults() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture); // cs-b: current + needs_restack
        app.set_icon_mode(crate::icons::IconMode::Nerd);

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        // Skip y=0: it's the full-width winbar, which ALSO renders a (still-unicode, CS4's job)
        // "⚠ needs restack" marker — an unskipped search would false-positive on it.
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let joined = content.join("\n");

        assert!(
            joined.contains(super::NERD_CURRENT_MARKER),
            "expected the nerd current-changeset marker, got:\n{joined}"
        );
        assert!(
            joined.contains(super::NERD_WARN_MARKER),
            "expected the nerd needs-restack marker, got:\n{joined}"
        );
        assert!(
            !joined.contains('\u{2022}') && !joined.contains('\u{26A0}'),
            "nerd mode must not leave the plain unicode markers behind in the outline pane, got:\n{joined}"
        );
        assert!(
            joined.contains(super::NERD_BRANCH_ICON),
            "expected a branch glyph on the changeset header row, got:\n{joined}"
        );
    }

    #[test]
    fn outline_file_status_xy_column_is_unaffected_by_icon_mode() {
        // CS3 retires StagedStatus's nerd/plain glyph split entirely — the X/Y status matrix is
        // now plain letters + `STATUS_PLACEHOLDER`, icon-mode-independent (only the devicons
        // per-file icon toggles on `IconMode::Nerd`). A fully staged file (`staged_file` writes a
        // brand-new path, so it's Added, not Modified) still renders `A·` whether or not nerd
        // icons are on.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("a.txt", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.set_icon_mode(crate::icons::IconMode::Nerd);
        if !app.outline_open() {
            app.toggle_outline();
        }

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (1..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        let row = content
            .iter()
            .find(|r| r.contains("a.txt"))
            .expect("a.txt's file row present");
        assert!(
            row.contains(&format!("A{}", '\u{b7}')),
            "expected the fully-staged 'A·' status pair to survive nerd icon mode, got: {row:?}"
        );
    }

    #[test]
    fn summary_panel_nerd_mode_renders_the_dir_icon_and_diffstat_glyphs() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture);
        app.set_icon_mode(crate::icons::IconMode::Nerd);
        app.focus_outline(); // opens (a lone changeset defaults closed) and focuses
        app.outline_cycle_mode(); // Stack -> StackTree
        app.outline_cycle_mode(); // StackTree -> Flat
        app.outline_cycle_mode(); // Flat -> Tree, so a Dir row exists to focus
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);
        let dir_idx = app
            .outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { .. }))
            .expect("a Dir row present in Tree mode") as i64;
        let delta = dir_idx - app.outline_cursor() as i64;
        app.outline_move_by(delta);
        assert!(matches!(
            app.outline_items()[app.outline_cursor()],
            OutlineItem::Dir { .. }
        ));

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let body = body_text(&buf);
        assert!(
            body.contains(crate::icons::DIR_ICON),
            "expected the summary panel's dir title to carry the nerd dir icon, got:\n{body}"
        );
        assert!(
            body.contains(super::NERD_DIFF_ADDED) && body.contains(super::NERD_DIFF_REMOVED),
            "expected nerd diffstat glyphs in the summary panel's totals line, got:\n{body}"
        );
    }

    // ── CS4: summary panel ───────────────────────────────────────────────────────

    /// The body area's columns, for a render at [`OUTLINE_TEST_WIDTH`] (outline `0..35`, divider
    /// `35`, body `36..`) — mirrors [`outline_row`]'s slice but for the OTHER side of the pane.
    fn body_text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (36..buf.area.width)
                    .map(|x| cell_text(buf, x, y))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn summary_header_shows_dir_title_and_body_drops_duplicate() {
        // CS1: `dir_summary_lines` now returns `(title, body)` — the title paints the diff
        // pane's header row (y=0), and the body (per-file rows + totals) no longer repeats it as
        // its own first line.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture);
        app.focus_outline(); // opens (a lone changeset defaults closed) and focuses
        app.outline_cycle_mode(); // Stack -> StackTree
        app.outline_cycle_mode(); // StackTree -> Flat
        app.outline_cycle_mode(); // Flat -> Tree, so a Dir row exists to focus
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);
        let dir_idx = app
            .outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::Dir { .. }))
            .expect("a Dir row present in Tree mode") as i64;
        let delta = dir_idx - app.outline_cursor() as i64;
        app.outline_move_by(delta);
        assert!(matches!(
            app.outline_items()[app.outline_cursor()],
            OutlineItem::Dir { .. }
        ));

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let header: String = (36..buf.area.width)
            .map(|x| cell_text(&buf, x, 0))
            .collect();
        assert!(
            header.trim_end().ends_with("src/"),
            "expected the diff pane's header row to carry the dir summary's title, got: {header:?}"
        );

        // The exact title text ("src/", nothing else) must not reappear as a whole body line —
        // a per-file row like "src/a.txt  +1 -0" legitimately CONTAINS "src/" as a substring, so
        // this checks for an exact-line match, not a substring.
        for y in 1..buf.area.height {
            let row: String = (36..buf.area.width)
                .map(|x| cell_text(&buf, x, y))
                .collect();
            assert_ne!(
                row.trim_end(),
                "src/",
                "the summary panel's body must not duplicate the title as its own line, \
                 got row {y}: {row:?}"
            );
        }
    }

    #[test]
    fn focused_header_selection_renders_the_summary_panel_instead_of_the_diff() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(app.outline_open(), "a two-changeset stack default-opens");
        // Default is open+unfocused; two toggles (close, reopen) focuses it — same idiom
        // `outline_cursor_row_carries_cursor_background_when_focused` uses. Construction's
        // `sync_outline_to_current` already parked the cursor on cs-b's (the `current`
        // changeset's) own File row, not a Header — move it onto cs-b's Header explicitly.
        app.toggle_outline();
        app.toggle_outline();
        assert!(app.outline_open() && app.outline_focused());
        let header_idx = app
            .outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::Header { cs_idx: 1, .. }))
            .expect("cs-b's header row present in Stack mode") as i64;
        // A Header row never jumps the diff on `outline_move_by` (only a File row does — see its
        // doc comment), so a single relative move onto it is side-effect-free.
        let delta = header_idx - app.outline_cursor() as i64;
        app.outline_move_by(delta);
        assert!(matches!(
            app.outline_items()[app.outline_cursor()],
            OutlineItem::Header { cs_idx: 1, .. }
        ));
        app.focus_outline(); // outline_move_by doesn't touch focus; ensure it's still focused

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let body = body_text(&buf);
        assert!(
            body.contains("cs-b"),
            "expected the summary panel's title (cs-b's label — it has no title, so falls back \
             to its name), got:\n{body}"
        );
        assert!(
            body.contains("+1") && body.contains("-0"),
            "expected a '+N -M' diffstat fragment for cs-b's single added file, got:\n{body}"
        );
        assert!(
            body.contains("1 files"),
            "expected the summary panel's totals line, got:\n{body}"
        );
    }

    #[test]
    fn unfocused_open_outline_on_a_header_row_still_renders_the_normal_diff_body() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        // Default state: open, UNFOCUSED — must NOT show the summary panel (locked design: only
        // a FOCUSED outline overrides the diff area) even with the cursor moved onto a Header
        // row (construction's `sync_outline_to_current` parks it on cs-b's File row by default).
        assert!(app.outline_open() && !app.outline_focused());
        let header_idx = app
            .outline_items()
            .iter()
            .position(|it| matches!(it, OutlineItem::Header { cs_idx: 1, .. }))
            .expect("cs-b's header row present in Stack mode") as i64;
        let delta = header_idx - app.outline_cursor() as i64;
        app.outline_move_by(delta);
        assert!(matches!(
            app.outline_items()[app.outline_cursor()],
            OutlineItem::Header { cs_idx: 1, .. }
        ));
        assert!(
            !app.outline_focused(),
            "outline_move_by must not itself grant focus"
        );

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let body = body_text(&buf);
        assert!(
            !body.contains("1 files"),
            "an unfocused open outline must never override the diff body with the summary \
             panel's totals line, got:\n{body}"
        );
    }

    // ── theming fix: canvas paint ────────────────────────────────────────────────

    #[test]
    fn light_theme_paints_the_canvas_with_the_light_background() {
        // The bug this fix addresses: `workon.review.theme light` still showed the terminal's own
        // (usually dark) bg/fg because the canvas was never painted. A body cell untouched by any
        // diff/cursor/selection tint (e.g. a blank row past the end of a short file) must carry
        // the theme's OWN background, not `None`/the terminal default.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        let theme = Palette::light();
        let buf = render_once_themed(&mut app, 40, 10, &theme);

        // Row 1 (below the header, no files loaded — "(no changes)" placeholder) is plain canvas:
        // no tint should have painted over it.
        let canvas_cell = buf.cell((30, 5)).unwrap();
        assert_eq!(
            canvas_cell.style().bg,
            Some(theme.background),
            "expected an untinted body cell to carry the light theme's painted canvas background"
        );
    }

    #[test]
    fn header_text_carries_the_theme_foreground_not_the_terminal_default() {
        // Regression (stack-review): render_header/render_winbar drew BOLD text with no `.fg()`,
        // so on a curated theme whose polarity differs from the terminal the top bar rendered in
        // the terminal's default fg over the painted canvas — invisible (light-on-light for
        // `theme=light` in a dark terminal). The header must carry the theme's own foreground.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        let theme = Palette::light();
        let buf = render_once_themed(&mut app, 40, 10, &theme);

        // Cell (0,0) is the header's leading '[' — a real glyph in the top status bar.
        let header_cell = buf.cell((0, 0)).unwrap();
        assert_eq!(
            header_cell.style().fg,
            Some(theme.foreground),
            "header text must use the theme foreground to stay visible on the painted canvas"
        );
    }

    #[test]
    fn dark_theme_paints_the_canvas_with_the_dark_background() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        let theme = Palette::dark();
        let buf = render_once_themed(&mut app, 40, 10, &theme);

        let canvas_cell = buf.cell((30, 5)).unwrap();
        assert_eq!(
            canvas_cell.style().bg,
            Some(theme.background),
            "expected an untinted body cell to carry the dark theme's painted canvas background"
        );
    }

    #[test]
    fn cursor_row_tint_still_shows_over_a_painted_canvas() {
        // The canvas paint must not mask the per-row tint compositing (cursor/diff washes) —
        // a cursor row must still show the theme's cursor tint, not the flat canvas color.
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nold word here\nl10\nl11\nl12\nl13\nl14\n";
        let new = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nnew word here\nl10\nl11\nl12\nl13\nl14\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("small.txt", old, new)
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let theme = Palette::light();

        let cursor_row = app
            .current_view_ref()
            .unwrap()
            .display
            .iter()
            .position(|row| matches!(row, DisplayRow::Row(r) if r.old == Row::Line(10)))
            .expect("l10 row present in the display vector");
        app.cursor = cursor_row;

        let buf = render_once_themed(&mut app, 60, 20, &theme);
        let content = buf_lines(&buf);
        let cursor_y = content
            .iter()
            .position(|line| line.contains("l10 "))
            .expect("cursor row (l10) visible") as u16;

        let cursor_bg = buf.cell((1, cursor_y)).unwrap().style().bg;
        assert_eq!(
            cursor_bg,
            Some(theme.cursor_bg),
            "expected the cursor row to carry the theme's cursor tint over the painted canvas"
        );
        assert_ne!(
            cursor_bg,
            Some(theme.background),
            "the cursor tint must be visually distinct from the flat painted canvas"
        );
    }

    // ── focused-pane-header (CS1): exactly-one-lit-label invariant ────────────────

    /// A cell's `(fg, bold?)` pair — the two axes [`pane_header_label_style`] toggles, checked
    /// together everywhere below since neither alone proves the invariant (a themed fg match with
    /// no bold, or vice versa, would both be bugs).
    fn label_style_at(buf: &Buffer, x: u16, y: u16) -> (Option<Color>, bool) {
        let style = buf.cell((x, y)).unwrap().style();
        (style.fg, style.add_modifier.contains(Modifier::BOLD))
    }

    #[test]
    fn startup_state_lights_the_diff_header_not_the_outline_header() {
        // Gotcha: `App::from_changesets` defaults the outline open but UNFOCUSED, so at launch the
        // one lit label must be on the diff side, not the outline's — this is also the general
        // "diff focused, effective zoom Single" case, since a Committed changeset's file has no
        // unstaged/staged split (always `EffectiveZoom::Single(Role::Combined)`).
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(
            app.outline_open() && !app.outline_focused(),
            "locked startup default"
        );
        assert_eq!(
            app.effective_zoom_for(app.current),
            EffectiveZoom::Single(Role::Combined)
        );

        let theme = Palette::dark();
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content = buf_lines(&buf);

        // Outline header's own title (row 0) names the current changeset ("cs-b") — dim, no bold.
        let outline_x = find_label_x(&content[0], "cs-b");
        assert_eq!(
            label_style_at(&buf, outline_x, 0),
            (Some(theme.dim), false),
            "outline header must stay dim while the outline is unfocused"
        );

        // Diff header's own label (row 0, right of the divider) names the file ("b.txt") — lit.
        let diff_x = find_label_x(&content[0], "b.txt");
        assert_eq!(
            label_style_at(&buf, diff_x, 0),
            (Some(theme.pane_header_focused_fg), true),
            "diff header must be lit at startup, since focus starts on the diff side"
        );
    }

    #[test]
    fn outline_focused_lights_the_outline_header_and_dims_every_diff_side_label() {
        // Locked decision #5's "outline focused" case: even a Split-zoom file's diff header AND
        // both of its captions must stay dim — the outline header is the frame's one lit label.
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
        assert_eq!(
            app.effective_zoom_for(app.current),
            EffectiveZoom::Split,
            "a partially-staged file defaults to a Split render"
        );
        app.focus_outline();
        assert!(app.outline_open() && app.outline_focused());

        let theme = Palette::dark();
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 24);
        let content = buf_lines(&buf);

        // `app_from_fixture`'s lone changeset is the synthetic uncommitted layer, whose
        // `display_label` is always "Uncommitted changes" (see `crate::app::display_label`), not
        // the file's own name.
        let outline_x = find_label_x(&content[0], "Uncommitted changes");
        assert_eq!(
            label_style_at(&buf, outline_x, 0),
            (Some(theme.pane_header_focused_fg), true),
            "outline header must be lit while the outline has focus"
        );

        let unstaged_row = content
            .iter()
            .position(|line| line.contains("UNSTAGED"))
            .expect("unstaged caption present");
        let staged_row = content
            .iter()
            .position(|line| line.contains("STAGED") && !line.contains("UNSTAGED"))
            .expect("staged caption present");
        let unstaged_x = find_label_x(&content[unstaged_row], "UNSTAGED");
        let staged_x = find_label_x(&content[staged_row], "STAGED");
        assert_eq!(
            label_style_at(&buf, unstaged_x, unstaged_row as u16),
            (Some(theme.dim), false),
            "the unstaged caption must stay dim while the outline holds focus"
        );
        assert_eq!(
            label_style_at(&buf, staged_x, staged_row as u16),
            (Some(theme.dim), false),
            "the staged caption must stay dim while the outline holds focus"
        );
    }

    #[test]
    fn split_zoom_lights_only_the_focused_halfs_caption_and_dims_the_diff_header() {
        // Locked decision #5's "diff focused, effective zoom Split" case: the diff pane's OWN
        // header stays dim (there's no single file-wide label to light while two panes show), and
        // exactly the focused half's caption lights up — flipping `split_focus` flips which one.
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
        assert_eq!(app.effective_zoom_for(app.current), EffectiveZoom::Split);
        assert!(!app.outline_focused());
        assert_eq!(
            app.split_focus_role(),
            Role::Unstaged,
            "default split focus"
        );

        let theme = Palette::dark();

        // "STAGED" is a substring of "UNSTAGED", so a naive `contains` search for the STAGED
        // caption's row can false-positive onto the UNSTAGED caption's row (which also contains
        // the literal text "STAGED") — same asymmetry
        // `split_renders_both_role_captions_stacked_with_content_in_each_pane` guards against.
        // Searching for "UNSTAGED" needs no such exclusion, since "UNSTAGED" never appears inside
        // the STAGED-only row.
        let caption_row = |content: &[String], label: &str| -> usize {
            content
                .iter()
                .position(|line| {
                    line.contains(label) && (label != "STAGED" || !line.contains("UNSTAGED"))
                })
                .unwrap_or_else(|| panic!("{label} caption present"))
        };

        let check = |app: &mut App, lit_label: &str, dim_label: &str| {
            let buf = render_once(app, OUTLINE_TEST_WIDTH, 24);
            let content = buf_lines(&buf);
            let lit_row = caption_row(&content, lit_label);
            let dim_row = caption_row(&content, dim_label);
            let lit_x = find_label_x(&content[lit_row], lit_label);
            let dim_x = find_label_x(&content[dim_row], dim_label);
            assert_eq!(
                label_style_at(&buf, lit_x, lit_row as u16),
                (Some(theme.pane_header_focused_fg), true),
                "{lit_label} should be the lit label"
            );
            assert_eq!(
                label_style_at(&buf, dim_x, dim_row as u16),
                (Some(theme.dim), false),
                "{dim_label} should stay dim"
            );
            // The diff pane's own header (row 0) stays dim under Split, regardless of which half
            // has focus — there is no single-file label to light while two panes are showing.
            let file_x = find_label_x(&content[0], "f.txt");
            assert_eq!(
                label_style_at(&buf, file_x, 0),
                (Some(theme.dim), false),
                "the diff header must stay dim under a Split zoom"
            );
        };

        check(&mut app, "UNSTAGED", "STAGED");
        app.toggle_split_focus();
        assert_eq!(app.split_focus_role(), Role::Staged);
        check(&mut app, "STAGED", "UNSTAGED");
    }

    #[test]
    fn zoom_collapse_to_single_lights_the_diff_header_not_a_caption() {
        // Gotcha: a requested `Split` collapses to `EffectiveZoom::Single` for a file lacking one
        // of the two sub-diffs (here, unstaged-only) — no captions render at all, so the diff
        // header itself must be the lit label, exactly as the plain-Single case above.
        let old = "l1\nl2\nl3\n";
        let new = "l1\nCHANGED\nl3\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("only.txt", old, new)
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        assert_eq!(
            app.zoom,
            crate::app::Zoom::Split,
            "default requested zoom is Split"
        );
        assert_eq!(
            app.effective_zoom_for(app.current),
            EffectiveZoom::Single(Role::Unstaged),
            "collapsed down to a single pane — no staged sub-diff to pair it with"
        );

        let theme = Palette::dark();
        let buf = render_once(&mut app, 60, 20);
        let content = buf_lines(&buf);
        for line in &content {
            assert!(
                !line.contains("UNSTAGED") && !line.contains("STAGED"),
                "a collapsed Single zoom must not render split captions, got: {line:?}"
            );
        }
        let file_x = find_label_x(&content[0], "only.txt");
        assert_eq!(
            label_style_at(&buf, file_x, 0),
            (Some(theme.pane_header_focused_fg), true),
            "the diff header must be the lit label once Split has collapsed to Single"
        );
    }

    #[test]
    fn split_zoom_short_area_fallback_lights_the_diff_header_not_a_caption() {
        // Gotcha: `render_body_split`'s own short-area fallback (`area.height < 4`) renders only
        // the focused pane and returns before either caption is drawn — no split caption survives
        // to be the frame's lit label, so `render_body` must light the diff header instead. A
        // 5-row frame leaves a diff pane body area of height 3 after the header carve-out (frame
        // height 5 - footer 1 = body/diff area height 4, minus the diff header's own 1 row = 3),
        // which is under the `render_body_split` fallback's `< 4` threshold.
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
        assert_eq!(app.effective_zoom_for(app.current), EffectiveZoom::Split);
        assert!(!app.outline_focused());

        let theme = Palette::dark();
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 5);
        let content = buf_lines(&buf);

        for line in &content {
            assert!(
                !line.contains("UNSTAGED") && !line.contains("STAGED"),
                "the short-area fallback must not render split captions, got: {line:?}"
            );
        }
        let file_x = find_label_x(&content[0], "f.txt");
        assert_eq!(
            label_style_at(&buf, file_x, 0),
            (Some(theme.pane_header_focused_fg), true),
            "the diff header must be the lit label once the split fallback drops both captions"
        );
    }

    #[test]
    fn no_color_bold_is_the_only_focus_differentiator() {
        // Locked decision #3: under `Palette::mono`, `pane_header_focused_fg` and `dim` both
        // collapse to `Color::Reset` (see theme.rs's own
        // `mono_pane_header_focused_fg_collapses_with_dim_leaving_bold_the_only_differentiator`)
        // — this test proves `render.rs` itself still differentiates the focused label via BOLD
        // alone when actually painting a frame under that palette.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(!app.outline_focused());

        let theme = Palette::mono(false);
        let buf = render_once_themed(&mut app, OUTLINE_TEST_WIDTH, 20, &theme);
        let content = buf_lines(&buf);

        let outline_x = find_label_x(&content[0], "cs-b");
        let (outline_fg, outline_bold) = label_style_at(&buf, outline_x, 0);
        let diff_x = find_label_x(&content[0], "b.txt");
        let (diff_fg, diff_bold) = label_style_at(&buf, diff_x, 0);

        assert_eq!(outline_fg, Some(Color::Reset));
        assert_eq!(diff_fg, Some(Color::Reset));
        assert_eq!(
            outline_fg, diff_fg,
            "color alone carries no distinction under NO_COLOR"
        );
        assert!(
            !outline_bold,
            "the dim (unfocused) outline header must not be bold"
        );
        assert!(
            diff_bold,
            "the lit (focused) diff header must stay bold under NO_COLOR"
        );
    }

    // ── unfocused-cursor-wash (CS1): the uniform dim-when-unfocused cursor model ───

    #[test]
    fn diff_cursor_dims_when_outline_holds_focus_single_zoom() {
        // Locked decision #1: the diff body (single/combined zoom) paints its cursor row with the
        // dim unfocused wash, not full `cursor_bg`, whenever the outline (not the diff) holds
        // focus.
        let old = "l1\nl2\nl3\n";
        let new = "l1\nCHANGED\nl3\n";
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("only.txt", old, new)
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        assert_eq!(
            app.effective_zoom_for(app.current),
            EffectiveZoom::Single(Role::Unstaged)
        );
        app.focus_outline();
        assert!(app.outline_open() && app.outline_focused());

        let theme = Palette::dark();
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);

        // Row 0 of the diff pane's own rect is its header; content starts at row 1. The cursor
        // row lands at `1 + (cursor - scroll)`, neither of which `render_body`'s Single-zoom arm
        // mutates (it only reads them), so the values read back after rendering are exactly what
        // painted the frame.
        let cursor_y = (1 + app.cursor - app.scroll) as u16;
        let cell = buf.cell((37, cursor_y)).unwrap();
        assert_eq!(
            cell.style().bg,
            Some(theme.cursor_unfocused_bg),
            "the diff cursor must dim to the unfocused wash while the outline holds focus"
        );
        assert_ne!(
            cell.style().bg,
            Some(theme.cursor_bg),
            "the diff cursor must NOT show the full focused wash while the outline holds focus"
        );
    }

    #[test]
    fn both_split_halves_dim_and_the_divider_carries_the_dim_wash_when_outline_holds_focus() {
        // Locked decision #1's "outline-focused + split zoom" case: neither half holds focus, so
        // BOTH show the dim wash on their own remembered cursor row — and the gotcha this
        // changeset must fix, the divider cell re-tint on that row must follow the same wash
        // (previously hardcoded to full `cursor_bg`).
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
        assert_eq!(app.effective_zoom_for(app.current), EffectiveZoom::Split);
        assert_eq!(
            app.split_focus_role(),
            Role::Unstaged,
            "default split focus"
        );

        // Move the (currently focused) unstaged pane's cursor onto its changed row, before the
        // outline takes focus — the staged pane's `alt` cursor is untouched, so it stays at
        // `reset_panes`'s first-hunk reseat: the staged pane renders the base->staged diff, whose
        // only change is "beta" -> "BETAEDIT" (row 1), not row 0 ("alpha"). ("GAMMAEDIT" is the
        // UNSTAGED pane's own hunk — the index->workdir diff — and never appears in the staged
        // pane at all.)
        app.cursor = 1;
        app.derive_scroll();
        app.focus_outline();
        assert!(app.outline_open() && app.outline_focused());

        let theme = Palette::dark();
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 24);
        let content = buf_lines(&buf);

        let unstaged_caption_row = caption_row(&content, "UNSTAGED");
        let staged_caption_row = caption_row(&content, "STAGED");

        // "BETAEDIT" appears once in EACH pane at row index 1 — the unstaged pane's unchanged
        // CONTEXT line (its own hunk is gamma -> GAMMAEDIT, at row 2, which `app.cursor` is never
        // set to here) and the staged pane's actual hunk (its `alt.cursor`, from `reset_panes`'s
        // first-hunk reseat) — so each search is bounded to its own pane's row range to
        // disambiguate which "BETAEDIT" it's finding. Bounded starting at column 36 to skip the
        // outline to the left of the diff panes.
        let unstaged_cursor_y = find_row(
            &buf,
            36,
            unstaged_caption_row + 1,
            staged_caption_row,
            "BETAEDIT",
        );
        let staged_cursor_y = find_row(&buf, 36, staged_caption_row + 1, content.len(), "BETAEDIT");

        // Same left/divider geometry `render_pane_sbs` computes for a `diff_w`-wide pane at
        // `OUTLINE_TEST_WIDTH` (outline `0..35` + 1-col divider, diff pane `36..`).
        let diff_x0 = 36u16;
        let diff_w = OUTLINE_TEST_WIDTH - diff_x0;
        let left_w = diff_w.saturating_sub(1) / 2;
        let div_x = diff_x0 + left_w;

        let unstaged_cell = buf.cell((diff_x0 + 1, unstaged_cursor_y)).unwrap();
        assert_eq!(
            unstaged_cell.style().bg,
            Some(theme.cursor_unfocused_bg),
            "the unstaged half's cursor must dim while the outline holds focus"
        );
        let staged_cell = buf.cell((diff_x0 + 1, staged_cursor_y)).unwrap();
        assert_eq!(
            staged_cell.style().bg,
            Some(theme.cursor_unfocused_bg),
            "the staged half's cursor must dim while the outline holds focus"
        );

        let divider_cell = buf.cell((div_x, unstaged_cursor_y)).unwrap();
        assert_eq!(
            divider_cell.style().bg,
            Some(theme.cursor_unfocused_bg),
            "the divider cell on a dimmed cursor row must carry the same dim wash, not stay bright"
        );
    }

    #[test]
    fn unfocused_split_half_shows_the_remembered_dim_cursor_while_the_focused_half_is_full() {
        // Locked decision #1's diff-focused split case: the half that just LOST focus (`w`
        // toggled away from it) now shows its remembered cursor position in the dim wash, rather
        // than no cursor at all (the pre-changeset behavior — `pane_render_state` returned `None`
        // for the unfocused half).
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
        assert_eq!(app.effective_zoom_for(app.current), EffectiveZoom::Split);
        assert!(!app.outline_focused());
        assert_eq!(
            app.split_focus_role(),
            Role::Unstaged,
            "default split focus"
        );
        if app.outline_open() {
            app.toggle_outline(); // force closed — a clean full-width diff pane, no outline offset
        }

        // Land the (currently focused) unstaged pane's cursor on row 1 (a context line in the
        // unstaged/index->workdir diff — its own hunk, gamma -> GAMMAEDIT, is row 2), then flip
        // focus to the staged half — `toggle_split_focus` swaps `cursor`/`scroll` with `alt`, so
        // that position becomes the unstaged half's REMEMBERED `alt` cursor. The staged half's OWN
        // `alt` (untouched since `reset_panes`'s first-hunk reseat) becomes the newly-focused
        // `cursor`: the staged pane renders the base->staged diff, whose only hunk is
        // "beta" -> "BETAEDIT", also row 1 — coincidentally the same row index, different text.
        app.cursor = 1;
        app.derive_scroll();
        app.toggle_split_focus();
        assert_eq!(app.split_focus_role(), Role::Staged);

        let theme = Palette::dark();
        let buf = render_once(&mut app, 60, 20);
        let content = buf_lines(&buf);

        let unstaged_caption_row = caption_row(&content, "UNSTAGED");
        let staged_caption_row = caption_row(&content, "STAGED");

        // "BETAEDIT" appears once in EACH pane at row index 1 (see the comment above) — bounded
        // per pane to disambiguate which one a given search lands on. No outline offset here (the
        // outline was force-closed above), so the search starts at column 0.
        let unstaged_cursor_y = find_row(
            &buf,
            0,
            unstaged_caption_row + 1,
            staged_caption_row,
            "BETAEDIT",
        );
        let staged_cursor_y = find_row(&buf, 0, staged_caption_row + 1, content.len(), "BETAEDIT");

        let unstaged_cell = buf.cell((1, unstaged_cursor_y)).unwrap();
        assert_eq!(
            unstaged_cell.style().bg,
            Some(theme.cursor_unfocused_bg),
            "the just-unfocused half's remembered cursor must show the dim wash"
        );
        assert_ne!(unstaged_cell.style().bg, Some(theme.cursor_bg));

        let staged_cell = buf.cell((1, staged_cursor_y)).unwrap();
        assert_eq!(
            staged_cell.style().bg,
            Some(theme.cursor_bg),
            "the newly-focused half must show the full cursor wash"
        );
    }

    // ── header-chrome-follows-focus (CS1): counters join the label's lit/dim toggle ───

    #[test]
    fn outline_header_counter_follows_the_labels_focus_toggle() {
        // The outline header's `[i/n]` counter used to stay unconditionally bold+foreground —
        // it now lights/dims together with the label beside it (locked decision #1).
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(app.outline_open() && !app.outline_focused());

        let theme = Palette::dark();
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content = buf_lines(&buf);
        let counter_x = find_label_x(&content[0], "[2/2]");
        assert_eq!(
            label_style_at(&buf, counter_x, 0),
            (Some(theme.dim), false),
            "the outline header counter must dim alongside the label while unfocused"
        );

        app.focus_outline();
        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content = buf_lines(&buf);
        let counter_x = find_label_x(&content[0], "[2/2]");
        assert_eq!(
            label_style_at(&buf, counter_x, 0),
            (Some(theme.pane_header_focused_fg), true),
            "the outline header counter must light alongside the label while focused"
        );
    }

    #[test]
    fn diff_header_file_counter_follows_the_labels_focus_toggle() {
        // Same toggle as the outline header's counter above, for the diff header's own
        // `[fidx/nfiles]` counter ([`super::file_segment_spans`]).
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        assert!(app.outline_open() && !app.outline_focused());

        let theme = Palette::dark();
        let buf = render_once(&mut app, 80, 20);
        let content = buf_lines(&buf);
        let counter_x = find_label_x(&content[0], "[1/1]");
        assert_eq!(
            label_style_at(&buf, counter_x, 0),
            (Some(theme.pane_header_focused_fg), true),
            "the diff header counter must light alongside the label while the diff has focus"
        );

        app.focus_outline();
        let buf = render_once(&mut app, 80, 20);
        let content = buf_lines(&buf);
        let counter_x = find_label_x(&content[0], "[1/1]");
        assert_eq!(
            label_style_at(&buf, counter_x, 0),
            (Some(theme.dim), false),
            "the diff header counter must dim alongside the label once focus leaves the diff"
        );
    }

    #[test]
    fn outline_header_diffstat_colors_stay_semantic_across_focus_toggle() {
        // Locked decision #2: the `+N -M` diffstat span is semantic information, not identity
        // chrome — it must keep its own color (and bold) regardless of which pane has focus.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        let theme = Palette::dark();

        let unfocused_buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let unfocused_content = buf_lines(&unfocused_buf);
        let add_x = find_label_x(&unfocused_content[0], "+1");
        let unfocused_add = label_style_at(&unfocused_buf, add_x, 0);

        app.focus_outline();
        let focused_buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let focused_content = buf_lines(&focused_buf);
        let add_x = find_label_x(&focused_content[0], "+1");
        let focused_add = label_style_at(&focused_buf, add_x, 0);

        assert_eq!(
            unfocused_add, focused_add,
            "the outline header's diffstat span must not change with focus"
        );
        assert_eq!(
            unfocused_add,
            (Some(theme.add_strong), true),
            "the diffstat span keeps its own semantic color and bold regardless of focus"
        );
    }

    #[test]
    fn changeset_prefix_text_follows_focus_while_the_warn_glyph_stays_semantic() {
        // `changeset_prefix_spans` (the diff header's changeset-position prefix, shown only with
        // the outline closed) splits into a `[i/n] {title}` text span that now follows the same
        // `focused` flag `diff_header_line` passes to `file_segment_spans`, and a glyph-only warn
        // span that keeps `theme.warn_fg` regardless (locked decision #2) — exercised directly
        // rather than through a rendered frame to probe both flag values in isolation. (A real
        // frame CAN show the prefix dim: outline closed + Split zoom, where a caption is the lit
        // label and `diff_header_line` receives `focused == false`.)
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let app = two_committed_changesets_app(&fixture); // cs-b: current + needs_restack
        let theme = Palette::dark();
        let icons = crate::icons::IconMode::None;

        let lit = changeset_prefix_spans(&app, &theme, icons, true);
        let dim = changeset_prefix_spans(&app, &theme, icons, false);

        let text_style = |spans: &[TSpan<'static>]| {
            spans
                .iter()
                .find(|s| s.content.contains("[2/2]"))
                .expect("counter+title span present")
                .style
        };
        assert_eq!(
            text_style(&lit),
            pane_header_label_style(&theme, true),
            "the changeset-prefix text lights with focus"
        );
        assert_eq!(
            text_style(&dim),
            pane_header_label_style(&theme, false),
            "the changeset-prefix text dims without focus"
        );

        let warn_style = |spans: &[TSpan<'static>]| {
            spans
                .iter()
                .find(|s| s.content.contains('⚠'))
                .expect("warn glyph span present")
                .style
        };
        assert_eq!(
            warn_style(&lit).fg,
            Some(theme.warn_fg),
            "the warn glyph keeps its semantic color while the prefix text is focused"
        );
        assert_eq!(
            warn_style(&lit),
            warn_style(&dim),
            "the warn glyph's style is unaffected by the prefix text's focus"
        );
    }
}
