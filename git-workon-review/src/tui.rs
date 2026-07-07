//! Terminal lifecycle, event seam, and the main input loop for the review TUI.
//!
//! Ported loop shape from the `review-tui-spike` prototype's `main.rs` (`install_panic_hook`,
//! raw-mode + alternate-screen setup, `draw -> quit-check -> next_event -> update`), adapted to
//! read events through [`next_event`] rather than calling crossterm directly from the loop: M4
//! swaps `next_event`'s internals for an mpsc channel fed by watcher threads without changing
//! the loop shape or [`AppEvent`]'s shape.

use std::io::{self, Stdout};
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

/// One event the review loop reacts to. `next_event`'s crossterm-specific mapping is the only
/// piece M4 will replace (for an mpsc channel fed by a file-watcher thread) — the loop and this
/// enum stay the same shape.
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
    ToggleLayout,
    CycleZoom,
    ToggleSplitFocus,
    Refresh,
    StageHunk,
    StageFile,
    DiscardHunk,
    DiscardFile,
    StartSelection,
    None,
}

/// Map one key press to an [`Action`], given `pending` (a `]` or `[` seen on the previous call,
/// awaiting its `f`/`h` suffix) and the current pane height (for `Ctrl-d`/`Ctrl-u` half-page
/// deltas). Unrecognized suffixes drop the pending bracket rather than re-processing the key.
fn map_key(pending: &mut Option<char>, key: KeyEvent, pane_height: usize) -> Action {
    if let Some(bracket) = pending.take() {
        return match (bracket, key.code) {
            (']', KeyCode::Char('f')) => Action::NextFile,
            ('[', KeyCode::Char('f')) => Action::PrevFile,
            (']', KeyCode::Char('h')) => Action::NextHunk,
            ('[', KeyCode::Char('h')) => Action::PrevHunk,
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
        Action::ToggleLayout => app.toggle_layout(),
        Action::CycleZoom => app.cycle_zoom(),
        Action::ToggleSplitFocus => app.toggle_split_focus(),
        Action::Refresh => app.refresh(),
        Action::StageHunk => app.stage_hunk(),
        Action::StageFile => app.stage_file(),
        Action::DiscardHunk => app.discard_hunk(),
        Action::DiscardFile => app.discard_file(),
        Action::StartSelection => app.start_selection(),
        Action::None => {}
    }
    false
}

/// Apply one [`AppEvent`] to `app`. Returns `true` when the loop should exit (q/Esc). Resize and
/// Tick are no-ops today — ratatui re-measures `body_area` every frame regardless, and Tick
/// exists for M4's periodic-refresh consumers, not M3's read-only loop.
///
/// A `Key` event clears any showing footer notice BEFORE applying the key's own action, so a
/// notice stays visible until the user's next keystroke — that same keystroke both dismisses the
/// message and performs its normal action. `Resize`/`Tick` do NOT clear it: a redraw or timer
/// tick isn't the user acting on the message.
///
/// Esc precedence (highest first): a pending discard confirm > an active line selection > the
/// normal key map (where Esc quits). Concretely:
///
/// 1. A pending discard confirm captures the keyboard FIRST (before the notice clear and the
///    normal key map): `y` accepts, `n`/`Esc` cancels, and every other key is swallowed — a modal
///    that neither clears the notice nor runs a normal action while it's up.
/// 2. Otherwise, with an active line selection, Esc CANCELS the selection instead of quitting (`q`
///    still quits). Other keys fall through to the normal map — `j`/`k` extend the selection,
///    `s`/`d` act on it.
/// 3. Otherwise the normal map applies, where Esc (like `q`) quits.
///
/// A `Key` event clears any showing footer notice before applying its own action (cases 2 and 3);
/// the confirm modal (case 1) deliberately does not.
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
        AppEvent::Key(key) if app.selection_anchor.is_some() && key.code == KeyCode::Esc => {
            app.clear_notice();
            app.cancel_selection();
            false
        }
        AppEvent::Key(key) => {
            app.clear_notice();
            apply_action(app, map_key(pending, key, app.pane_height))
        }
        AppEvent::Resize(_, _) | AppEvent::Tick => false,
    }
}

/// Install a panic hook that restores the terminal (raw mode off, leave alternate screen) before
/// the default hook prints the panic — without this, a panic mid-review leaves the user's shell
/// in alternate-screen raw mode with no visible message. Ported from the spike's
/// `install_panic_hook`.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}

/// Run the review TUI's terminal lifecycle and main loop against `app`. Callers must have
/// already loaded the initial file (`app.open_current()`) before calling this.
pub fn run(app: &mut App) -> io::Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
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
            map_key(&mut pending, key(KeyCode::Char('q')), 20),
            Action::Quit
        );
        assert_eq!(map_key(&mut pending, key(KeyCode::Esc), 20), Action::Quit);
    }

    #[test]
    fn scroll_keys_map_by_one_line() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('j')), 20),
            Action::MoveCursorBy(1)
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Down), 20),
            Action::MoveCursorBy(1)
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('k')), 20),
            Action::MoveCursorBy(-1)
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Up), 20),
            Action::MoveCursorBy(-1)
        );
    }

    #[test]
    fn ctrl_d_u_scroll_by_half_the_pane_height() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, ctrl_key('d'), 21),
            Action::MoveCursorBy(10)
        );
        assert_eq!(
            map_key(&mut pending, ctrl_key('u'), 21),
            Action::MoveCursorBy(-10)
        );
        // A pane height of 1 still scrolls by at least one line.
        assert_eq!(
            map_key(&mut pending, ctrl_key('d'), 1),
            Action::MoveCursorBy(1)
        );
    }

    #[test]
    fn g_and_shift_g_map_to_top_and_bottom() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('g')), 20),
            Action::ScrollTop
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('G')), 20),
            Action::ScrollBottom
        );
    }

    #[test]
    fn shift_l_maps_to_toggle_layout() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('L')), 20),
            Action::ToggleLayout
        );
    }

    #[test]
    fn z_and_w_map_to_zoom_and_split_focus() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('z')), 20),
            Action::CycleZoom
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('w')), 20),
            Action::ToggleSplitFocus
        );
    }

    #[test]
    fn r_maps_to_refresh() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('r')), 20),
            Action::Refresh
        );
    }

    #[test]
    fn tab_and_backtab_map_to_file_nav() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Tab), 20),
            Action::NextFile
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::BackTab), 20),
            Action::PrevFile
        );
    }

    #[test]
    fn bracket_f_maps_to_file_nav() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char(']')), 20),
            Action::None
        );
        assert_eq!(pending, Some(']'));
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('f')), 20),
            Action::NextFile
        );
        assert_eq!(pending, None);

        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('[')), 20),
            Action::None
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('f')), 20),
            Action::PrevFile
        );
    }

    #[test]
    fn bracket_h_maps_to_hunk_nav() {
        let mut pending = None;
        map_key(&mut pending, key(KeyCode::Char(']')), 20);
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('h')), 20),
            Action::NextHunk
        );

        map_key(&mut pending, key(KeyCode::Char('[')), 20);
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('h')), 20),
            Action::PrevHunk
        );
    }

    #[test]
    fn unrecognized_bracket_suffix_drops_pending_without_side_effect() {
        let mut pending = None;
        map_key(&mut pending, key(KeyCode::Char(']')), 20);
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('x')), 20),
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
        assert_eq!(app.files.len(), 1);
        assert_eq!(app.files[0].path, "a.txt");
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
    fn staging_keys_map_to_their_actions() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('s')), 20),
            Action::StageHunk
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('S')), 20),
            Action::StageFile
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('d')), 20),
            Action::DiscardHunk
        );
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('D')), 20),
            Action::DiscardFile
        );
        // Ctrl-d keeps its half-page meaning — the plain-`d` staging arm must not shadow it.
        assert_eq!(
            map_key(&mut pending, ctrl_key('d'), 20),
            Action::MoveCursorBy(10)
        );
    }

    #[test]
    fn v_maps_to_start_selection() {
        let mut pending = None;
        assert_eq!(
            map_key(&mut pending, key(KeyCode::Char('v')), 20),
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
}
