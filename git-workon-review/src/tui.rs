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

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use workon_review::app::App;
use workon_review::render;

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

/// The action a mapped key requests, independent of any [`App`] — kept separate from
/// [`map_key`]'s dispatch so the mapping itself is unit-testable without building an `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Quit,
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

/// Map one key press to an [`Action`], given `pending` (a `]` or `[` seen on the previous call,
/// awaiting its `f`/`h` suffix), the current pane height (for `Ctrl-d`/`Ctrl-u` half-page
/// deltas), and whether the outline pane currently has focus. Unrecognized suffixes drop the
/// pending bracket rather than re-processing the key.
///
/// `outline_focused` re-routes the plain single-key map (NOT the bracket-pending path, which is
/// diff-only chording that can't be mid-flight while the outline has focus) to the outline's own
/// small key set (locked design: "only outline-relevant keys — `j k Enter i o Esc` — act" while
/// it has focus). `o` always toggles regardless of focus (checked before the split) since it's
/// the one key that must work from EITHER side to move focus between panes; `q` still quits from
/// either side too — the locked design only enumerates outline-focused keys, it doesn't say `q`
/// should stop working.
fn map_key(
    pending: &mut Option<char>,
    key: KeyEvent,
    pane_height: usize,
    outline_focused: bool,
) -> Action {
    if let Some(bracket) = pending.take() {
        return match (bracket, key.code) {
            (']', KeyCode::Char('f')) => Action::NextFile,
            ('[', KeyCode::Char('f')) => Action::PrevFile,
            (']', KeyCode::Char('h')) => Action::NextHunk,
            ('[', KeyCode::Char('h')) => Action::PrevHunk,
            (']', KeyCode::Char('c')) => Action::NextChangeset,
            ('[', KeyCode::Char('c')) => Action::PrevChangeset,
            _ => Action::None,
        };
    }

    if key.code == KeyCode::Char('o') {
        return Action::ToggleOutline;
    }

    if outline_focused {
        return match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => Action::OutlineMoveBy(1),
            KeyCode::Char('k') | KeyCode::Up => Action::OutlineMoveBy(-1),
            KeyCode::Enter => Action::OutlineConfirm,
            KeyCode::Char('i') => Action::OutlineCycleMode,
            KeyCode::Esc => Action::OutlineUnfocus,
            _ => Action::None,
        };
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => Action::MoveCursorBy(1),
        KeyCode::Char('k') | KeyCode::Up => Action::MoveCursorBy(-1),
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Action::MoveCursorBy((pane_height / 2).max(1) as i64)
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Action::MoveCursorBy(-((pane_height / 2).max(1) as i64))
        }
        KeyCode::Char('g') => Action::ScrollTop,
        KeyCode::Char('G') => Action::ScrollBottom,
        KeyCode::Char('L') => Action::ToggleLayout,
        KeyCode::Char('z') => Action::CycleZoom,
        KeyCode::Char('w') => Action::ToggleSplitFocus,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('s') => Action::StageHunk,
        KeyCode::Char('S') => Action::StageFile,
        KeyCode::Char('d') => Action::DiscardHunk,
        KeyCode::Char('D') => Action::DiscardFile,
        KeyCode::Char('v') => Action::StartSelection,
        KeyCode::Tab => Action::NextFile,
        KeyCode::BackTab => Action::PrevFile,
        KeyCode::Char(']') => {
            *pending = Some(']');
            Action::None
        }
        KeyCode::Char('[') => {
            *pending = Some('[');
            Action::None
        }
        _ => Action::None,
    }
}

/// Apply an [`Action`] to `app`. Returns `true` when the loop should exit.
fn apply_action(app: &mut App, action: Action) -> bool {
    match action {
        Action::Quit => return true,
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

/// Apply one [`AppEvent`] to `app`. Returns `true` when the loop should exit (q/Esc). Resize is a
/// no-op — ratatui re-measures `body_area` every frame regardless. Tick drives
/// [`App::on_tick`], the M4 index watcher's poll (see the module doc).
///
/// A `Key` event clears any showing footer notice BEFORE applying the key's own action, so a
/// notice stays visible until the user's next keystroke — that same keystroke both dismisses the
/// message and performs its normal action. `Resize`/`Tick` do NOT clear it: a redraw or timer
/// tick isn't the user acting on the message.
///
/// Esc precedence (highest first): a pending discard confirm > the outline having focus > an
/// active line selection > the normal key map (where Esc quits). Concretely:
///
/// 1. A pending discard confirm captures the keyboard FIRST (before the notice clear and the
///    normal key map): `y` accepts, `n`/`Esc` cancels, and every other key is swallowed — a modal
///    that neither clears the notice nor runs a normal action while it's up.
/// 2. Otherwise, while the outline pane has focus, Esc returns focus to the diff (via the normal
///    map's `outline_focused` branch — see [`map_key`]) rather than quitting or falling into the
///    selection-cancel case below (locked design: "Esc must still not quit when the outline has
///    focus"). The selection-Esc arm below is guarded to defer to this case.
/// 3. Otherwise, with an active line selection, Esc CANCELS the selection instead of quitting (`q`
///    still quits). Other keys fall through to the normal map — `j`/`k` extend the selection,
///    `s`/`d` act on it.
/// 4. Otherwise the normal map applies, where Esc (like `q`) quits.
///
/// A `Key` event clears any showing footer notice before applying its own action (cases 2-4); the
/// confirm modal (case 1) deliberately does not.
fn update(app: &mut App, pending: &mut Option<char>, event: AppEvent) -> bool {
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
        AppEvent::Key(key)
            if app.selection_anchor.is_some()
                && key.code == KeyCode::Esc
                && !app.outline_focused() =>
        {
            app.clear_notice();
            app.cancel_selection();
            false
        }
        AppEvent::Key(key) => {
            app.clear_notice();
            apply_action(
                app,
                map_key(pending, key, app.pane_height, app.outline_focused()),
            )
        }
        AppEvent::Tick => {
            app.on_tick();
            false
        }
        AppEvent::Resize(_, _) => false,
    }
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
pub fn run(app: &mut App) -> io::Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    let mut out = terminal_writer();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn event_loop<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    app: &mut App,
) -> io::Result<()> {
    let mut pending: Option<char> = None;
    let mut quit = false;

    loop {
        terminal.draw(|f| render::render(f, app))?;

        if quit {
            return Ok(());
        }

        if let Some(event) = next_event(Duration::from_millis(200))? {
            quit = update(app, &mut pending, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn quit_keys_map_to_quit() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('q')), 20, false),
            Action::Quit
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Esc), 20, false),
            Action::Quit
        );
    }

    #[test]
    fn scroll_keys_map_by_one_line() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('j')), 20, false),
            Action::MoveCursorBy(1)
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Down), 20, false),
            Action::MoveCursorBy(1)
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('k')), 20, false),
            Action::MoveCursorBy(-1)
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Up), 20, false),
            Action::MoveCursorBy(-1)
        );
    }

    #[test]
    fn ctrl_d_u_scroll_by_half_the_pane_height() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, ctrl_key('d'), 21, false),
            Action::MoveCursorBy(10)
        );
        assert_eq!(
            map_key(&mut pending, ctrl_key('u'), 21, false),
            Action::MoveCursorBy(-10)
        );
        // A pane height of 1 still scrolls by at least one line.
        assert_eq!(
            map_key(&mut pending, ctrl_key('d'), 1, false),
            Action::MoveCursorBy(1)
        );
    }

    #[test]
    fn g_and_shift_g_map_to_top_and_bottom() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('g')), 20, false),
            Action::ScrollTop
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('G')), 20, false),
            Action::ScrollBottom
        );
    }

    #[test]
    fn shift_l_maps_to_toggle_layout() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('L')), 20, false),
            Action::ToggleLayout
        );
    }

    #[test]
    fn z_and_w_map_to_zoom_and_split_focus() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('z')), 20, false),
            Action::CycleZoom
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('w')), 20, false),
            Action::ToggleSplitFocus
        );
    }

    #[test]
    fn r_maps_to_refresh() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('r')), 20, false),
            Action::Refresh
        );
    }

    #[test]
    fn tab_and_backtab_map_to_file_nav() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Tab), 20, false),
            Action::NextFile
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::BackTab), 20, false),
            Action::PrevFile
        );
    }

    #[test]
    fn bracket_f_maps_to_file_nav() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char(']')), 20, false),
            Action::None
        );
        assert_eq!(pending, Some(']'));
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('f')), 20, false),
            Action::NextFile
        );
        assert_eq!(pending, None);

        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('[')), 20, false),
            Action::None
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('f')), 20, false),
            Action::PrevFile
        );
    }

    #[test]
    fn bracket_h_maps_to_hunk_nav() {
        let mut pending = None;
        map_key(&mut pending, key(KeyCode::Char(']')), 20, false);
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('h')), 20, false),
            Action::NextHunk
        );

        map_key(&mut pending, key(KeyCode::Char('[')), 20, false);
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('h')), 20, false),
            Action::PrevHunk
        );
    }

    #[test]
    fn unrecognized_bracket_suffix_drops_pending_without_side_effect() {
        let mut pending = None;
        map_key(&mut pending, key(KeyCode::Char(']')), 20, false);
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('x')), 20, false),
            Action::None
        );
        assert_eq!(
            pending, None,
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
        let mut pending = None;

        app.notify("something happened", Severity::Info);
        assert!(app.notice.is_some());

        // Any key — even one that maps to no action — dismisses the notice.
        update(
            &mut app,
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
        let mut pending = None;

        update(
            &mut app,
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
        let mut pending = None;

        app.notify("something happened", Severity::Info);

        update(&mut app, &mut pending, AppEvent::Tick);
        assert!(app.notice.is_some(), "a Tick event must not clear a notice");

        update(&mut app, &mut pending, AppEvent::Resize(80, 24));
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
        let mut pending = None;

        // A plain Tick with nothing changed externally must be a safe no-op wired all the way
        // through `update` — the smoke test for M4's index-watcher hookup (the substantive
        // signature-change/echo-suppression assertions live in `app.rs`'s own `on_tick` tests,
        // which have direct access to its private state).
        let quit = update(&mut app, &mut pending, AppEvent::Tick);

        assert!(!quit, "Tick must never quit the loop");
        assert_eq!(app.files().len(), 1);
        assert_eq!(app.files()[0].path, "a.txt");
    }

    #[test]
    fn staging_keys_map_to_their_actions() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('s')), 20, false),
            Action::StageHunk
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('S')), 20, false),
            Action::StageFile
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('d')), 20, false),
            Action::DiscardHunk
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('D')), 20, false),
            Action::DiscardFile
        );
        // Ctrl-d keeps its half-page meaning — the plain-`d` staging arm must not shadow it.
        assert_eq!(
            map_key(&mut pending, ctrl_key('d'), 20, false),
            Action::MoveCursorBy(10)
        );
    }

    #[test]
    fn v_maps_to_start_selection() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('v')), 20, false),
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
        let mut pending = None;

        // Lowest precedence: with neither a confirm nor a selection up, Esc quits.
        assert!(
            update(&mut app, &mut pending, AppEvent::Key(key(KeyCode::Esc))),
            "Esc quits when nothing modal is active"
        );

        // Middle precedence: an active selection makes Esc cancel the selection (not quit).
        app.start_selection();
        assert!(app.selection_anchor.is_some());
        let quit = update(&mut app, &mut pending, AppEvent::Key(key(KeyCode::Esc)));
        assert!(!quit, "Esc must not quit while a selection is active");
        assert!(
            app.selection_anchor.is_none(),
            "Esc cancels the active selection"
        );

        // Highest precedence: a pending confirm captures Esc as a cancel, even with a selection up.
        app.start_selection();
        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });
        let quit = update(&mut app, &mut pending, AppEvent::Key(key(KeyCode::Esc)));
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
        let mut pending = None;

        // A pending confirm makes every non-answer key a no-op — the cursor doesn't move and the
        // confirm stays up.
        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });
        let cursor_before = app.cursor;
        update(
            &mut app,
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
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('n'))),
        );
        assert!(app.pending_confirm.is_none(), "n must cancel the confirm");

        // `y` resolves (and runs) a fresh confirm.
        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });
        update(
            &mut app,
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
        use workon::{Changeset, ChangesetSource};
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
            source: ChangesetSource::Committed {
                base: root,
                head: mid,
            },
            title: None,
            current: false,
            needs_restack: false,
        };
        let cs_b = Changeset {
            name: "cs-b".to_string(),
            source: ChangesetSource::Committed { base: mid, head },
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
        let mut pending = None;
        // Default: open, unfocused.
        assert!(app.outline_open() && !app.outline_focused());

        update(
            &mut app,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('o'))),
        );
        assert!(!app.outline_open(), "o from open+unfocused closes the pane");

        update(
            &mut app,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('o'))),
        );
        assert!(
            app.outline_open() && app.outline_focused(),
            "o from closed opens AND focuses the pane"
        );

        update(
            &mut app,
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
        let mut pending = None;

        // `k` (not `j`): the outline cursor starts on the last row (cs-b's file, since it's the
        // active/current changeset), so `j` would clamp in place — `k` has room to move.
        update(
            &mut app,
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
        let mut pending = None;

        let quit = update(&mut app, &mut pending, AppEvent::Key(key(KeyCode::Esc)));

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
        let mut pending = None;

        update(&mut app, &mut pending, AppEvent::Key(key(KeyCode::Enter)));

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
}
