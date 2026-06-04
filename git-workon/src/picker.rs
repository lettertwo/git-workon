//! Generic interactive picker backed by `console::Term`.
//!
//! Provides a single `select` function that drives a pick loop over any list
//! produced by a caller-supplied render function. The caller controls what items
//! are displayed and which item the cursor starts on; the picker handles the
//! terminal lifecycle, key events, and redraw.
//!
//! ## Interaction model
//!
//! - Type characters to refine the query; the render function is called on every
//!   keystroke and may filter/highlight the list however it likes.
//! - `Backspace` removes the last query character.
//! - `↑`/`↓` move the cursor through the current visible list.
//! - `Enter` confirms the selection (returns the key at the current cursor).
//! - `Esc` / `Ctrl-C` cancel (returns `None`).
//!
//! ## Terminal safety
//!
//! Raw mode is entered when the picker opens and a [`CursorGuard`] ensures
//! `show_cursor` is called even if the picker returns early (Esc, Ctrl-C) or
//! panics.

use dialoguer::console::{Key, Term};
use miette::{IntoDiagnostic, Result};

use crate::display::PickerRender;
use crate::output::style;

/// Run an interactive picker.
///
/// `prompt` is shown above the list (e.g. `"Select a worktree"`).
/// `render` is called with the current query string on every keystroke; it
/// returns the full set of visible lines, their selection keys, and the index
/// the cursor should jump to (best fuzzy match, or the active item).
///
/// Returns the selected key on `Enter`, or `None` on `Esc`/`Ctrl-C`.
pub fn select(prompt: &str, render: impl Fn(&str) -> PickerRender) -> Result<Option<String>> {
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
                // Clear the picker UI before returning.
                term.clear_last_lines(prev_line_count).into_diagnostic()?;
                return Ok(Some(rendered.keys[cursor].clone()));
            }
            // Empty list: ignore Enter.
            Key::Escape | Key::CtrlC => {
                term.clear_last_lines(prev_line_count).into_diagnostic()?;
                return Ok(None);
            }
            _ => {}
        }
    }
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
    term.write_line(&format!("{}{}", style::bold(prompt), query_display))
        .into_diagnostic()?;

    if rendered.lines.is_empty() {
        term.write_line(&style::dim("  (no matches)"))
            .into_diagnostic()?;
    } else {
        for (i, line) in rendered.lines.iter().enumerate() {
            if i == cursor {
                let marker = style::cyan_bold("▶");
                let tinted = style::cursor_tint(line);
                term.write_line(&format!("{} {}", marker, tinted))
                    .into_diagnostic()?;
            } else {
                term.write_line(&format!("  {}", line)).into_diagnostic()?;
            }
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
