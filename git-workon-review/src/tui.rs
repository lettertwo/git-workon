//! Terminal lifecycle, event seam, and the main input loop for the review TUI.
//!
//! Ported loop shape from the `review-tui-spike` prototype's `main.rs` (`install_panic_hook`,
//! raw-mode + alternate-screen setup, `draw -> quit-check -> next_event -> update`), adapted to
//! read events through [`next_event`] rather than calling crossterm directly from the loop.
//!
//! M4's index watcher (locked decision #4) does NOT swap `next_event`'s internals for a
//! channel-fed watcher thread, despite an earlier note here suggesting that direction — the
//! locked decision is a synchronous poll on the existing `Tick` (every `next_event` timeout),
//! comparing [`workon_review::refresh::IndexSignature`] and re-diffing in place via
//! [`App::on_tick`] when it changes. No threads, no `mpsc`, no new deps.

use std::fs::File;
use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use workon_review::app::App;
use workon_review::keymap::{Command, Dispatch, KeyPress, Keymap};
use workon_review::render;
use workon_review::theme::Palette;

/// One event the review loop reacts to. `Tick` is now also the index-watcher's poll beat (see the
/// module doc's note on locked decision #4) — `next_event`'s mapping and this enum otherwise stay
/// the shape M3 built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,
}

/// Poll for the next terminal event, up to `timeout`.
///
/// `Ok(Some(AppEvent::Tick))` on a plain timeout (the loop's regular redraw beat); `Ok(None)` for
/// a terminal event we don't map to an [`AppEvent`] (key release/repeat, mouse, paste, focus) —
/// the loop redraws and keeps going without calling `update`.
pub fn next_event(timeout: Duration) -> io::Result<Option<AppEvent>> {
    if !event::poll(timeout)? {
        return Ok(Some(AppEvent::Tick));
    }
    Ok(match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(AppEvent::Key(key)),
        Event::Resize(w, h) => Some(AppEvent::Resize(w, h)),
        _ => None,
    })
}

/// Cap on how many events [`drain_pending`] batches per iteration — leftover input past this
/// count is simply picked up by the next iteration's `next_event` call.
const MAX_DRAIN_BATCH: usize = 128;

/// Drain all immediately-available terminal events into `batch`, mapping them exactly like
/// [`next_event`]'s read arm (key-press and resize map; release/repeat/mouse/paste/focus are
/// skipped, not pushed). Unlike calling `next_event(Duration::ZERO)` in a loop, a not-ready poll
/// here simply stops draining — it must NOT fabricate a `Tick`, since `next_event`'s `!poll` arm
/// exists solely to give the loop its regular redraw beat on a real timeout, and reusing it here
/// would inject a spurious tick at the end of every drain.
fn drain_pending(batch: &mut Vec<AppEvent>) -> io::Result<()> {
    while batch.len() < MAX_DRAIN_BATCH {
        if !event::poll(Duration::ZERO)? {
            break;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => batch.push(AppEvent::Key(key)),
            Event::Resize(w, h) => batch.push(AppEvent::Resize(w, h)),
            _ => {}
        }
    }
    Ok(())
}

/// The action a mapped key requests, independent of any [`App`] — kept separate from
/// [`map_key`]'s dispatch so the mapping itself is unit-testable without building an `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Quit,
    ToggleHelp,
    MoveCursorBy(i64),
    ScrollTop,
    ScrollBottom,
    NextFile,
    PrevFile,
    NextHunk,
    PrevHunk,
    NextChangeset,
    PrevChangeset,
    ToggleLayout,
    CycleZoom,
    ToggleSplitFocus,
    Refresh,
    StageHunk,
    StageFile,
    DiscardHunk,
    DiscardFile,
    StartSelection,
    ToggleOutline,
    OutlineMoveBy(i64),
    OutlineConfirm,
    OutlineCycleMode,
    OutlineUnfocus,
    None,
}

/// Convert a resolved rebindable [`Command`] into the concrete [`Action`] the loop applies,
/// supplying the runtime context the registry can't hold — here, the pane height that sizes a
/// half-page scroll (`Ctrl-d`/`Ctrl-u`). This is the seam between the config-driven keymap and the
/// hardcoded action effects.
fn command_to_action(command: Command, pane_height: usize) -> Action {
    let half_page = (pane_height / 2).max(1) as i64;
    match command {
        Command::Quit => Action::Quit,
        Command::ToggleOutline => Action::ToggleOutline,
        Command::ToggleHelp => Action::ToggleHelp,
        Command::CursorDown => Action::MoveCursorBy(1),
        Command::CursorUp => Action::MoveCursorBy(-1),
        Command::HalfPageDown => Action::MoveCursorBy(half_page),
        Command::HalfPageUp => Action::MoveCursorBy(-half_page),
        Command::ScrollTop => Action::ScrollTop,
        Command::ScrollBottom => Action::ScrollBottom,
        Command::ToggleLayout => Action::ToggleLayout,
        Command::CycleZoom => Action::CycleZoom,
        Command::ToggleSplitFocus => Action::ToggleSplitFocus,
        Command::Refresh => Action::Refresh,
        Command::StageHunk => Action::StageHunk,
        Command::StageFile => Action::StageFile,
        Command::DiscardHunk => Action::DiscardHunk,
        Command::DiscardFile => Action::DiscardFile,
        Command::StartSelection => Action::StartSelection,
        Command::NextFile => Action::NextFile,
        Command::PrevFile => Action::PrevFile,
        Command::NextHunk => Action::NextHunk,
        Command::PrevHunk => Action::PrevHunk,
        Command::NextChangeset => Action::NextChangeset,
        Command::PrevChangeset => Action::PrevChangeset,
        Command::OutlineDown => Action::OutlineMoveBy(1),
        Command::OutlineUp => Action::OutlineMoveBy(-1),
        Command::OutlineConfirm => Action::OutlineConfirm,
        Command::OutlineCycleMode => Action::OutlineCycleMode,
    }
}

/// Map one key press to an [`Action`] through the resolved [`Keymap`], given `pending` (the
/// in-flight multi-key sequence buffer — generalized from the old `]`/`[` bracket chord to ANY
/// bound sequence), the current pane height (for the half-page deltas), and whether the outline
/// pane currently has focus.
///
/// Dispatch order:
/// 1. The keymap ([`Keymap::advance`]) consumes the key. A bound sequence fires its command; a
///    strict prefix reports [`Dispatch::Pending`] and holds the buffer for the next key; an
///    unrecognized suffix mid-sequence drops the buffer without re-processing (the old
///    bracket-drop behavior, now general).
/// 2. `Esc` stays HARDCODED (ADR-028: the whole `Esc`-precedence cascade is never routed through
///    the registry). Reached only as a fresh, otherwise-unbound key: it unfocuses the outline when
///    the outline has focus, else quits — the terminal leaf of the cascade `update` enforces.
///
/// `outline_focused` selects the keymap's outline vs diff context; the global bindings (`q`/`o`)
/// are active in both, so `o` toggles and `q` quits from either pane.
fn map_key(
    keymap: &Keymap,
    pending: &mut Vec<KeyPress>,
    key: KeyEvent,
    pane_height: usize,
    outline_focused: bool,
) -> Action {
    match keymap.advance(outline_focused, pending, key) {
        Dispatch::Command(command) => command_to_action(command, pane_height),
        Dispatch::Pending => Action::None,
        Dispatch::Unmatched { mid_sequence } => {
            if !mid_sequence && key.code == KeyCode::Esc {
                if outline_focused {
                    Action::OutlineUnfocus
                } else {
                    Action::Quit
                }
            } else {
                Action::None
            }
        }
    }
}

/// Apply an [`Action`] to `app`. Returns `true` when the loop should exit.
fn apply_action(app: &mut App, action: Action) -> bool {
    match action {
        Action::Quit => return true,
        Action::ToggleHelp => app.toggle_help(),
        Action::MoveCursorBy(delta) => app.move_cursor_by(delta),
        Action::ScrollTop => app.scroll_top(),
        Action::ScrollBottom => app.scroll_bottom(),
        Action::NextFile => app.next_file(),
        Action::PrevFile => app.prev_file(),
        Action::NextHunk => app.next_hunk_row(),
        Action::PrevHunk => app.prev_hunk_row(),
        Action::NextChangeset => app.next_changeset(),
        Action::PrevChangeset => app.prev_changeset(),
        Action::ToggleLayout => app.toggle_layout(),
        Action::CycleZoom => app.cycle_zoom(),
        Action::ToggleSplitFocus => app.toggle_split_focus(),
        Action::Refresh => app.coordinated_refresh(),
        Action::StageHunk => app.stage_hunk(),
        Action::StageFile => app.stage_file(),
        Action::DiscardHunk => app.discard_hunk(),
        Action::DiscardFile => app.discard_file(),
        Action::StartSelection => app.start_selection(),
        Action::ToggleOutline => app.toggle_outline(),
        Action::OutlineMoveBy(delta) => app.outline_move_by(delta),
        Action::OutlineConfirm => app.outline_confirm(),
        Action::OutlineCycleMode => app.outline_cycle_mode(),
        Action::OutlineUnfocus => app.outline_unfocus(),
        Action::None => {}
    }
    false
}

/// The result of resolving a `Key` event through the non-modal cascade (see [`resolve_key`]):
/// either the key was fully handled inline (the selection-Esc guard cancelled the selection), or
/// it resolved to an [`Action`] still waiting to be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOutcome {
    Handled,
    Action(Action),
}

/// Resolve one `Key` event to a [`KeyOutcome`], given the caller has already ruled out the two
/// modal cases (a pending discard confirm, the help overlay) — this is cases 3-5 of `update`'s
/// documented Esc-precedence cascade, extracted so [`update`] and [`update_batch`] share the exact
/// same resolution instead of duplicating it.
///
/// Clears any showing footer notice as a side effect, exactly like `update`'s cases 3-5 do (the
/// confirm/help modals deliberately do not — that stays in their own arms, not here).
fn resolve_key(
    app: &mut App,
    keymap: &Keymap,
    pending: &mut Vec<KeyPress>,
    key: KeyEvent,
) -> KeyOutcome {
    if app.selection_anchor.is_some() && key.code == KeyCode::Esc && !app.outline_focused() {
        app.clear_notice();
        app.cancel_selection();
        return KeyOutcome::Handled;
    }
    app.clear_notice();
    KeyOutcome::Action(map_key(
        keymap,
        pending,
        key,
        app.pane_height,
        app.outline_focused(),
    ))
}

/// Apply one [`AppEvent`] to `app`. Returns `true` when the loop should exit (q/Esc). Resize is a
/// no-op — ratatui re-measures `body_area` every frame regardless. Tick drives
/// [`App::on_tick`], the M4 index watcher's poll (see the module doc).
///
/// A `Key` event clears any showing footer notice BEFORE applying the key's own action, so a
/// notice stays visible until the user's next keystroke — that same keystroke both dismisses the
/// message and performs its normal action. `Resize`/`Tick` do NOT clear it: a redraw or timer
/// tick isn't the user acting on the message.
///
/// Esc precedence (highest first): a pending discard confirm > the help overlay being open > the
/// outline having focus > an active line selection > the normal key map (where Esc quits).
/// Concretely:
///
/// 1. A pending discard confirm captures the keyboard FIRST (before the notice clear and the
///    normal key map): `y` accepts, `n`/`Esc` cancels, and every other key is swallowed — a modal
///    that neither clears the notice nor runs a normal action while it's up.
/// 2. Otherwise, the help overlay (`?`) captures the keyboard next, mirroring the confirm modal's
///    swallow: `?`/`q`/`Esc` close it, every other key is a no-op (nothing on the diff behind it
///    reacts). Ranked just below the confirm modal — in practice the two are never up
///    together, since opening help doesn't run through a confirm, but the confirm winning keeps
///    a destructive prompt from ever being silently dismissed by a stray overlay key.
/// 3. Otherwise, while the outline pane has focus, Esc returns focus to the diff (via the normal
///    map's `outline_focused` branch — see [`map_key`]) rather than quitting or falling into the
///    selection-cancel case below (locked design: "Esc must still not quit when the outline has
///    focus"). The selection-Esc arm below is guarded to defer to this case.
/// 4. Otherwise, with an active line selection, Esc CANCELS the selection instead of quitting (`q`
///    still quits). Other keys fall through to the normal map — `j`/`k` extend the selection,
///    `s`/`d` act on it.
/// 5. Otherwise the normal map applies, where Esc (like `q`) quits.
///
/// A `Key` event clears any showing footer notice before applying its own action (cases 3-5); the
/// confirm and help modals (cases 1-2) deliberately do not. Cases 3-5 are delegated to
/// [`resolve_key`], shared with [`update_batch`].
fn update(app: &mut App, keymap: &Keymap, pending: &mut Vec<KeyPress>, event: AppEvent) -> bool {
    match event {
        AppEvent::Key(key) if app.pending_confirm.is_some() => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => app.resolve_confirm(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    app.resolve_confirm(false)
                }
                _ => {}
            }
            false
        }
        AppEvent::Key(key) if app.help_visible => {
            match key.code {
                KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc => app.toggle_help(),
                _ => {}
            }
            false
        }
        AppEvent::Key(key) => match resolve_key(app, keymap, pending, key) {
            KeyOutcome::Handled => false,
            KeyOutcome::Action(action) => apply_action(app, action),
        },
        AppEvent::Tick => {
            app.on_tick();
            false
        }
        AppEvent::Resize(_, _) => false,
    }
}

/// One in-flight coalesced nav run tracked by [`update_batch`]: a same-sign burst of either
/// outline moves or diff-cursor moves, deferred until a context-changing event forces a flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunKind {
    OutlineMoveBy,
    MoveCursorBy,
}

/// Apply and clear `run`, if one is open. `outline_move_by`/`move_cursor_by` both clamp at their
/// ends, so one call with the summed delta lands exactly where the equivalent sequence of unit
/// calls would (see [`update_batch`]'s doc comment for why this only holds for same-sign runs).
fn flush_run(app: &mut App, run: &mut Option<(RunKind, i64)>) {
    if let Some((kind, delta)) = run.take() {
        match kind {
            RunKind::OutlineMoveBy => app.outline_move_by(delta),
            RunKind::MoveCursorBy => app.move_cursor_by(delta),
        }
    }
}

/// Drain-and-coalesce entry point used by the event loop (`update` stays the single-event
/// primitive whose doc-comment cascade and tests are the spec — this delegates to it for
/// everything that isn't a coalescable nav key).
///
/// Batches `events` (already drained by [`drain_pending`]) and merges same-sign runs of
/// `Action::OutlineMoveBy`/`Action::MoveCursorBy` into ONE deferred `App` call each, so
/// intermediate outline rows in a fast `j`/`k` burst are never opened (`render_body` only loads
/// the landing row, at draw time, via `App::ensure_loaded`). Returns `true` when the loop should
/// exit; remaining batched events after a quit are dropped.
///
/// # Why coalescing same-sign runs is safe
///
/// - Both `App::outline_move_by(delta)` and `App::move_cursor_by(delta)` clamp at the ends; for a
///   same-sign run, one call with the summed delta lands exactly where N unit calls land. Mixed
///   signs are NOT equivalent at a clamped boundary (`k` at row 0 then `j` = row 1, but summed
///   delta 0 = row 0 — a no-op) — hence a sign change always flushes the open run first.
/// - Applying a Move action never changes key-mapping context: it cannot toggle outline focus,
///   alter pane height (render sets it), open a modal, or change the keymap. So resolving key N+1
///   before applying keys 1..N's deferred run is sound. Any action that COULD change context
///   (`ToggleOutline`, `ToggleHelp`, zoom, refresh, a modal, …) forces a flush before it is
///   applied, preserving strict ordering.
/// - `outline_move_by(sum)` only opens the LANDING row's file (the jump happens once, at the
///   final position) — this is precisely what skips the intermediate loads.
fn update_batch(
    app: &mut App,
    keymap: &Keymap,
    pending: &mut Vec<KeyPress>,
    events: Vec<AppEvent>,
) -> bool {
    let mut run: Option<(RunKind, i64)> = None;

    for event in events {
        match event {
            // The coalescable path: no modal is up, and this isn't the selection-Esc-cancel
            // guard (that guard is a context change — an "Esc cascade" — so it falls to the
            // catch-all arm below, which flushes first and delegates the whole event to
            // `update`). Notice-clearing still happens per key via `resolve_key`.
            AppEvent::Key(key)
                if app.pending_confirm.is_none()
                    && !app.help_visible
                    && !(app.selection_anchor.is_some()
                        && key.code == KeyCode::Esc
                        && !app.outline_focused()) =>
            {
                match resolve_key(app, keymap, pending, key) {
                    KeyOutcome::Action(Action::OutlineMoveBy(delta)) => match &mut run {
                        Some((RunKind::OutlineMoveBy, acc)) if acc.signum() == delta.signum() => {
                            *acc += delta;
                        }
                        _ => {
                            flush_run(app, &mut run);
                            run = Some((RunKind::OutlineMoveBy, delta));
                        }
                    },
                    KeyOutcome::Action(Action::MoveCursorBy(delta)) => match &mut run {
                        Some((RunKind::MoveCursorBy, acc)) if acc.signum() == delta.signum() => {
                            *acc += delta;
                        }
                        _ => {
                            flush_run(app, &mut run);
                            run = Some((RunKind::MoveCursorBy, delta));
                        }
                    },
                    // Any other resolved action (Quit, ToggleHelp, chord-pending `Action::None`,
                    // …) can change context, so flush first, then apply it directly — `resolve_key`
                    // already did the notice-clear and keymap resolution `update` would have done
                    // for this key, so applying here (rather than re-delegating to `update`)
                    // avoids resolving the same key twice.
                    KeyOutcome::Action(action) => {
                        flush_run(app, &mut run);
                        if apply_action(app, action) {
                            return true;
                        }
                    }
                    // The selection-Esc guard already ran inline inside `resolve_key`, but the
                    // outer match guard above rules this arm's condition out before we ever
                    // reach it — kept for exhaustiveness.
                    KeyOutcome::Handled => flush_run(app, &mut run),
                }
            }
            // Any other event — Tick, Resize, a modal-captured key, or the selection-Esc-cancel
            // guard — flushes the open run first, then is handled with `update`'s existing,
            // unmodified semantics.
            _ => {
                flush_run(app, &mut run);
                if update(app, keymap, pending, event) {
                    return true;
                }
            }
        }
    }

    flush_run(app, &mut run);
    false
}

/// Open the controlling terminal (`/dev/tty`) for writing, falling back to stdout when there is
/// none (a pipe/CI with no tty). The TUI renders here rather than to stdout so it stays usable
/// inside a shell command substitution: the `workon` wrapper function captures `git workon`'s
/// stdout to `cd` into a printed path, and `git workon review` dispatches to this TUI — if the
/// alternate screen went to the captured stdout, nothing would reach the terminal and the wrapper
/// would hang. Writing to `/dev/tty` keeps stdout clean (this mirrors crossterm, which already
/// reads *input* events from `/dev/tty` on unix). The boxed writer unifies the two branches so the
/// rest of the lifecycle is one type.
fn terminal_writer() -> Box<dyn Write> {
    match File::options().write(true).open("/dev/tty") {
        Ok(tty) => Box::new(tty),
        Err(_) => Box::new(io::stdout()),
    }
}

/// Install a panic hook that restores the terminal (raw mode off, leave alternate screen) before
/// the default hook prints the panic — without this, a panic mid-review leaves the user's shell
/// in alternate-screen raw mode with no visible message. Ported from the spike's
/// `install_panic_hook`. Restores on `/dev/tty` (where the alternate screen was entered), falling
/// back to stdout — matching [`terminal_writer`].
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut out = terminal_writer();
        let _ = execute!(out, LeaveAlternateScreen);
        default_hook(info);
    }));
}

/// Run the review TUI's terminal lifecycle and main loop against `app`. Callers must have
/// already loaded the initial file (`app.open_current()`) before calling this.
pub fn run(app: &mut App, keymap: &Keymap, theme: &Palette) -> io::Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    let mut out = terminal_writer();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, app, keymap, theme);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn event_loop<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    app: &mut App,
    keymap: &Keymap,
    theme: &Palette,
) -> io::Result<()> {
    let mut pending: Vec<KeyPress> = Vec::new();
    let mut quit = false;

    loop {
        terminal.draw(|f| render::render(f, app, keymap, theme))?;

        if quit {
            return Ok(());
        }

        if let Some(event) = next_event(Duration::from_millis(200))? {
            let mut batch = vec![event];
            drain_pending(&mut batch)?;
            quit = update_batch(app, keymap, &mut pending, batch);
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn quit_keys_map_to_quit() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('q')), 20, false),
            Action::Quit
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Esc), 20, false),
            Action::Quit
        );
    }

    #[test]
    fn scroll_keys_map_by_one_line() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('j')), 20, false),
            Action::MoveCursorBy(1)
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Down), 20, false),
            Action::MoveCursorBy(1)
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('k')), 20, false),
            Action::MoveCursorBy(-1)
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Up), 20, false),
            Action::MoveCursorBy(-1)
        );
    }

    #[test]
    fn ctrl_d_u_scroll_by_half_the_pane_height() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, ctrl_key('d'), 21, false),
            Action::MoveCursorBy(10)
        );
        assert_eq!(
            map_key(&km, &mut pending, ctrl_key('u'), 21, false),
            Action::MoveCursorBy(-10)
        );
        // A pane height of 1 still scrolls by at least one line.
        assert_eq!(
            map_key(&km, &mut pending, ctrl_key('d'), 1, false),
            Action::MoveCursorBy(1)
        );
    }

    #[test]
    fn g_and_shift_g_map_to_top_and_bottom() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('g')), 20, false),
            Action::ScrollTop
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('G')), 20, false),
            Action::ScrollBottom
        );
    }

    #[test]
    fn shift_l_maps_to_toggle_layout() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('L')), 20, false),
            Action::ToggleLayout
        );
    }

    #[test]
    fn z_and_w_map_to_zoom_and_split_focus() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('z')), 20, false),
            Action::CycleZoom
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('w')), 20, false),
            Action::ToggleSplitFocus
        );
    }

    #[test]
    fn r_maps_to_refresh() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('r')), 20, false),
            Action::Refresh
        );
    }

    #[test]
    fn tab_and_backtab_map_to_file_nav() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Tab), 20, false),
            Action::NextFile
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::BackTab), 20, false),
            Action::PrevFile
        );
    }

    #[test]
    fn bracket_f_maps_to_file_nav() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char(']')), 20, false),
            Action::None
        );
        // The buffer holds the in-flight chord prefix (generalized from the old `Option<char>`).
        assert_eq!(pending, vec![KeyPress::from_event(key(KeyCode::Char(']')))]);
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('f')), 20, false),
            Action::NextFile
        );
        assert!(pending.is_empty());

        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('[')), 20, false),
            Action::None
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('f')), 20, false),
            Action::PrevFile
        );
    }

    #[test]
    fn bracket_h_maps_to_hunk_nav() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        map_key(&km, &mut pending, key(KeyCode::Char(']')), 20, false);
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('h')), 20, false),
            Action::NextHunk
        );

        map_key(&km, &mut pending, key(KeyCode::Char('[')), 20, false);
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('h')), 20, false),
            Action::PrevHunk
        );
    }

    #[test]
    fn unrecognized_bracket_suffix_drops_pending_without_side_effect() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        map_key(&km, &mut pending, key(KeyCode::Char(']')), 20, false);
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('x')), 20, false),
            Action::None
        );
        assert!(
            pending.is_empty(),
            "pending bracket must be cleared, not left dangling"
        );
    }

    /// Build an [`App`] straight from a fixture's repo, for `tui`'s own event-loop tests. `app.rs`
    /// has an identical private helper (`test_support::app_from_fixture`), but that's
    /// `pub(crate)` to the `workon_review` LIB crate — invisible here, since `tui.rs` compiles
    /// into the separate bin crate (see `main.rs`'s `mod tui;`). Not worth promoting the lib's
    /// helper to `pub` just to share four lines across a crate boundary.
    fn app_from_fixture(fixture: &git_workon_fixture::fixture::Fixture) -> App {
        use git2::Repository;
        use workon_review::acquire::diff_uncommitted;

        let repo = fixture.repo().expect("fixture repo");
        let diffs = diff_uncommitted(repo).expect("diff_uncommitted");
        let owned = Repository::open(repo.workdir().expect("fixture has a workdir"))
            .expect("reopen fixture repo");
        App::new(owned, diffs)
    }

    #[test]
    fn key_event_through_update_clears_a_previously_set_notice() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::Severity;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        app.notify("something happened", Severity::Info);
        assert!(app.notice.is_some());

        // Any key — even one that maps to no action — dismisses the notice.
        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('x'))),
        );
        assert!(
            app.notice.is_none(),
            "a Key event must clear a showing notice"
        );
    }

    #[test]
    fn r_key_through_update_refreshes_without_panicking() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('r'))),
        );

        // A no-op refresh (nothing changed externally) still rebuilds the view in place; the
        // smoke test is simply that this doesn't panic and the file is still there.
        assert_eq!(app.files().len(), 1);
        assert_eq!(app.files()[0].path, "a.txt");
    }

    #[test]
    fn tick_and_resize_events_do_not_clear_a_notice() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::Severity;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        app.notify("something happened", Severity::Info);

        update(&mut app, &km, &mut pending, AppEvent::Tick);
        assert!(app.notice.is_some(), "a Tick event must not clear a notice");

        update(&mut app, &km, &mut pending, AppEvent::Resize(80, 24));
        assert!(
            app.notice.is_some(),
            "a Resize event must not clear a notice"
        );
    }

    #[test]
    fn tick_event_through_update_calls_on_tick_without_panicking() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        // A plain Tick with nothing changed externally must be a safe no-op wired all the way
        // through `update` — the smoke test for M4's index-watcher hookup (the substantive
        // signature-change/echo-suppression assertions live in `app.rs`'s own `on_tick` tests,
        // which have direct access to its private state).
        let quit = update(&mut app, &km, &mut pending, AppEvent::Tick);

        assert!(!quit, "Tick must never quit the loop");
        assert_eq!(app.files().len(), 1);
        assert_eq!(app.files()[0].path, "a.txt");
    }

    #[test]
    fn staging_keys_map_to_their_actions() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('s')), 20, false),
            Action::StageHunk
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('S')), 20, false),
            Action::StageFile
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('d')), 20, false),
            Action::DiscardHunk
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('D')), 20, false),
            Action::DiscardFile
        );
        // Ctrl-d keeps its half-page meaning — the plain-`d` staging arm must not shadow it.
        assert_eq!(
            map_key(&km, &mut pending, ctrl_key('d'), 20, false),
            Action::MoveCursorBy(10)
        );
    }

    #[test]
    fn v_maps_to_start_selection() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('v')), 20, false),
            Action::StartSelection
        );
    }

    #[test]
    fn esc_precedence_confirm_over_selection_over_quit() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::PendingOp;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        // Lowest precedence: with neither a confirm nor a selection up, Esc quits.
        assert!(
            update(
                &mut app,
                &km,
                &mut pending,
                AppEvent::Key(key(KeyCode::Esc))
            ),
            "Esc quits when nothing modal is active"
        );

        // Middle precedence: an active selection makes Esc cancel the selection (not quit).
        app.start_selection();
        assert!(app.selection_anchor.is_some());
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );
        assert!(!quit, "Esc must not quit while a selection is active");
        assert!(
            app.selection_anchor.is_none(),
            "Esc cancels the active selection"
        );

        // Highest precedence: a pending confirm captures Esc as a cancel, even with a selection up.
        app.start_selection();
        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );
        assert!(!quit, "Esc must not quit while a confirm is pending");
        assert!(
            app.pending_confirm.is_none(),
            "the confirm arm consumes Esc first"
        );
    }

    #[test]
    fn pending_confirm_captures_y_and_n_and_ignores_other_keys() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::PendingOp;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        // A pending confirm makes every non-answer key a no-op — the cursor doesn't move and the
        // confirm stays up.
        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });
        let cursor_before = app.cursor;
        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('j'))),
        );
        assert!(
            app.pending_confirm.is_some(),
            "a non-answer key must not resolve the confirm"
        );
        assert_eq!(
            app.cursor, cursor_before,
            "a captured key must not run its normal action"
        );

        // `n` cancels it.
        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('n'))),
        );
        assert!(app.pending_confirm.is_none(), "n must cancel the confirm");

        // `y` resolves (and runs) a fresh confirm.
        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });
        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('y'))),
        );
        assert!(app.pending_confirm.is_none(), "y must resolve the confirm");
        let repo = fixture.repo().unwrap();
        repo.assert(predicate::repo::workdir_file_equals("a.txt", "one\ntwo\n"));
    }

    // ── M5 CS3: outline pane key routing ─────────────────────────────────────

    /// A two-committed-changeset stack, built the same way as `app.rs`/`render.rs`'s own M5
    /// tests — `tui.rs` needs its own copy since it compiles into the separate bin crate (see
    /// `app_from_fixture`'s doc comment above for why the helpers can't be shared directly).
    fn two_committed_changesets_app(fixture: &git_workon_fixture::fixture::Fixture) -> App {
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};
        use workon_review::acquire::diff_changeset;
        use workon_review::app::ChangesetView;

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
            name: "cs-b".to_string(),
            span: ChangesetSpan::Committed { base: mid, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view_a =
            ChangesetView::from_changeset_diff(cs_a.clone(), diff_changeset(repo, &cs_a).unwrap());
        let view_b =
            ChangesetView::from_changeset_diff(cs_b.clone(), diff_changeset(repo, &cs_b).unwrap());
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view_a, view_b]);
        app.open_current();
        app
    }

    #[test]
    fn o_key_toggles_the_outline_through_its_full_cycle() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        // Default: open, unfocused.
        assert!(app.outline_open() && !app.outline_focused());

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('o'))),
        );
        assert!(!app.outline_open(), "o from open+unfocused closes the pane");

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('o'))),
        );
        assert!(
            app.outline_open() && app.outline_focused(),
            "o from closed opens AND focuses the pane"
        );

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('o'))),
        );
        assert!(
            app.outline_open() && !app.outline_focused(),
            "o from open+focused returns focus to the diff without closing"
        );
    }

    #[test]
    fn outline_focused_j_k_move_the_outline_cursor_not_the_diff() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline(); // close
        app.toggle_outline(); // open + focus, cursor synced onto cs-b's file row (the LAST row)
        assert!(app.outline_focused());
        let diff_cursor_before = app.cursor;
        let diff_file_before = app.current;
        let outline_cursor_before = app.outline_cursor();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        // `k` (not `j`): the outline cursor starts on the last row (cs-b's file, since it's the
        // active/current changeset), so `j` would clamp in place — `k` has room to move.
        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('k'))),
        );

        assert_ne!(
            app.outline_cursor(),
            outline_cursor_before,
            "k while the outline has focus must move the OUTLINE cursor"
        );
        assert_eq!(
            (app.cursor, app.current),
            (diff_cursor_before, diff_file_before),
            "j while the outline has focus must not move the diff's own cursor \
             (unless the outline cursor happened to land on a different file row, which the \
             two-changeset fixture's first outline move does not)"
        );
    }

    #[test]
    fn esc_does_not_quit_while_the_outline_has_focus() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline(); // close
        app.toggle_outline(); // open + focus
        assert!(app.outline_focused());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );

        assert!(!quit, "Esc must not quit while the outline has focus");
        assert!(
            !app.outline_focused(),
            "Esc while the outline has focus returns focus to the diff"
        );
        assert!(
            app.outline_open(),
            "Esc must not also close the pane, only unfocus it"
        );
    }

    #[test]
    fn enter_confirms_an_outline_jump_and_returns_focus_to_the_diff() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline(); // close
        app.toggle_outline(); // open + focus, cursor synced onto cs-b's file row
        assert!(app.outline_focused());
        // Move the outline cursor up onto cs-a's header row.
        app.outline_move_by(-3);
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Enter)),
        );

        assert_eq!(
            app.current_cs(),
            0,
            "Enter on cs-a's header must jump there"
        );
        assert_eq!(app.current, 0, "...landing on its first file");
        assert!(
            !app.outline_focused(),
            "Enter returns focus to the diff after jumping"
        );
    }

    // ── CS3: help overlay ───────────────────────────────────────────────────

    #[test]
    fn question_mark_opens_the_help_overlay() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert!(!app.help_visible);

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('?'))),
        );
        assert!(app.help_visible, "? opens the help overlay");
    }

    #[test]
    fn while_help_is_open_other_keys_are_swallowed() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        app.toggle_help();
        assert!(app.help_visible);
        let cursor_before = app.cursor;

        // `j` would normally move the cursor — while help is up it must be a pure no-op, exactly
        // like the pending-confirm modal's swallow.
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('j'))),
        );

        assert!(!quit);
        assert!(
            app.help_visible,
            "an unrelated key must not close the overlay"
        );
        assert_eq!(
            app.cursor, cursor_before,
            "a swallowed key must not run its normal action"
        );
    }

    #[test]
    fn question_mark_q_and_esc_all_close_the_help_overlay() {
        use git_workon_fixture::prelude::*;

        for close_key in [
            key(KeyCode::Char('?')),
            key(KeyCode::Char('q')),
            key(KeyCode::Esc),
        ] {
            let fixture = FixtureBuilder::new()
                .config("core.autocrlf", "false")
                .build()
                .unwrap();
            let mut app = app_from_fixture(&fixture);
            let km = Keymap::defaults();
            let mut pending: Vec<KeyPress> = Vec::new();

            app.toggle_help();
            assert!(app.help_visible);

            let quit = update(&mut app, &km, &mut pending, AppEvent::Key(close_key));

            assert!(!quit, "closing help must not also quit the app");
            assert!(
                !app.help_visible,
                "{close_key:?} must close the help overlay"
            );
        }
    }

    #[test]
    fn a_pending_confirm_still_wins_over_an_open_help_overlay() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::PendingOp;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        app.toggle_help();
        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });

        // `y` while BOTH modals are up must resolve the confirm (case 1 wins per `update`'s
        // documented precedence), not close help or fall through to a normal action.
        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('y'))),
        );

        assert!(
            app.pending_confirm.is_none(),
            "the confirm modal must capture y first"
        );
        assert!(
            app.help_visible,
            "the confirm arm must not have touched help_visible"
        );
    }

    // ── CS2: coalesce buffered nav input ─────────────────────────────────────

    /// A single committed changeset with `n` distinct multi-line files ("f0.txt".."f{n-1}.txt"),
    /// opened on file 0 — CS2's batching tests need several files so a coalesced outline jump has
    /// intermediate rows to skip over, and several lines per file so a coalesced diff-cursor run
    /// has room to move without immediately clamping.
    fn many_files_app(fixture: &git_workon_fixture::fixture::Fixture, n: usize) -> App {
        use git2::Repository;
        use workon::{Changeset, ChangesetSpan};
        use workon_review::acquire::diff_changeset;
        use workon_review::app::ChangesetView;

        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let mut builder = fixture.commit("main");
        for i in 0..n {
            builder = builder.file(
                &format!("f{i}.txt"),
                &format!("line-{i}-a\nline-{i}-b\nline-{i}-c\nline-{i}-d\nline-{i}-e\n"),
            );
        }
        let head = builder.create("head").unwrap();
        let repo = fixture.repo().unwrap();

        let cs = Changeset {
            name: "cs".to_string(),
            span: ChangesetSpan::Committed { base: root, head },
            title: None,
            current: true,
            needs_restack: false,
        };
        let view =
            ChangesetView::from_changeset_diff(cs.clone(), diff_changeset(repo, &cs).unwrap());
        let owned = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut app = App::from_changesets(owned, vec![view]);
        app.open_current();
        app
    }

    #[test]
    fn batched_outline_jump_skips_intermediate_file_loads() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::Role;
        use workon_review::outline::OutlineMode;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = many_files_app(&fixture, 5);
        app.set_outline_mode(OutlineMode::Flat);
        app.toggle_outline(); // open + focus, cursor synced onto file 0's row (index 0 in Flat mode)
        assert!(app.outline_focused());
        assert!(
            app.role_view_ref(0, Role::Combined).is_some(),
            "file 0 loaded by open_current"
        );

        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        // 4 outline-down keys: sequentially this would visit (and load) files 1, 2, 3, then land
        // on 4 — coalescing must apply ONE outline_move_by(4), landing on file 4 directly.
        let events = vec![
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
        ];

        let quit = update_batch(&mut app, &km, &mut pending, events);

        assert!(!quit);
        assert_eq!(
            app.outline_cursor(),
            4,
            "the outline cursor lands on the final row"
        );
        assert_eq!(app.current, 4, "the diff jumps to the landing file only");
        for skipped in 1..4 {
            assert!(
                app.role_view_ref(skipped, Role::Combined).is_none(),
                "file {skipped} must never have been visited, so its view must not be loaded"
            );
        }
        assert!(
            app.role_view_ref(4, Role::Combined).is_some(),
            "the landing file's view IS loaded"
        );
    }

    #[test]
    fn batched_outline_jump_matches_sequential_moves() {
        use git_workon_fixture::prelude::*;
        use workon_review::outline::OutlineMode;

        let fixture_batch = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app_batch = many_files_app(&fixture_batch, 5);
        app_batch.set_outline_mode(OutlineMode::Flat);
        app_batch.toggle_outline();

        let fixture_seq = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app_seq = many_files_app(&fixture_seq, 5);
        app_seq.set_outline_mode(OutlineMode::Flat);
        app_seq.toggle_outline();

        let km = Keymap::defaults();
        let mut pending_batch: Vec<KeyPress> = Vec::new();
        let mut pending_seq: Vec<KeyPress> = Vec::new();
        let events = vec![
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
        ];

        update_batch(&mut app_batch, &km, &mut pending_batch, events.clone());
        for event in events {
            update(&mut app_seq, &km, &mut pending_seq, event);
        }

        assert_eq!(app_batch.outline_cursor(), app_seq.outline_cursor());
        assert_eq!(app_batch.current, app_seq.current);
    }

    #[test]
    fn mixed_direction_batch_matches_sequential_moves_including_at_a_clamp_boundary() {
        use git_workon_fixture::prelude::*;

        // Mixed-sign run (j,j,j,k) starting away from any boundary. Each `App` gets its OWN
        // fixture — `many_files_app` commits onto the fixture's `main`, so reusing one fixture
        // across calls would have the second call's "head" commit re-add files the first call's
        // "head" already committed, producing an empty diff for it.
        let fixture_batch = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app_batch = many_files_app(&fixture_batch, 1);
        let fixture_seq = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app_seq = many_files_app(&fixture_seq, 1);
        let km = Keymap::defaults();
        let mut pending_batch: Vec<KeyPress> = Vec::new();
        let mut pending_seq: Vec<KeyPress> = Vec::new();
        let events = vec![
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('k'))),
        ];

        update_batch(&mut app_batch, &km, &mut pending_batch, events.clone());
        for event in events {
            update(&mut app_seq, &km, &mut pending_seq, event);
        }
        assert_eq!(app_batch.cursor, app_seq.cursor);

        // Clamp-boundary case: k then j starting at row 0 — a naive sum (0) would wrongly stay
        // put; sequential unit calls land on row 1 (k clamps at 0, then j moves to 1).
        let fixture_batch2 = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app_batch2 = many_files_app(&fixture_batch2, 1);
        let fixture_seq2 = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app_seq2 = many_files_app(&fixture_seq2, 1);
        let mut pending_batch2: Vec<KeyPress> = Vec::new();
        let mut pending_seq2: Vec<KeyPress> = Vec::new();
        let boundary_events = vec![
            AppEvent::Key(key(KeyCode::Char('k'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
        ];

        update_batch(
            &mut app_batch2,
            &km,
            &mut pending_batch2,
            boundary_events.clone(),
        );
        for event in boundary_events {
            update(&mut app_seq2, &km, &mut pending_seq2, event);
        }
        assert_eq!(app_batch2.cursor, app_seq2.cursor);
        assert_eq!(app_batch2.cursor, 1, "k clamps at 0, then j moves to row 1");
    }

    #[test]
    fn a_context_changing_key_mid_run_applies_moves_in_their_own_context() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = many_files_app(&fixture, 5);
        // Default OutlineMode::Stack: row 0 is the header, row 1 is file 0 — so `o`'s
        // sync-to-current lands the outline cursor on row 1, and a single outline `k` afterward
        // lands on the header row (no file jump), leaving `app.cursor`/`app.current` observable.
        assert!(!app.outline_open());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        let events = vec![
            AppEvent::Key(key(KeyCode::Char('j'))), // MoveCursorBy(1), outline unfocused
            AppEvent::Key(key(KeyCode::Char('j'))), // MoveCursorBy(1), outline unfocused
            AppEvent::Key(key(KeyCode::Char('o'))), // ToggleOutline: open + focus
            AppEvent::Key(key(KeyCode::Char('k'))), // OutlineMoveBy(-1), outline now focused
        ];

        let quit = update_batch(&mut app, &km, &mut pending, events);

        assert!(!quit);
        assert_eq!(
            app.cursor, 2,
            "the two j's before `o` must apply as diff-cursor moves in the OLD context"
        );
        assert!(
            app.outline_open() && app.outline_focused(),
            "`o` toggles the outline open and focused"
        );
        assert_eq!(
            app.outline_cursor(),
            0,
            "the k after `o` must apply as an outline move in the NEW context, landing on the \
             header row"
        );
        assert_eq!(
            app.current, 0,
            "landing on the header row must not jump the diff"
        );
    }

    #[test]
    fn a_pending_confirm_disables_coalescing_and_batch_matches_sequential_updates() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::PendingOp;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app_batch = app_from_fixture(&fixture);
        app_batch.open_current();
        let mut app_seq = app_from_fixture(&fixture);
        app_seq.open_current();

        app_batch.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });
        app_seq.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });

        let km = Keymap::defaults();
        let mut pending_batch: Vec<KeyPress> = Vec::new();
        let mut pending_seq: Vec<KeyPress> = Vec::new();
        let cursor_before = app_batch.cursor;
        let events = vec![
            AppEvent::Key(key(KeyCode::Char('j'))), // swallowed by the confirm modal
            AppEvent::Key(key(KeyCode::Char('n'))), // cancels the confirm
        ];

        update_batch(&mut app_batch, &km, &mut pending_batch, events.clone());
        for event in events {
            update(&mut app_seq, &km, &mut pending_seq, event);
        }

        assert_eq!(
            app_batch.cursor, cursor_before,
            "a captured key inside the modal must not run its normal action"
        );
        assert!(app_batch.pending_confirm.is_none(), "n cancels the confirm");
        assert_eq!(app_batch.cursor, app_seq.cursor);
        assert_eq!(
            app_batch.pending_confirm.is_none(),
            app_seq.pending_confirm.is_none()
        );
    }

    #[test]
    fn quit_mid_batch_drops_the_remaining_events_and_returns_true() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = many_files_app(&fixture, 1);
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        let cursor_before = app.cursor;
        let events = vec![
            AppEvent::Key(key(KeyCode::Char('j'))), // applies: cursor_before + 1
            AppEvent::Key(key(KeyCode::Char('q'))), // quits
            AppEvent::Key(key(KeyCode::Char('j'))), // dropped: must never apply
        ];

        let quit = update_batch(&mut app, &km, &mut pending, events);

        assert!(quit, "q mid-batch must report quit");
        assert_eq!(
            app.cursor,
            cursor_before + 1,
            "only the j before q must have applied"
        );
    }

    #[test]
    fn a_chord_split_across_two_batches_still_fires() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = many_files_app(&fixture, 3);
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(app.current, 0);

        // First batch: only the chord's first key arrives — held in `pending` across the drain
        // boundary, exactly like a real terminal delivering the two keys in separate polls.
        let quit1 = update_batch(
            &mut app,
            &km,
            &mut pending,
            vec![AppEvent::Key(key(KeyCode::Char(']')))],
        );
        assert!(!quit1);
        assert_eq!(pending, vec![KeyPress::from_event(key(KeyCode::Char(']')))]);

        // Second batch: the chord's second key completes it via the SAME `pending` buffer.
        let quit2 = update_batch(
            &mut app,
            &km,
            &mut pending,
            vec![AppEvent::Key(key(KeyCode::Char('f')))],
        );
        assert!(!quit2);
        assert!(pending.is_empty());
        assert_eq!(app.current, 1, "]f must have fired NextFile");
    }
}
