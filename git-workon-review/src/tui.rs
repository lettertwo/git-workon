//! Terminal lifecycle, event seam, and the main input loop for the review TUI.
//!
//! Ported loop shape from the `review-tui-spike` prototype's `main.rs` (`install_panic_hook`,
//! raw-mode + alternate-screen setup, `draw -> quit-check -> recv_event -> update`), adapted to
//! read events through the [`AppEvent`] inbox rather than calling crossterm directly from the
//! loop.
//!
//! ADR-037 (progressive pipeline) supersedes M4's locked decision #4 — the "no threads, no
//! `mpsc`" letter of that note, recorded here in an earlier revision, no longer holds. A
//! dedicated *input thread* (spawned by [`Tui::run`]) is now the ONLY code that calls
//! crossterm's event API: it blocks on `event::read()` forever, maps each event exactly like
//! this module's old `next_event`/`drain_pending` read arms did, and forwards mapped events into
//! an `std::sync::mpsc` inbox that the main loop drains via [`recv_event`]/[`drain_pending`].
//! `recv_timeout`'s timeout arm IS the `Tick` beat — unchanged from before, just relocated from
//! `event::poll`'s timeout to the channel's. The M4 index watcher's *semantics* are exactly
//! unchanged by this move: it still compares [`workon_review::refresh::IndexSignature`] and
//! re-diffs in place via [`App::on_tick`] on every `Tick`; only the beat's mechanism moved.

use std::fs::File;
use std::io::{self, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use workon_review::app::App;
use workon_review::keymap::{Command, Dispatch, KeyPress, Keymap};
use workon_review::render;
use workon_review::theme::Palette;

/// One event the review loop reacts to. `Tick` is synthesized by the main loop on an inbox
/// `recv_timeout` timeout — it is never sent through the channel itself (see [`recv_event`]).
/// `Key`/`Resize` are forwarded from the input thread via [`map_terminal_event`]. Not `Copy`
/// (ADR-037): the next slice's loader-result variants carry non-`Copy` payloads; dropping `Copy`
/// now is mechanical prep so this slice's diff doesn't collide with that one's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,
}

/// The inbox message type: a mapped terminal event, or the input thread's terminal `event::read`
/// error forwarded verbatim (ADR-037: "the input thread never exits silently" — a read error is
/// still observable, just relayed rather than swallowed). `Tick` never appears here.
type InboxMessage = io::Result<AppEvent>;

/// Map one crossterm terminal [`Event`] to the [`AppEvent`] the loop reacts to — key-press and
/// resize map; key release/repeat, mouse, paste, and focus events are skipped (`None`), exactly
/// like this module's pre-ADR-037 `next_event`/`drain_pending` read arms did. Pure and
/// independent of any thread or channel, so it's unit-tested directly; the input thread's loop
/// body is a thin wrapper around it.
fn map_terminal_event(event: Event) -> Option<AppEvent> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(AppEvent::Key(key)),
        Event::Resize(w, h) => Some(AppEvent::Resize(w, h)),
        _ => None,
    }
}

/// Spawn the dedicated input thread and return the receiving end of its inbox. Must be called
/// AFTER the terminal is acquired and any pre-takeover tty work (the theme probe, stray-input
/// flush) has finished — crossterm input must not be consumed before that ordering completes
/// (see `main.rs`'s block comment on the resolve/probe/acquire sequence). The thread loops
/// forever on a blocking `event::read()`, forwarding mapped events; on a read error it forwards
/// the error once and exits — the sole way this thread ever stops short of the process dying.
/// Never joined: [`Tui::run`] returns without waiting for it (ADR-037's kill-on-exit lifecycle —
/// the input thread, like the future loader thread, never writes, so an abandoned read can't
/// corrupt anything).
fn spawn_input_thread() -> mpsc::Receiver<InboxMessage> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        match event::read() {
            Ok(event) => {
                if let Some(mapped) = map_terminal_event(event) {
                    if tx.send(Ok(mapped)).is_err() {
                        return; // main loop is gone; nothing left to forward to
                    }
                }
            }
            Err(err) => {
                let _ = tx.send(Err(err));
                return;
            }
        }
    });
    rx
}

/// Receive the next event from `inbox`, waiting up to `timeout`. A timeout with nothing received
/// yields `Ok(AppEvent::Tick)` — the loop's regular redraw beat, and the mechanism the M4 index
/// watcher polls on (see the module doc). A disconnected inbox (the input thread panicked, or
/// exited after an error without this being observed yet) is surfaced as an `io::Error` rather
/// than spinning — the loop must exit, not busy-loop on an empty channel forever.
fn recv_event(inbox: &mpsc::Receiver<InboxMessage>, timeout: Duration) -> io::Result<AppEvent> {
    match inbox.recv_timeout(timeout) {
        Ok(Ok(event)) => Ok(event),
        Ok(Err(err)) => Err(err),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(AppEvent::Tick),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "review TUI input thread disconnected without a final error",
        )),
    }
}

/// Cap on how many events [`drain_pending`] batches per iteration — leftover input past this
/// count is simply picked up by the next iteration's `recv_event` call.
const MAX_DRAIN_BATCH: usize = 128;

/// Drain all immediately-available events from `inbox` into `batch`. Unlike calling
/// `recv_event(inbox, Duration::ZERO)` in a loop, an empty inbox here simply stops draining — it
/// must NOT fabricate a `Tick`, since [`recv_event`]'s timeout arm exists solely to give the loop
/// its regular redraw beat on a real timeout, and reusing it here would inject a spurious tick at
/// the end of every drain.
fn drain_pending(
    inbox: &mpsc::Receiver<InboxMessage>,
    batch: &mut Vec<AppEvent>,
) -> io::Result<()> {
    while batch.len() < MAX_DRAIN_BATCH {
        match inbox.try_recv() {
            Ok(Ok(event)) => batch.push(event),
            Ok(Err(err)) => return Err(err),
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(io::Error::other(
                    "review TUI input thread disconnected without a final error",
                ))
            }
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
/// 2. `Esc` stays HARDCODED (ADR-034: the whole `Esc`-precedence cascade is never routed through
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

/// Whether `action`'s effect READS the current [`App::current_view`]/cursor-space state
/// (cursor-space movement, staging, selection) rather than only changing WHICH file/changeset is
/// current. An action in the first group must force-complete any pending deferred open first (see
/// [`apply_action`]'s chokepoint) so it observes the same loaded view an eager `open_current` would
/// have produced — e.g. `j` then immediately `s` must stage the same hunk eager code would have.
///
/// Exempt (returns `false`): every action that ends in its own fresh `open_current` (`NextFile`,
/// `PrevFile`, `NextChangeset`, `PrevChangeset`, `CycleZoom`, and the outline nav/confirm actions),
/// since those simply set a NEW pending open rather than needing the current one force-completed;
/// plus pure UI toggles/no-ops (`Refresh` rebuilds all views itself; `ToggleHelp`/`Quit`/`None`
/// touch no view state at all).
fn action_needs_loaded_view(action: Action) -> bool {
    matches!(
        action,
        Action::MoveCursorBy(_)
            | Action::ScrollTop
            | Action::ScrollBottom
            | Action::NextHunk
            | Action::PrevHunk
            | Action::StageHunk
            | Action::StageFile
            | Action::DiscardHunk
            | Action::DiscardFile
            | Action::StartSelection
            | Action::ToggleSplitFocus
    )
}

/// Apply an [`Action`] to `app`. Returns `true` when the loop should exit.
///
/// Chokepoint (CS4): before doing anything else, force-complete a pending deferred open for every
/// action [`action_needs_loaded_view`] flags — see that function's doc comment for the principle
/// and the exemption list. [`App::complete_pending_open`] is a no-op when nothing is pending, so
/// this costs nothing outside defer mode (where `open_pending` is never set) or when the debounce
/// window already completed the open on its own.
fn apply_action(app: &mut App, action: Action) -> bool {
    if action_needs_loaded_view(action) {
        app.complete_pending_open();
    }
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

/// The run kind and delta for an action [`update_batch`] can coalesce, or `None` for every
/// other action. The single source of truth for WHICH actions coalesce — `update_batch`'s
/// accumulate arm matches through this so the rule can't drift per action kind.
fn coalescable(action: Action) -> Option<(RunKind, i64)> {
    match action {
        Action::OutlineMoveBy(delta) => Some((RunKind::OutlineMoveBy, delta)),
        Action::MoveCursorBy(delta) => Some((RunKind::MoveCursorBy, delta)),
        _ => None,
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
///
/// `MoveCursorBy` reads cursor-space state exactly like [`apply_action`]'s `Action::MoveCursorBy`
/// arm does, and this is the OTHER path (besides `apply_action`) that can run it — CS2's
/// coalescing calls `App::move_cursor_by` directly rather than routing the flush through
/// `apply_action`, so the same force-completion has to happen here too (see the plan's chokepoint
/// note: whichever path applies `MoveCursorBy` must complete first). `OutlineMoveBy` needs no such
/// call: it ends in its own fresh `open_current`, exactly like `apply_action`'s exemption list.
fn flush_run(app: &mut App, run: &mut Option<(RunKind, i64)>) {
    if let Some((kind, delta)) = run.take() {
        match kind {
            RunKind::OutlineMoveBy => app.outline_move_by(delta),
            RunKind::MoveCursorBy => {
                app.complete_pending_open();
                app.move_cursor_by(delta);
            }
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
/// - `outline_move_by(sum)` opens at most ONE file — the landing row's, or for a header/dir
///   landing the last file the burst crossed (see its doc comment) — rather than one per row
///   crossed. That single jump is precisely what skips the intermediate loads.
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
                    // A coalescable nav action extends the open run when it matches in kind
                    // and sign, else flushes and starts a fresh run — one arm for both kinds
                    // so the coalescing rule can't drift between them.
                    KeyOutcome::Action(action) if coalescable(action).is_some() => {
                        let Some((kind, delta)) = coalescable(action) else {
                            continue; // unreachable: the guard just matched
                        };
                        match &mut run {
                            Some((k, acc)) if *k == kind && acc.signum() == delta.signum() => {
                                *acc += delta;
                            }
                            _ => {
                                flush_run(app, &mut run);
                                run = Some((kind, delta));
                            }
                        }
                    }
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

/// The acquired terminal: raw mode on, alternate screen entered, panic hook installed.
///
/// Owning this as a value (rather than the old take-the-terminal-inside-`run` flow) is what lets
/// `main` show a splash frame BEFORE changeset acquisition — the terminal is live from the first
/// milliseconds of the launch, so resolve/diff work happens behind visible feedback instead of a
/// dead prompt. Restoration is idempotent and runs on [`Tui::restore`] or on drop, so every early
/// exit from `main` — "nothing to review", a `?`-propagated acquisition error — puts the shell
/// back before anything is printed to it.
pub struct Tui {
    terminal: Terminal<CrosstermBackend<Box<dyn Write>>>,
    restored: bool,
}

impl Tui {
    /// Take over the terminal now: install the panic hook, enable raw mode, enter the alternate
    /// screen. Call this before any slow launch work so [`Tui::splash`] can show it.
    pub fn acquire() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        let mut out = terminal_writer();
        execute!(out, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(out);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    /// Draw a one-line launch-activity frame (e.g. `resolving changesets…`). Deliberately
    /// theme-free (`DIM` modifier, no palette colors): it renders before the theme is resolved —
    /// resolving the theme first would put the up-to-800ms `theme=auto` terminal probe back in
    /// front of the first visible frame, defeating the point.
    pub fn splash(&mut self, msg: &str) -> io::Result<()> {
        self.terminal.draw(|f| draw_splash(f, msg))?;
        Ok(())
    }

    /// Run the main loop against `app`, then restore the terminal. Callers must have already
    /// called `app.open_current()` — under CS4's deferred-load mode (`app.set_defer_loads(true)`,
    /// `main.rs`'s default) that call marks the open PENDING rather than loading eagerly, so the
    /// first frame shows CS4's placeholder for one `OPEN_DEBOUNCE` window instead of blocking on
    /// the initial file's load; a caller that never turned defer mode on gets eager behavior.
    ///
    /// Spawns the ADR-037 input thread here — after the terminal is fully acquired (`self` already
    /// exists, so raw mode and the alternate screen are live) and after every earlier tty
    /// consumer (`main.rs`'s theme probe and its stray-input flush) has already run, since those
    /// must own the tty before crossterm's event stream has a reader racing them. The thread is
    /// never joined: when `run` returns, `main` returns, and the process takes it down (ADR-037's
    /// kill-on-exit lifecycle — the input thread never writes, so this can't corrupt anything).
    pub fn run(&mut self, app: &mut App, keymap: &Keymap, theme: &Palette) -> io::Result<()> {
        let inbox = spawn_input_thread();
        let result = event_loop(&mut self.terminal, app, keymap, theme, &inbox);
        let restored = self.restore();
        result.and(restored)
    }

    /// Put the terminal back (raw mode off, leave the alternate screen, cursor shown). Idempotent
    /// — a second call (including the one [`Drop`] always makes) is a no-op, so explicit callers
    /// (the "nothing to review" exit, which must restore BEFORE its `eprintln`) and the drop
    /// backstop coexist without double-restoring.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()
    }
}

impl Drop for Tui {
    /// Backstop restore for every exit path that doesn't call [`Tui::restore`] explicitly — most
    /// importantly `main`'s `?` returns between `acquire` and `run`, whose errors miette prints
    /// only after locals drop; without this they would print into the alternate screen.
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Render the splash frame's widget tree — split from [`Tui::splash`] so tests can drive it
/// against a `TestBackend` frame without acquiring a real terminal.
fn draw_splash(frame: &mut Frame<'_>, msg: &str) {
    let para = Paragraph::new(msg).style(Style::default().add_modifier(Modifier::DIM));
    frame.render_widget(para, frame.area());
}

/// CS4's input-idle window: how long the loop waits with no new input before running a pending
/// deferred file open. Long enough that held-key autorepeat (~30-90ms between events on most
/// terminals) usually keeps re-arming the debounce and deferring the load past the whole burst;
/// short enough that releasing the key feels instant rather than laggy. Tunable if either edge
/// proves wrong in practice — there is nothing else load-bearing about this exact number.
const OPEN_DEBOUNCE: Duration = Duration::from_millis(80);

fn event_loop<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    app: &mut App,
    keymap: &Keymap,
    theme: &Palette,
    inbox: &mpsc::Receiver<InboxMessage>,
) -> io::Result<()> {
    let mut pending: Vec<KeyPress> = Vec::new();
    let mut quit = false;

    loop {
        terminal.draw(|f| render::render(f, app, keymap, theme))?;

        if quit {
            return Ok(());
        }

        // While an open is pending, wait on the short debounce window instead of the regular
        // 200ms redraw beat, so the deferred load runs promptly once input goes quiet — a plain
        // timeout (no new inbox message) is what "quiet" means here. This borrows the same
        // `Tick` beat the M4 index watcher already polls on (see the module doc); the watcher
        // occasionally running ~120ms early during a debounce window is harmless (its own doc
        // comment already tolerates an "unseen" signature settling one tick late).
        let timeout = if app.open_pending() {
            OPEN_DEBOUNCE
        } else {
            Duration::from_millis(200)
        };

        let event = recv_event(inbox, timeout)?;
        if matches!(event, AppEvent::Tick) && app.open_pending() {
            app.complete_pending_open();
        }
        let mut batch = vec![event];
        drain_pending(inbox, &mut batch)?;
        quit = update_batch(app, keymap, &mut pending, batch);
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

    // ── ADR-037: input thread's pure mapping + inbox draining ──────────────────

    #[test]
    fn map_terminal_event_maps_key_press_and_resize() {
        assert_eq!(
            map_terminal_event(Event::Key(key(KeyCode::Char('q')))),
            Some(AppEvent::Key(key(KeyCode::Char('q'))))
        );
        assert_eq!(
            map_terminal_event(Event::Resize(80, 24)),
            Some(AppEvent::Resize(80, 24))
        );
    }

    #[test]
    fn map_terminal_event_skips_release_repeat_mouse_paste_and_focus() {
        use crossterm::event::{KeyEventState, MouseButton, MouseEvent, MouseEventKind};

        let release = KeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(map_terminal_event(Event::Key(release)), None);

        let repeat = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
            KeyEventState::NONE,
        );
        assert_eq!(map_terminal_event(Event::Key(repeat)), None);

        assert_eq!(
            map_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })),
            None
        );
        assert_eq!(map_terminal_event(Event::Paste("pasted".to_string())), None);
        assert_eq!(map_terminal_event(Event::FocusGained), None);
        assert_eq!(map_terminal_event(Event::FocusLost), None);
        let _ = MouseButton::Left; // silence an unused-import lint if MouseButton goes unused above
    }

    #[test]
    fn recv_event_yields_tick_on_a_plain_timeout() {
        let (_tx, rx) = mpsc::channel::<InboxMessage>();
        let event = recv_event(&rx, Duration::from_millis(5)).expect("timeout is not an error");
        assert_eq!(event, AppEvent::Tick);
    }

    #[test]
    fn recv_event_forwards_a_sent_event_before_the_timeout() {
        let (tx, rx) = mpsc::channel::<InboxMessage>();
        tx.send(Ok(AppEvent::Key(key(KeyCode::Char('q'))))).unwrap();
        let event = recv_event(&rx, Duration::from_secs(1)).unwrap();
        assert_eq!(event, AppEvent::Key(key(KeyCode::Char('q'))));
    }

    #[test]
    fn recv_event_propagates_a_forwarded_read_error() {
        let (tx, rx) = mpsc::channel::<InboxMessage>();
        tx.send(Err(io::Error::other("read failed"))).unwrap();
        let err = recv_event(&rx, Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.to_string(), "read failed");
    }

    #[test]
    fn recv_event_errors_when_the_inbox_disconnects_instead_of_spinning() {
        let (tx, rx) = mpsc::channel::<InboxMessage>();
        drop(tx);
        let result = recv_event(&rx, Duration::from_millis(5));
        assert!(
            result.is_err(),
            "a disconnected inbox must surface as an error, not a Tick"
        );
    }

    #[test]
    fn drain_pending_collects_everything_immediately_available_without_a_tick() {
        let (tx, rx) = mpsc::channel::<InboxMessage>();
        tx.send(Ok(AppEvent::Key(key(KeyCode::Char('a'))))).unwrap();
        tx.send(Ok(AppEvent::Key(key(KeyCode::Char('b'))))).unwrap();
        let mut batch = Vec::new();
        drain_pending(&rx, &mut batch).unwrap();
        assert_eq!(
            batch,
            vec![
                AppEvent::Key(key(KeyCode::Char('a'))),
                AppEvent::Key(key(KeyCode::Char('b'))),
            ]
        );
    }

    #[test]
    fn drain_pending_stops_on_an_empty_inbox_without_fabricating_a_tick() {
        let (_tx, rx) = mpsc::channel::<InboxMessage>();
        let mut batch = Vec::new();
        drain_pending(&rx, &mut batch).unwrap();
        assert!(
            batch.is_empty(),
            "an empty inbox must not inject a spurious Tick"
        );
    }

    #[test]
    fn drain_pending_propagates_a_forwarded_read_error() {
        let (tx, rx) = mpsc::channel::<InboxMessage>();
        tx.send(Err(io::Error::other("read failed"))).unwrap();
        let mut batch = Vec::new();
        let err = drain_pending(&rx, &mut batch).unwrap_err();
        assert_eq!(err.to_string(), "read failed");
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

    // ── CS4: idle-deferred loads ──────────────────────────────────────────────

    #[test]
    fn deferred_outline_burst_loads_nothing_until_completed() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::Role;
        use workon_review::outline::OutlineMode;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = many_files_app(&fixture, 5);
        // `many_files_app` opens eagerly (defer mode isn't on yet) — file 0 is loaded before we
        // flip the switch, exactly like a real session's startup open would be under CS4 (see
        // `main.rs`, which turns defer mode on before its own initial `open_current`).
        app.set_defer_loads(true);
        app.set_outline_mode(OutlineMode::Flat);
        app.toggle_outline(); // open + focus, cursor synced onto file 0's row

        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        let events = vec![
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
            AppEvent::Key(key(KeyCode::Char('j'))),
        ];

        let quit = update_batch(&mut app, &km, &mut pending, events);

        assert!(!quit);
        assert_eq!(app.current, 4, "the outline jump still lands on file 4");
        assert!(
            app.open_pending(),
            "landing on file 4 in defer mode must mark the open pending, not load it"
        );
        for f in 1..=4 {
            assert!(
                app.role_view_ref(f, Role::Combined).is_none(),
                "file {f} must not be loaded — not even the landing file, until completed"
            );
        }

        app.complete_pending_open();

        assert!(!app.open_pending());
        assert!(
            app.role_view_ref(4, Role::Combined).is_some(),
            "completing the pending open loads only the landing file"
        );
    }

    #[test]
    fn force_completion_before_move_lets_stage_hit_the_eager_hunk() {
        use git_workon_fixture::prelude::*;

        // Twin fixtures with identical content: one driven through defer mode (open_current
        // defers, `j` must force-complete before moving, then `s` stages), the other through
        // today's eager path — both must end up staging the exact same hunk.
        let fixture_deferred = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let fixture_eager = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut app_deferred = app_from_fixture(&fixture_deferred);
        app_deferred.set_defer_loads(true);
        app_deferred.open_current();
        assert!(
            app_deferred.open_pending(),
            "open_current in defer mode must not load eagerly"
        );

        let mut app_eager = app_from_fixture(&fixture_eager);
        app_eager.open_current();

        let km = Keymap::defaults();
        let mut pending_deferred: Vec<KeyPress> = Vec::new();
        let mut pending_eager: Vec<KeyPress> = Vec::new();

        // `j`: in defer mode this must force-complete the pending open (loading the view and
        // re-deriving the cursor from the REAL first-hunk row) before applying the move — else
        // the move would apply against the `0`-fallback cursor `reset_panes` left behind.
        update(
            &mut app_deferred,
            &km,
            &mut pending_deferred,
            AppEvent::Key(key(KeyCode::Char('j'))),
        );
        assert!(
            !app_deferred.open_pending(),
            "MoveCursorBy must force-complete the pending open"
        );
        update(
            &mut app_eager,
            &km,
            &mut pending_eager,
            AppEvent::Key(key(KeyCode::Char('j'))),
        );
        assert_eq!(
            app_deferred.cursor, app_eager.cursor,
            "post-completion cursor must match the eager path's cursor exactly"
        );

        // `s`: stages whatever hunk the (now-correct) cursor resolves to.
        update(
            &mut app_deferred,
            &km,
            &mut pending_deferred,
            AppEvent::Key(key(KeyCode::Char('s'))),
        );
        update(
            &mut app_eager,
            &km,
            &mut pending_eager,
            AppEvent::Key(key(KeyCode::Char('s'))),
        );

        let repo_deferred = fixture_deferred.repo().unwrap();
        let repo_eager = fixture_eager.repo().unwrap();
        repo_deferred.assert(predicate::repo::has_staged_file("a.txt"));
        repo_eager.assert(predicate::repo::has_staged_file("a.txt"));
    }

    // ── CS5: launch splash ────────────────────────────────────────────────────

    #[test]
    fn splash_renders_the_message() {
        let backend = ratatui::backend::TestBackend::new(40, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_splash(f, "resolving changesets…"))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let top_row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect();
        assert!(
            top_row.contains("resolving changesets…"),
            "splash frame must show the launch-activity message, got: {top_row:?}"
        );
    }
}
