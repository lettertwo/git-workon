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
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::align::{CellKind, DisplayRow, InlineRow, Row};
use crate::app::{
    App, EffectiveZoom, FileView, Layout as AppLayout, Notice, Role, Severity, Summary,
};
use crate::attribute::Attribution;
use crate::config::View;
use crate::highlight::FgSpan;
use crate::icons::OutlineIcons;
use crate::keymap::{footer_hint, help_sections, Keymap};
use crate::model::FileStatus;
use crate::outline::OutlineItem;
use crate::summary::{ChangesetSummary, DirSummary, SummaryFileRow};
use crate::theme::Palette;
use crate::wordiff::Span as WordSpan;

// The on-tint colors (diff add/del gradient + staged variants, cursor/selection washes, and syntax
// foreground) come from a [`Palette`] threaded through render (ADR-035). The canvas background and
// default/dim/gutter chrome foreground ALSO now come from the palette (`theme.background`/
// `theme.foreground`/`theme.dim`/`theme.gutter`) — see the theme module's revised hybrid-boundary
// doc comment — so a curated theme fully controls the look. Only semantic chrome that is never a
// theme knob (error/warn/current-marker) stays ANSI-named / const below.

/// Footer text color for an [`Severity::Error`] [`Notice`] — a clearly-red tone that reads on
/// both light and dark terminal themes.
const FG_ERROR: Color = Color::Rgb(220, 60, 60);
/// Warning tone for the winbar's needs-restack marker (locked decision #9) — an amber, distinct
/// from [`FG_ERROR`]'s red: a stale-parent changeset is a heads-up to `gt restack`, not a failure.
const FG_WARN: Color = Color::Rgb(214, 158, 46);
/// Tone for the outline's "this is the lib-marked `current` changeset" marker (locked decision
/// #9's outline half) — a green, distinct from every other marker color in this module so
/// "current" reads unambiguously at a glance.
const FG_CURRENT: Color = Color::Rgb(96, 200, 128);

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

/// Wash the cursor row with the theme's cursor tint.
fn apply_cursor_row(line: Line<'static>, width: u16, theme: &Palette) -> Line<'static> {
    apply_row_tint(line, width, theme.cursor_bg)
}

/// Wash a selected (line-selection) row with the theme's selection tint.
fn apply_selection_row(line: Line<'static>, width: u16, theme: &Palette) -> Line<'static> {
    apply_row_tint(line, width, theme.selection_bg)
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
/// resolved HERE against `theme` (ADR-035's render-time resolution) — a segment with no covering
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
    theme: &Palette,
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
    mode: AttributionMode,
    gutter_w: usize,
    content_w: usize,
    theme: &Palette,
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
            ));
            Line::from(spans)
        }
    }
}

/// Render one frame: header, SBS body, footer, and (when [`App::help_visible`]) the `?` overlay
/// on top of everything else. `keymap` is the resolved, possibly-rebound keymap — the footer hint
/// and help overlay render its ACTUAL bindings (see [`crate::keymap::footer_hint`]/
/// [`crate::keymap::help_sections`]), never a hardcoded key string. `theme` is the resolved
/// on-tint palette — see [`crate::theme`]; the diff body, syntax foreground, and cursor/selection
/// washes all resolve their colors against it at paint time, as do the canvas background and the
/// default/dim/gutter chrome foreground (ADR-035, revised).
pub fn render(frame: &mut Frame, app: &mut App, keymap: &Keymap, theme: &Palette) {
    let area = frame.area();

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

    render_header(frame, app, header_area, theme);
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

/// Render the outline side pane's rows into `area`: [`OutlineItem::Header`]s (Stack mode only)
/// carry the changeset's position marker (green ● for `cs.current`) and needs-restack glyph
/// (amber ⚠, [`FG_WARN`] — locked decision #9's outline half); [`OutlineItem::File`]s carry an
/// indent, a one-character staged-ness glyph (blank for a committed changeset's files — see
/// [`crate::outline::StagedStatus`]'s doc comment for why no special-casing is needed here), and
/// the path. The cursor row (the outline's OWN cursor — a separate coordinate space from the
/// diff's [`App::cursor`]) gets the theme's cursor tint while the outline has focus, or the dimmer
/// [`Palette::outline_cursor_unfocused_bg`] while it's merely open (so the remembered position stays
/// legible even after focus returns to the diff). `&mut App` (CS2, precedent: [`render_body`]
/// writing [`App::pane_height`]) — writes [`App::outline_height`] and re-derives
/// [`App::derive_outline_scroll`] before painting from `app.outline.scroll`, giving the outline
/// the same stateful scrolloff-margined viewport the diff panes already have, instead of the old
/// transient bottom-anchor scroll computed fresh each frame.
fn render_outline(frame: &mut Frame, app: &mut App, area: Rect, theme: &Palette) {
    app.outline_height = area.height as usize;
    let items = app.outline_items();
    app.derive_outline_scroll(items.len());

    let cursor = app.outline_cursor();
    let focused = app.outline_focused();
    let scroll = app.outline_scroll();
    let icons = app.outline_icons();

    let buf = frame.buffer_mut();
    for row in 0..area.height {
        let item_idx = scroll + row as usize;
        let y = area.y + row;
        let Some(item) = items.get(item_idx) else {
            continue;
        };
        let is_cursor = item_idx == cursor;
        let line = build_outline_line(item, theme, icons);
        let line = if is_cursor && focused {
            apply_cursor_row(line, area.width, theme)
        } else if is_cursor {
            apply_row_tint(line, area.width, theme.outline_cursor_unfocused_bg)
        } else {
            line
        };
        buf.set_line(area.x, y, &line, area.width);
    }
}

/// Render a tree-guide prefix from an [`OutlineItem::Dir`]/[`OutlineItem::File`] `guides`
/// vector: every element but the last draws a continuing `│` (if that ancestor level was NOT
/// its parent's last child) or blank space (if it was), and the last element draws the row's own
/// `└─`/`├─` connector.
fn tree_prefix(guides: &[bool]) -> String {
    let mut s = String::new();
    let Some((&is_last, ancestors)) = guides.split_last() else {
        return s;
    };
    for &last in ancestors {
        s.push_str(if last { "   " } else { "\u{2502}  " });
    }
    s.push_str(if is_last {
        "\u{2514}\u{2500} "
    } else {
        "\u{251C}\u{2500} "
    });
    s
}

/// The [`FileStatus`] change-letter's foreground color (CS5): a create-like status (Added/
/// Untracked) reuses the theme's `add_strong` tint, a destroy-like status (Deleted) reuses
/// `del_strong`, and everything else (Modified/Renamed/Copied/Unmerged — a change to EXISTING
/// content, not a create/destroy) gets the theme's neutral `foreground`. No new [`Palette`]
/// fields — this is deliberately just a remap of tints CS4's summary rows already use.
fn change_letter_color(change: FileStatus, theme: &Palette) -> Color {
    match change {
        FileStatus::Added | FileStatus::Untracked => theme.add_strong,
        FileStatus::Deleted => theme.del_strong,
        FileStatus::Modified | FileStatus::Renamed | FileStatus::Copied | FileStatus::Unmerged => {
            theme.foreground
        }
    }
}

/// Build one outline row's rendered [`Line`] — see [`render_outline`]'s doc comment for the
/// marker rules. `icons` (CS5, `workon.review.outline.icons`) is [`OutlineIcons::None`] by
/// default, which reproduces the pre-CS5 row text exactly (no icon glyph, no extra space); only
/// [`OutlineIcons::Nerd`] inserts an icon before the name/path.
fn build_outline_line(item: &OutlineItem, theme: &Palette, icons: OutlineIcons) -> Line<'static> {
    match item {
        OutlineItem::Header {
            label,
            current,
            needs_restack,
            loading,
            failed,
            ..
        } => {
            let marker = if *current { "\u{25CF} " } else { "  " };
            let mut spans = vec![TSpan::styled(
                marker.to_string(),
                Style::default().fg(FG_CURRENT),
            )];
            spans.push(TSpan::styled(
                label.clone(),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ));
            if *needs_restack {
                spans.push(TSpan::styled(" \u{26A0}", Style::default().fg(FG_WARN)));
            }
            // ADR-037: a Failed changeset's marker wins over Pending's (a slot is never both,
            // but Failed is the more actionable state to surface if it somehow were).
            if *failed {
                spans.push(TSpan::styled(" \u{2717}", Style::default().fg(FG_ERROR)));
            } else if *loading {
                spans.push(TSpan::styled(" \u{2026}", Style::default().fg(theme.dim)));
            }
            Line::from(spans)
        }
        OutlineItem::Dir { name, guides, .. } => {
            let icon = match icons {
                OutlineIcons::Nerd => format!("{} ", crate::icons::DIR_ICON),
                OutlineIcons::None => String::new(),
            };
            let text = format!("{}{icon}{name}/", tree_prefix(guides));
            Line::from(TSpan::styled(
                text,
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
            ))
        }
        OutlineItem::File {
            path,
            status,
            change,
            guides,
            ..
        } => {
            let glyph = status.glyph();
            let letter = change.letter();
            // Empty `guides` (Flat/Stack modes) keeps the original two-space indent; a
            // non-empty `guides` (Tree/StackTree modes) draws tree connectors instead — see
            // `OutlineItem`'s doc comment for why emptiness is the mode signal.
            let prefix = if guides.is_empty() {
                "  ".to_string()
            } else {
                tree_prefix(guides)
            };
            let mut spans = vec![
                TSpan::styled(
                    format!("{prefix}{glyph}"),
                    Style::default().fg(theme.foreground),
                ),
                TSpan::styled(
                    letter.to_string(),
                    Style::default().fg(change_letter_color(*change, theme)),
                ),
                TSpan::styled(" ".to_string(), Style::default().fg(theme.foreground)),
            ];
            if icons == OutlineIcons::Nerd {
                let (icon, color) = crate::icons::icon_for_path(
                    path,
                    crate::theme::is_light_background(theme.background),
                );
                spans.push(TSpan::styled(
                    format!("{icon} "),
                    Style::default().fg(color.unwrap_or(theme.foreground)),
                ));
            }
            spans.push(TSpan::styled(
                path.clone(),
                Style::default().fg(theme.foreground),
            ));
            Line::from(spans)
        }
    }
}

/// The current file's label for the top status row: its path, or a rename's `old @ base ->
/// path` form — shared by the lone-changeset header and the multi-changeset winbar (they differ
/// only in what wraps this).
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

/// The top status row: `[fidx/nfiles] path` for a lone changeset (the M4 look, unchanged), or the
/// changeset-aware winbar (locked decision #8) once the stack has more than one changeset — the
/// winbar's own `[i/n]` is the CHANGESET counter, so showing both here would render two different
/// counters under the same bracket notation. Never both at once.
fn render_header(frame: &mut Frame, app: &App, area: Rect, theme: &Palette) {
    if app.changeset_count() > 1 {
        render_winbar(frame, app, area, theme);
        return;
    }
    let idx = app.current + 1;
    let n = app.files().len();
    let text = format!("[{idx}/{n}] {}", current_file_label(app));
    frame.render_widget(
        Paragraph::new(text).style(
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

/// The multi-changeset winbar (locked decisions #8 + #9): `[i/n] <title-or-name>
/// <restack-marker>  —  <path> (fidx/nfiles)`, where `i/n` is the changeset's position in the
/// stack and `fidx/nfiles` the active file's position within it. Only reached when
/// [`App::changeset_count`] > 1 (see [`render_header`]) — a lone uncommitted changeset never
/// shows this, keeping the M4 full-width look.
fn render_winbar(frame: &mut Frame, app: &App, area: Rect, theme: &Palette) {
    let cs = app.current_changeset();
    let i = app.current_cs() + 1;
    let n = app.changeset_count();
    let title = cs.title.as_deref().unwrap_or(cs.name.as_str());

    let mut spans = vec![TSpan::styled(
        format!("[{i}/{n}] {title}"),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    )];
    // A boolean-driven glyph + color (locked decision #9), not a title-string suffix — distinct
    // from the plain title so a stale-parent changeset reads as a heads-up at a glance.
    if cs.needs_restack {
        spans.push(TSpan::styled(
            "  ⚠ needs restack",
            Style::default().fg(FG_WARN).add_modifier(Modifier::BOLD),
        ));
    }
    let fidx = app.current + 1;
    let nfiles = app.files().len();
    spans.push(TSpan::styled(
        format!("  —  {} ({fidx}/{nfiles})", current_file_label(app)),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Footer priority: a pending discard confirm's prompt (warn-toned) wins over a transient notice,
/// which wins over the curated hint line (CS3) — a notice TEMPORARILY REPLACES the hint rather
/// than adding a second row; it clears on the user's next keypress (`tui::update`).
fn render_footer(frame: &mut Frame, app: &App, area: Rect, keymap: &Keymap, theme: &Palette) {
    if let Some(confirm) = &app.pending_confirm {
        frame.render_widget(
            Paragraph::new(confirm.prompt.as_str()).style(Style::default().fg(FG_ERROR)),
            area,
        );
        return;
    }
    match &app.notice {
        Some(Notice { text, severity }) => {
            let fg = match severity {
                Severity::Error => FG_ERROR,
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
            let text = footer_hint(keymap, focused);
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
fn render_gap_row(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    skipped: usize,
    is_cursor: bool,
    is_selected: bool,
    theme: &Palette,
) {
    let msg = format!("··· {skipped} unchanged lines (Enter to expand) ···");
    let line = Line::from(TSpan::styled(msg, Style::default().fg(theme.dim)));
    // Cursor wins over selection on the same row.
    let line = if is_cursor {
        apply_cursor_row(line, area.width, theme)
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
) {
    lines.push(Line::from(""));
    let footer_budget = 1; // the totals line always shows
    let file_budget = height.saturating_sub(lines.len() + footer_budget);
    push_summary_file_rows(lines, files, file_budget, theme);
    lines.push(Line::from(vec![
        TSpan::styled(
            format!("{} files", files.len()),
            Style::default().fg(theme.foreground),
        ),
        TSpan::raw("  "),
        TSpan::styled(
            format!("+{total_adds}"),
            Style::default().fg(theme.add_strong),
        ),
        TSpan::raw(" "),
        TSpan::styled(
            format!("-{total_dels}"),
            Style::default().fg(theme.del_strong),
        ),
    ]));
}

/// Build a [`ChangesetSummary`]'s lines: title line (carrying the same current/needs-restack/
/// failed markers `build_outline_line`'s Header arm draws), a loading/failed line OR the per-file
/// list + totals line.
fn changeset_summary_lines(
    summary: &ChangesetSummary,
    height: usize,
    theme: &Palette,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let mut title_spans = vec![TSpan::styled(
        if summary.current { "\u{25CF} " } else { "  " },
        Style::default().fg(FG_CURRENT),
    )];
    title_spans.push(TSpan::styled(
        summary.label.clone(),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    ));
    if summary.needs_restack {
        title_spans.push(TSpan::styled(" \u{26A0}", Style::default().fg(FG_WARN)));
    }
    lines.push(Line::from(title_spans));

    if summary.failed {
        let msg = summary
            .failure_message
            .as_deref()
            .unwrap_or("(no error message)");
        lines.push(Line::from(TSpan::styled(
            format!("\u{2717} {msg}"),
            Style::default().fg(FG_ERROR),
        )));
        return lines;
    }
    if summary.loading {
        lines.push(Line::from(TSpan::styled(
            "Loading\u{2026}",
            Style::default().fg(theme.dim),
        )));
        return lines;
    }

    push_summary_body(
        &mut lines,
        &summary.files,
        summary.total_adds,
        summary.total_dels,
        height,
        theme,
    );
    lines
}

/// Build a [`DirSummary`]'s lines: a bold path title, a blank line, the per-file list, and the
/// totals line — no current/restack/loading/failed markers (a directory carries none of those).
fn dir_summary_lines(summary: &DirSummary, height: usize, theme: &Palette) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(TSpan::styled(
        format!("{}/", summary.path),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    ))];
    push_summary_body(
        &mut lines,
        &summary.files,
        summary.total_adds,
        summary.total_dels,
        height,
        theme,
    );
    lines
}

/// CS4's summary panel: renders in place of the diff body while the outline is open and focused
/// with its cursor on a Header/Dir row (see [`App::summary_target`]) — a title line, a blank
/// line, per-file `"path  +N -M"` rows (truncated to the pane height), and a totals line. A
/// loading/failed Header shows its own inline state instead of a file list (see
/// [`changeset_summary_lines`]).
fn render_summary(frame: &mut Frame, summary: &Summary, area: Rect, theme: &Palette) {
    let height = area.height as usize;
    let lines = match summary {
        Summary::Changeset(cs) => changeset_summary_lines(cs, height, theme),
        Summary::Dir(dir) => dir_summary_lines(dir, height, theme),
    };
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_body(frame: &mut Frame, app: &mut App, area: Rect, theme: &Palette) {
    // CS4: the outline is open AND focused, and its cursor rests on a Header/Dir row — show that
    // row's summary instead of a file's diff. Checked before every other body gate below (an
    // unfocused open outline, or the cursor on a File row, falls straight through to the usual
    // diff-body rendering; `summary_target` returns `None` in both cases).
    if let Some(target) = app.summary_target() {
        let summary = app.summary_for(target);
        render_summary(frame, &summary, area, theme);
        return;
    }
    // ADR-037: the active changeset's diff hasn't been acquired (or failed to acquire) yet —
    // both cases have an empty `files()` list, so they must be checked BEFORE the "(no changes)"
    // fallback below, which would otherwise misreport a Pending/Failed changeset as an
    // intentionally empty one.
    if let Some(message) = app.current_failure() {
        let msg = format!("Failed to load this changeset: {message}");
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(FG_ERROR)),
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
            let scroll = app.scroll;
            let cursor = Some(app.cursor);
            // The single pane is the focused one, so it shows any active selection.
            let selection = app.selection_range();
            match app.layout {
                AppLayout::Sbs => render_pane_sbs(
                    frame, app, area, idx, role, scroll, cursor, selection, theme,
                ),
                AppLayout::Inline => render_pane_inline(
                    frame, app, area, idx, role, scroll, cursor, selection, theme,
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
    // Too short to fit two captions plus a content line each: fall back to the focused pane alone,
    // rendered over the whole area, so the user still sees SOMETHING navigable.
    if area.height < 4 {
        let role = app.split_focus_role();
        app.pane_height = area.height as usize;
        let (scroll, cursor) = app.pane_render_state(role);
        let selection = app.selection_range();
        match app.layout {
            AppLayout::Sbs => render_pane_sbs(
                frame, app, area, idx, role, scroll, cursor, selection, theme,
            ),
            AppLayout::Inline => render_pane_inline(
                frame, app, area, idx, role, scroll, cursor, selection, theme,
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
    app.derive_scroll();
    app.derive_alt_scroll();

    render_caption(frame.buffer_mut(), unstaged_caption, "UNSTAGED", theme);
    render_caption(frame.buffer_mut(), staged_caption, "STAGED", theme);

    let (u_scroll, u_cursor) = app.pane_render_state(Role::Unstaged);
    let (s_scroll, s_cursor) = app.pane_render_state(Role::Staged);
    // A selection lives in the focused pane only — the one whose `pane_render_state` yields a
    // cursor. Show it there, `None` in the unfocused pane.
    let range = app.selection_range();
    let u_selection = u_cursor.and(range);
    let s_selection = s_cursor.and(range);
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
            );
        }
    }
}

/// Write a split pane's role caption (`── LABEL ──`) across the pane width, styled like the dim
/// gap-row markers.
fn render_caption(buf: &mut Buffer, area: Rect, label: &str, theme: &Palette) {
    let text = format!("── {label} ──");
    let line = Line::from(TSpan::styled(text, Style::default().fg(theme.dim)));
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
    selection: Option<(usize, usize)>,
    theme: &Palette,
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
                );
                // Cursor wins over selection on the same row (see [`Palette::selection_bg`]).
                let (old_line, new_line) = if is_cursor {
                    (
                        apply_cursor_row(old_line, old_area.width, theme),
                        apply_cursor_row(new_line, new_area.width, theme),
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
                // The divider column was painted once for the whole pane height above, with the
                // default background; re-tint just this row's divider cell so the cursor wash
                // covers the full width (panes AND the `│` between them), like `render_gap_row`.
                if is_cursor {
                    frame.buffer_mut().set_string(
                        div_area.x,
                        y,
                        "│",
                        Style::default().fg(theme.dim).bg(theme.cursor_bg),
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
    mode: AttributionMode,
    old_gutter_w: usize,
    new_gutter_w: usize,
    theme: &Palette,
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
) {
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
                );
                // Cursor wins over selection on the same row (see [`Palette::selection_bg`]).
                let line = if is_cursor {
                    apply_cursor_row(line, area.width, theme)
                } else if is_selected {
                    apply_selection_row(line, area.width, theme)
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
    use crate::keymap::Keymap;
    use crate::outline::OutlineItem;
    use crate::theme::Palette;

    /// Render one frame against the default (unrebound) keymap and the dark theme — the vast
    /// majority of `render.rs` tests don't care about keybindings and only ever ran dark. Tests
    /// that DO care about bindings (the footer/overlay content tests) build their own [`Keymap`]
    /// and call [`render`] directly instead. Color assertions resolve through [`Palette::dark`], so
    /// they pin the exact dark values the refactor must preserve (ADR-035's pixel-identity gate).
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
            footer.contains("open") && footer.contains("mode") && footer.contains("? help"),
            "expected the curated outline hint string in the footer, got: {footer:?}"
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
            Some(super::FG_ERROR),
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

    // ── M5 CS2: winbar (locked decisions #8 + #9) ─────────────────────────────

    /// Build a two-committed-changeset stack for the winbar tests, hand-built the same way as
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
    fn winbar_shows_changeset_position_title_path_and_restack_marker() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();

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
        assert!(
            header.contains("b.txt") && header.contains("(1/1)"),
            "expected the active file's path and position, got: {header:?}"
        );
    }

    #[test]
    fn winbar_restack_marker_carries_the_warning_color() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);

        let buf = render_once(&mut app, 80, 20);
        let header: String = (0..buf.area.width).map(|x| cell_text(&buf, x, 0)).collect();
        let marker_x = header.find('⚠').expect("restack glyph present") as u16;
        assert_eq!(
            buf.cell((marker_x, 0)).unwrap().style().fg,
            Some(super::FG_WARN),
            "expected the restack glyph to carry the warning color, not the plain header color"
        );
    }

    #[test]
    fn winbar_uses_title_when_present() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.prev_changeset();

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
    fn winbar_absent_for_a_lone_changeset() {
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
            "a lone changeset must not render the winbar chrome, got: {header:?}"
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
            .position(|r| r.contains('\u{25CF}'))
            .expect("current marker present in the outline");
        let marker_x = content[row].find('\u{25CF}').unwrap() as u16;
        assert_eq!(
            buf.cell((marker_x, row as u16)).unwrap().style().fg,
            Some(super::FG_CURRENT),
            "expected the outline's current marker to carry FG_CURRENT"
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
            Some(super::FG_WARN),
            "expected the outline's restack glyph to carry FG_WARN"
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
        app.outline_cycle_mode(); // Stack -> Tree
        app.outline_cycle_mode(); // Tree -> StackTree
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
        app.outline_cycle_mode(); // Stack -> Tree
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();

        // Row order per the CS3 dirs-before-files/alpha-within-group rule, one outline row per
        // buffer row starting at y=1 (y=0 is the winbar): `src/` (dir, root, NOT the root's last
        // child — `top.txt` follows), `a.txt` nested one level under `src/` (the only — hence
        // last — child of `src/`), then `top.txt` (file, root, IS the root's last child).
        assert!(
            content[1].contains('\u{251C}') && content[1].contains("src/"),
            "expected row 1 to be the src/ directory row with a non-last '├─' guide, got:\n{}",
            content.join("\n")
        );
        assert!(
            content[2].contains('\u{2514}') && content[2].contains("a.txt"),
            "expected row 2 to be src/a.txt, indented under src/ with its own last-child '└─' \
             guide, got:\n{}",
            content.join("\n")
        );
        assert!(
            content[3].contains('\u{2514}') && content[3].contains("top.txt"),
            "expected row 3 to be top.txt with a last-child '└─' guide, got:\n{}",
            content.join("\n")
        );
    }

    // ── CS5: file status letter + opt-in nerd-font icons ───────────────────────────

    #[test]
    fn outline_file_row_shows_the_modified_change_letter_in_its_own_color() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.rs", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        // A lone changeset defaults the outline closed — force it open so this render test can
        // inspect its rows (same pattern as `outline_tree_mode_renders_directory_rows_with_tree_guides`).
        if !app.outline_open() {
            app.toggle_outline();
        }

        let buf = render_once(&mut app, OUTLINE_TEST_WIDTH, 20);
        let content: Vec<String> = (0..buf.area.height).map(|y| outline_row(&buf, y)).collect();
        // Skip y=0: the full-width winbar also names the file ("[1/1] a.rs"), so an unskipped
        // search would match it instead of the outline's own row below it.
        let row = content
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, r)| r.contains("a.rs"))
            .map(|(i, _)| i)
            .expect("a.rs's file row present");
        assert!(
            content[row].contains('M'),
            "expected the Modified change letter 'M' in a.rs's row, got: {:?}",
            content[row]
        );

        let letter_x = content[row].find('M').unwrap() as u16;
        assert_eq!(
            buf.cell((letter_x, row as u16)).unwrap().style().fg,
            Some(Palette::dark().foreground),
            "Modified is a change-to-existing-content status, so its letter must carry the \
             theme's neutral foreground, not an add/del tint"
        );
    }

    #[test]
    fn outline_icons_nerd_renders_the_rust_file_icon_and_the_dir_icon() {
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
        app.outline_cycle_mode(); // Stack -> Tree, so `src/` renders as its own Dir row
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);
        app.set_outline_icons(crate::icons::OutlineIcons::Nerd);

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
    fn outline_icons_none_renders_neither_icon() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = changeset_with_nested_paths(&fixture);
        if !app.outline_open() {
            app.toggle_outline();
        }
        app.outline_cycle_mode(); // Stack -> Tree
        assert_eq!(app.outline_mode(), crate::outline::OutlineMode::Tree);
        assert_eq!(
            app.outline_icons(),
            crate::icons::OutlineIcons::None,
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
}
