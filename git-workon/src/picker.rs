//! Generic interactive pickers backed by `console::Term`.
//!
//! Provides two pick loops over caller-supplied lists; the picker handles the
//! terminal lifecycle, key events, and redraw.
//!
//! ## `select` — single-select with fuzzy query (used by `find`)
//!
//! - Type characters to refine the query; the render function is called on every
//!   keystroke and may filter/highlight the list however it likes.
//! - `Backspace` removes the last query character.
//! - `↑`/`↓` move the cursor through the current visible list.
//! - `Enter` confirms the selection with [`PickerAction::Resolve`].
//! - `Tab` confirms the selection with [`PickerAction::Materialize`] (force create).
//! - `Esc` / `Ctrl-C` cancel (returns `None`).
//!
//! ## `multi_select` — checkbox multi-select (used by `prune`)
//!
//! - `↑`/`↓` move the cursor; `Space` toggles the row under the cursor.
//! - `a` toggles all rows at once (all-on unless already all-on, then all-off).
//! - `Enter` confirms the current selection; `Esc` / `Ctrl-C` cancel (returns `None`).
//! - No fuzzy query: prune lists are short, and `Space` would conflict with typing.
//! - Checkbox glyphs reuse the project vocabulary: `◉` (green) selected, `◯` (dim)
//!   unselected.
//!
//! ## Terminal safety
//!
//! Raw mode is entered when a picker opens and a [`CursorGuard`] ensures
//! `show_cursor` is called even if the picker returns early (Esc, Ctrl-C) or
//! panics.

use std::borrow::Cow;

use dialoguer::console::{truncate_str, Key, Term};
use miette::{IntoDiagnostic, Result};

use crate::display::PickerRender;
use crate::output::style;

/// How the user confirmed their selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// `Enter` — use the normal resolution path.
    Resolve,
    /// `Tab` — bypass the resolver and force-materialize a fresh worktree.
    Materialize,
}

/// Run an interactive picker.
///
/// `prompt` is shown above the list (e.g. `"Select a worktree"`).
/// `render` is called with the current query string on every keystroke; it
/// returns the full set of visible lines, their selection keys, and the index
/// the cursor should jump to (best fuzzy match, or the active item).
///
/// Returns `(selected_key, action)` on `Enter`/`Tab`, or `None` on `Esc`/`Ctrl-C`.
pub fn select(
    prompt: &str,
    render: impl Fn(&str) -> PickerRender,
) -> Result<Option<(String, PickerAction)>> {
    let term = Term::stderr();
    term.hide_cursor().into_diagnostic()?;
    let _guard = CursorGuard(&term);

    let mut query = String::new();
    let mut rendered = render(&query);
    let mut cursor = rendered.cursor;
    let mut prev_line_count = 0usize;

    // Initial draw.
    draw(&term, prompt, &query, &rendered, cursor, prev_line_count)?;
    prev_line_count = prompt_lines(prompt, &query) + rendered.lines.len();

    loop {
        match term.read_key().into_diagnostic()? {
            Key::Char(c) => {
                query.push(c);
                rendered = render(&query);
                // Jump cursor to best match whenever the query changes.
                cursor = rendered.cursor;
                draw(&term, prompt, &query, &rendered, cursor, prev_line_count)?;
                prev_line_count = prompt_lines(prompt, &query) + rendered.lines.len();
            }
            Key::Backspace => {
                query.pop();
                rendered = render(&query);
                cursor = rendered.cursor;
                draw(&term, prompt, &query, &rendered, cursor, prev_line_count)?;
                prev_line_count = prompt_lines(prompt, &query) + rendered.lines.len();
            }
            Key::ArrowUp => {
                let n = rendered.lines.len();
                if n > 0 {
                    cursor = (cursor + n - 1) % n;
                }
                draw(&term, prompt, &query, &rendered, cursor, prev_line_count)?;
            }
            Key::ArrowDown => {
                let n = rendered.lines.len();
                if n > 0 {
                    cursor = (cursor + 1) % n;
                }
                draw(&term, prompt, &query, &rendered, cursor, prev_line_count)?;
            }
            Key::Enter if !rendered.keys.is_empty() => {
                term.clear_last_lines(prev_line_count).into_diagnostic()?;
                return Ok(Some((rendered.keys[cursor].clone(), PickerAction::Resolve)));
            }
            Key::Tab if !rendered.keys.is_empty() => {
                term.clear_last_lines(prev_line_count).into_diagnostic()?;
                return Ok(Some((
                    rendered.keys[cursor].clone(),
                    PickerAction::Materialize,
                )));
            }
            // Empty list: ignore Enter/Tab.
            Key::Escape | Key::CtrlC => {
                term.clear_last_lines(prev_line_count).into_diagnostic()?;
                return Ok(None);
            }
            _ => {}
        }
    }
}

/// Run an interactive checkbox multi-select.
///
/// `prompt` is shown above the list. `lines` are pre-rendered display rows (the
/// caller controls styling and alignment); `defaults` marks the initially-checked
/// rows (parallel to `lines`, padded with `false` when shorter).
///
/// Returns the checked indices on `Enter` (possibly empty), or `None` on
/// `Esc`/`Ctrl-C`.
pub fn multi_select(
    prompt: &str,
    lines: &[String],
    defaults: &[bool],
) -> Result<Option<Vec<usize>>> {
    let term = Term::stderr();
    term.hide_cursor().into_diagnostic()?;
    let _guard = CursorGuard(&term);

    let mut checked: Vec<bool> = (0..lines.len())
        .map(|i| defaults.get(i).copied().unwrap_or(false))
        .collect();
    let mut cursor = 0usize;
    // Fixed list (no filtering), so the redraw height never changes.
    let line_count = 1 + lines.len();

    draw_multi(&term, prompt, lines, &checked, cursor, 0)?;

    loop {
        match term.read_key().into_diagnostic()? {
            Key::ArrowUp => {
                if !lines.is_empty() {
                    cursor = (cursor + lines.len() - 1) % lines.len();
                }
                draw_multi(&term, prompt, lines, &checked, cursor, line_count)?;
            }
            Key::ArrowDown => {
                if !lines.is_empty() {
                    cursor = (cursor + 1) % lines.len();
                }
                draw_multi(&term, prompt, lines, &checked, cursor, line_count)?;
            }
            Key::Char(' ') if !lines.is_empty() => {
                checked[cursor] = !checked[cursor];
                draw_multi(&term, prompt, lines, &checked, cursor, line_count)?;
            }
            Key::Char('a') if !lines.is_empty() => {
                let target = !checked.iter().all(|c| *c);
                checked.iter_mut().for_each(|c| *c = target);
                draw_multi(&term, prompt, lines, &checked, cursor, line_count)?;
            }
            Key::Enter => {
                term.clear_last_lines(line_count).into_diagnostic()?;
                let selected = checked
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.then_some(i))
                    .collect();
                return Ok(Some(selected));
            }
            Key::Escape | Key::CtrlC => {
                term.clear_last_lines(line_count).into_diagnostic()?;
                return Ok(None);
            }
            _ => {}
        }
    }
}

/// Render one multi-select row: cursor marker + checkbox + content line.
///
/// Extracted as a pure function so the glyph/tint logic is unit-testable; the
/// interaction loop above stays thin.
fn render_multi_row(line: &str, is_checked: bool, is_cursor: bool) -> String {
    let checkbox = if is_checked {
        style::green("◉")
    } else {
        style::dim("◯")
    };
    if is_cursor {
        let marker = style::cyan_bold("▶");
        format!("{} {} {}", marker, checkbox, style::cursor_tint(line))
    } else {
        format!("  {} {}", checkbox, line)
    }
}

/// Fit a styled line to the terminal width (ANSI-aware truncation with a `…` tail).
///
/// `clear_last_lines` counts logical lines, so a row that wraps onto extra physical
/// lines would leave stale copies behind on every redraw. Skipped when the terminal
/// size can't be determined (e.g. not a TTY).
fn fit_line<'a>(term: &Term, line: &'a str) -> Cow<'a, str> {
    match term.size_checked() {
        Some((_, cols)) if cols > 0 => truncate_str(line, cols as usize, "…"),
        _ => Cow::Borrowed(line),
    }
}

/// Redraw the multi-select: clear the previous render, then emit prompt + rows.
fn draw_multi(
    term: &Term,
    prompt: &str,
    lines: &[String],
    checked: &[bool],
    cursor: usize,
    prev_line_count: usize,
) -> Result<()> {
    if prev_line_count > 0 {
        term.clear_last_lines(prev_line_count).into_diagnostic()?;
    }

    let hint = style::dim("(space: toggle, a: all, enter: confirm)");
    term.write_line(&fit_line(
        term,
        &format!("{} {}", style::bold(prompt), hint),
    ))
    .into_diagnostic()?;

    for (i, line) in lines.iter().enumerate() {
        term.write_line(&fit_line(
            term,
            &render_multi_row(line, checked[i], i == cursor),
        ))
        .into_diagnostic()?;
    }

    term.flush().into_diagnostic()?;
    Ok(())
}

/// Redraw the picker: clear the previous render, then emit prompt + list.
fn draw(
    term: &Term,
    prompt: &str,
    query: &str,
    rendered: &PickerRender,
    cursor: usize,
    prev_line_count: usize,
) -> Result<()> {
    if prev_line_count > 0 {
        term.clear_last_lines(prev_line_count).into_diagnostic()?;
    }

    // Prompt line: "prompt [query]"
    let query_display = if query.is_empty() {
        String::new()
    } else {
        format!(" {}", style::dim(&format!("[{}]", query)))
    };
    term.write_line(&fit_line(
        term,
        &format!("{}{}", style::bold(prompt), query_display),
    ))
    .into_diagnostic()?;

    if rendered.lines.is_empty() {
        term.write_line(&style::dim("  (no matches)"))
            .into_diagnostic()?;
    } else {
        for (i, line) in rendered.lines.iter().enumerate() {
            let row = if i == cursor {
                let marker = style::cyan_bold("▶");
                let tinted = style::cursor_tint(line);
                format!("{} {}", marker, tinted)
            } else {
                format!("  {}", line)
            };
            term.write_line(&fit_line(term, &row)).into_diagnostic()?;
        }
    }

    term.flush().into_diagnostic()?;
    Ok(())
}

/// Number of lines the prompt occupies (always 1 in the current layout).
fn prompt_lines(_prompt: &str, _query: &str) -> usize {
    1
}

/// RAII guard that restores the cursor on drop.
///
/// Ensures `show_cursor` is called whether the picker exits normally, via
/// early return, or due to a panic.
struct CursorGuard<'a>(&'a Term);

impl Drop for CursorGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin color off so rendered rows are plain strings and glyph assertions are
    /// byte-exact. Color support is process-global and env-sensitive — FORCE_COLOR
    /// enables styling even when stdout is not a TTY.
    fn no_color() {
        crate::output::set_no_color(true);
    }

    #[test]
    fn render_multi_row_checked_uses_filled_glyph() {
        no_color();
        let row = render_multi_row("./feature", true, false);
        assert_eq!(row, "  ◉ ./feature");
    }

    #[test]
    fn render_multi_row_unchecked_uses_hollow_glyph() {
        no_color();
        let row = render_multi_row("./feature", false, false);
        assert_eq!(row, "  ◯ ./feature");
    }

    #[test]
    fn render_multi_row_cursor_shows_marker() {
        no_color();
        let row = render_multi_row("./feature", false, true);
        assert_eq!(row, "▶ ◯ ./feature");
    }

    #[test]
    fn render_multi_row_cursor_and_checked_combine() {
        no_color();
        let row = render_multi_row("./feature", true, true);
        assert_eq!(row, "▶ ◉ ./feature");
    }
}
