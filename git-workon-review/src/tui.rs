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
//!
//! CS10 turns the mouse on: [`Tui::acquire`] enables capture for the whole session (undone by
//! [`Tui::restore`] and, unconditionally, the panic hook), and [`map_terminal_event`] maps a
//! left-click or wheel-scroll into an [`AppEvent::Mouse`] the loop dispatches to
//! [`workon_review::app::App::handle_click`]/[`workon_review::app::App::handle_wheel`] — every
//! other mouse kind (drag, move, non-left buttons, button-up) is still dropped, same as key
//! release/repeat.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use git2::Repository;
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use workon::Changeset;
use workon_review::acquire::{diff_changeset, ChangesetDiff};
use workon_review::app::{self, App, FileLoadSpec, LoadedViews, Severity};
use workon_review::config;
use workon_review::highlight::TsHighlighter;
use workon_review::keymap::{Command, Dispatch, KeyPress, Keymap};
use workon_review::render;
use workon_review::theme::{Palette, PaletteContext};

/// One event the review loop reacts to. `Tick` is synthesized by the main loop on an inbox
/// `recv_timeout` timeout — it is never sent through the channel itself (see [`recv_event`]).
/// `Key`/`Resize` are forwarded from the input thread via [`map_terminal_event`]; `FileReady` is
/// forwarded from the loader thread via [`run_load_job`]. Not `Copy`/`Clone`/`PartialEq`/`Eq`
/// (ADR-037): `FileReady`'s payload carries [`LoadedViews`], which wraps
/// [`workon_review::app::FileView`] — a type with none of those (its highlight/word-diff caches
/// don't implement them, and rebuilding one is cheap enough that nothing has ever needed to).
#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    /// A left-click or wheel-scroll (CS10) — the only [`MouseEventKind`]s [`map_terminal_event`]
    /// maps; drag, move, non-left buttons, and up events are dropped at the mapping step, exactly
    /// like key release/repeat.
    Mouse(MouseEvent),
    Tick,
    /// One [`LoadRequest`]'s result — ADR-037's loader-result variant. `gen`/`cs_idx`/`file_idx`
    /// echo the request's stamp; `result` is `Err` for a job that panicked or otherwise failed
    /// (see [`run_load_job`]'s doc comment for why a footer notice, not a new AppEvent shape, is
    /// how that surfaces). Applied at ONE chokepoint: [`App::apply_file_ready`].
    FileReady {
        gen: u64,
        cs_idx: usize,
        file_idx: usize,
        result: Result<LoadedViews, String>,
    },
    /// One changeset's streamed-diff result — ADR-037's streamed-launch counterpart to
    /// `FileReady`, forwarded from the wave thread [`spawn_wave_thread`] spawns. `gen`/`idx` echo
    /// the wave's stamp/the changeset's position in `App`'s stack; `result` is `Err` for a
    /// changeset whose diff itself failed (a bad/garbage `Oid`, not a job panic — see
    /// [`spawn_wave_thread`]'s doc comment). Applied at ONE chokepoint:
    /// [`App::apply_changeset_ready`].
    ChangesetReady {
        gen: u64,
        idx: usize,
        result: Result<ChangesetDiff, String>,
    },
}

impl PartialEq for AppEvent {
    /// Manual, deliberately PARTIAL equality (can't derive — `FileReady`'s `LoadedViews` payload
    /// isn't `PartialEq`, see the enum's doc comment): `Key`/`Resize`/`Mouse`/`Tick` compare
    /// structurally, exactly like the pre-ADR-037 derive did, for the input-thread tests that
    /// still assert mapped-event shape via `assert_eq!`. Two `FileReady` events are never
    /// considered equal — there's no sound definition of "the same loader result" once `FileView`
    /// can't be compared, and nothing needs one; tests that care about a `FileReady`'s fields
    /// match on them directly. Every fully-comparable variant needs its own arm here: the
    /// `_ => false` catch-all exists ONLY for `FileReady`/`ChangesetReady`, and letting a
    /// comparable variant fall into it silently breaks reflexivity (`Mouse` did exactly that
    /// when CS10 first added it — crossterm's `MouseEvent` derives `PartialEq` fine).
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (AppEvent::Key(a), AppEvent::Key(b)) => a == b,
            (AppEvent::Resize(w1, h1), AppEvent::Resize(w2, h2)) => w1 == w2 && h1 == h2,
            (AppEvent::Mouse(a), AppEvent::Mouse(b)) => a == b,
            (AppEvent::Tick, AppEvent::Tick) => true,
            _ => false,
        }
    }
}

/// The inbox message type: a mapped terminal event, or the input thread's terminal `event::read`
/// error forwarded verbatim (ADR-037: "the input thread never exits silently" — a read error is
/// still observable, just relayed rather than swallowed). `Tick` never appears here.
type InboxMessage = io::Result<AppEvent>;

/// Map one crossterm terminal [`Event`] to the [`AppEvent`] the loop reacts to — key-press,
/// resize, and (CS10, extended by the mouse h-wheel follow-up) a left-click or vertical/
/// horizontal wheel-scroll map; key release/repeat, every other mouse kind (drag, move, non-left
/// buttons, button-up), paste, and focus events are skipped (`None`). Pure and independent of any
/// thread or channel, so it's unit-tested directly; the input thread's loop body is a thin wrapper
/// around it.
fn map_terminal_event(event: Event) -> Option<AppEvent> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(AppEvent::Key(key)),
        Event::Resize(w, h) => Some(AppEvent::Resize(w, h)),
        Event::Mouse(m)
            if matches!(
                m.kind,
                MouseEventKind::Down(MouseButton::Left)
                    | MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight
            ) =>
        {
            Some(AppEvent::Mouse(m))
        }
        _ => None,
    }
}

/// Spawn the dedicated input thread against an already-built inbox sender. Must be called AFTER
/// the terminal is acquired and any pre-takeover tty work (the theme probe, stray-input flush)
/// has finished — crossterm input must not be consumed before that ordering completes (see
/// `main.rs`'s block comment on the resolve/probe/acquire sequence). The thread loops forever on
/// a blocking `event::read()`, forwarding mapped events; on a read error it forwards the error
/// once and exits — the sole way this thread ever stops short of the process dying. Never joined:
/// [`Tui::run`] returns without waiting for it (ADR-037's kill-on-exit lifecycle — the input
/// thread, like the loader thread, never writes, so an abandoned read can't corrupt anything).
///
/// `tx` is a clone of the SAME inbox sender the loader thread also holds (ADR-037's "one inbox" —
/// [`Tui::run`] builds the channel once and hands a clone to each producer thread), so both
/// threads' events interleave into a single `recv_event`/`drain_pending` stream.
fn spawn_input_thread(tx: mpsc::Sender<InboxMessage>) {
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
}

/// One file-load request handed to the loader thread (ADR-037's "Protocol": the loader is
/// stateless between jobs — everything a job needs rides along on the request). `gen`/`cs_idx`/
/// `file_idx` are stamped at send time from [`App::take_pending_load_spec`]'s return and echoed
/// back verbatim on the [`AppEvent::FileReady`] result, so [`App::apply_file_ready`] can apply
/// (or drop) it without the loader ever touching `App`.
struct LoadRequest {
    gen: u64,
    cs_idx: usize,
    file_idx: usize,
    spec: FileLoadSpec,
}

/// Extract a human-readable message from a `catch_unwind` panic payload — the common `&str`/
/// `String` panic-message shapes get their text; anything else (a panic with a non-string
/// payload) falls back to a generic message rather than failing to report at all.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "loader job panicked".to_string()
    }
}

/// The ADR-037 loader job's pure body: `LoadRequest -> AppEvent`, unit-tested directly (no
/// threads) against a fixture repo + highlighter. Wrapped in `catch_unwind` per the ADR's
/// "Lifecycle" decision — the specific failure mode this catches that nothing else does: a
/// panicked job would otherwise silently drop into a slot stranded `Pending` forever (the file
/// never re-requested, since [`App::open_pending_dispatched`]'s guard already marked it sent), an
/// invisible hang instead of a visible error.
///
/// A panic's message surfaces through [`AppEvent::FileReady`]'s `Err` arm, which
/// [`App::apply_file_ready`] turns into a footer notice — a footer notice, not a new per-file
/// `Failed` slot, is the shape chosen here (see this changeset's report): it's visible, it
/// doesn't strand `open_pending`, and correctness never depended on the loader succeeding in the
/// first place (the force-completion sync fallback is where correctness actually lives).
fn run_load_job(repo: &Repository, ts: &mut TsHighlighter, req: LoadRequest) -> AppEvent {
    let LoadRequest {
        gen,
        cs_idx,
        file_idx,
        spec,
    } = req;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app::build_file_views(repo, ts, &spec)
    }))
    .map_err(panic_message);
    AppEvent::FileReady {
        gen,
        cs_idx,
        file_idx,
        result,
    }
}

/// Spawn the ADR-037 loader thread: it owns its own long-lived `Repository` + `TsHighlighter`
/// (never `App`'s — the loader is a separate thread and can't touch `App`'s handles) and serves
/// [`LoadRequest`]s sequentially off `req_rx`, forwarding each job's [`AppEvent::FileReady`] into
/// the shared inbox via `tx` (a clone of the same sender the input thread holds). Returns the
/// `Sender` half the main loop dispatches requests through.
///
/// If `repo_path` can't be opened here (should be unreachable — `main.rs` already opened it once
/// to build `App`), the thread exits immediately without serving anything: every subsequent
/// dispatch attempt just accumulates in `req_rx`'s buffer until the main loop's `send` starts
/// erroring, which is harmless (the force-completion sync fallback is what correctness actually
/// depends on — see [`run_load_job`]'s doc comment). Never joined — same kill-on-exit lifecycle as
/// the input thread.
fn spawn_loader_thread(
    repo_path: PathBuf,
    tx: mpsc::Sender<InboxMessage>,
) -> mpsc::Sender<LoadRequest> {
    let (req_tx, req_rx) = mpsc::channel::<LoadRequest>();
    thread::spawn(move || {
        let Ok(repo) = Repository::open(&repo_path) else {
            return;
        };
        let mut ts = TsHighlighter::new();
        for req in req_rx {
            let event = run_load_job(&repo, &mut ts, req);
            if tx.send(Ok(event)).is_err() {
                return; // main loop is gone; nothing left to forward to
            }
        }
    });
    req_tx
}

/// Spawn a ADR-037 diff wave — the startup wave over the whole resolved stack, or (ADR-037
/// "Refresh") a refresh's span-keyed reuse leftovers, the changed/new committed spans
/// [`workon_review::app::App::take_pending_wave`] queued. Stripes `to_diff` (`current_idx`-first
/// if the active changeset is among the pairs being diffed, then input order) across
/// `available_parallelism`-many transient WORKER threads — same fan-out shape as
/// `crate::acquire::diff_changesets` (each worker opens its own `Repository`, since
/// `git2::Repository` is `Send` but not `Sync`) — but STREAM each result the instant it completes
/// via `tx` rather than joining the batch. Never joined itself either — a wave straggler left
/// running past quit (or superseded by a later refresh's generation) is harmless: it only ever
/// sends into an inbox nothing is listening to anymore, or a result [`App::apply_changeset_ready`]
/// drops outright on a generation mismatch; `tx.send` failing is the signal each worker already
/// checks for the former.
///
/// `to_diff`'s `usize` is the pair's index into `App`'s FULL changeset stack (not a position
/// within `to_diff` itself) — carried straight through to each `ChangesetReady { idx, .. }` so
/// [`App::apply_changeset_ready`] can seat the result without `App` and this wave ever agreeing
/// on a separate numbering.
///
/// A DELIBERATELY separate set of threads from the loader thread (ADR-037 leaves this shape
/// open — "yours to shape"): the wave never touches the loader's request queue, so an in-flight
/// wave can never starve a `LoadFile` request behind it — they run on entirely disjoint threads
/// with entirely disjoint work queues. The cost is a second family of `Repository` handles
/// (`workers + 1`, alongside the loader's one) alive for the wave's brief lifetime; accepted for
/// the starvation-freedom it buys for free.
///
/// A changeset whose own diff fails (a bad/garbage `Oid` — see [`diff_changeset`]'s doc comment)
/// sends `Err` for THAT changeset only; a worker whose own `Repository::open` fails sends `Err`
/// for every changeset in its chunk (mirroring `diff_changesets`' per-chunk failure shape) rather
/// than silently dropping them — every index must get exactly one result, or its slot stays
/// `Pending` forever with nothing left to complete it.
fn spawn_wave_thread(
    repo_path: PathBuf,
    tx: mpsc::Sender<InboxMessage>,
    to_diff: Vec<(usize, Changeset)>,
    gen: u64,
    current_idx: Option<usize>,
) {
    thread::spawn(move || {
        let n = to_diff.len();
        if n == 0 {
            return;
        }
        // The active changeset first (if it's among these pairs at all), then input order for
        // the rest — the changeset the user lands on becomes interactive earliest (ADR-037's
        // "Slots"). `current_pos` is a position WITHIN `to_diff`, not the stack index itself.
        let current_pos = current_idx.and_then(|ci| to_diff.iter().position(|(idx, _)| *idx == ci));
        let mut order: Vec<usize> = Vec::with_capacity(n);
        order.extend(current_pos);
        order.extend((0..n).filter(|&i| Some(i) != current_pos));

        let workers = thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
            .min(n);
        let chunk = n.div_ceil(workers.max(1));

        thread::scope(|scope| {
            for pos_chunk in order.chunks(chunk) {
                let tx = tx.clone();
                let to_diff = &to_diff;
                let repo_path = &repo_path;
                scope.spawn(move || {
                    let repo = match Repository::open(repo_path) {
                        Ok(repo) => repo,
                        Err(err) => {
                            let message = err.to_string();
                            for &pos in pos_chunk {
                                let (idx, _) = &to_diff[pos];
                                if tx
                                    .send(Ok(AppEvent::ChangesetReady {
                                        gen,
                                        idx: *idx,
                                        result: Err(message.clone()),
                                    }))
                                    .is_err()
                                {
                                    return; // main loop is gone
                                }
                            }
                            return;
                        }
                    };
                    for &pos in pos_chunk {
                        let (idx, cs) = &to_diff[pos];
                        let result = diff_changeset(&repo, cs).map_err(|e| e.to_string());
                        if tx
                            .send(Ok(AppEvent::ChangesetReady {
                                gen,
                                idx: *idx,
                                result,
                            }))
                            .is_err()
                        {
                            return; // main loop is gone; nothing left to forward to
                        }
                    }
                });
            }
        });
    });
}

/// The ADR-037 pipeline handles [`event_loop`] needs to dispatch off-thread work — bundled into
/// one struct (rather than four separate parameters) so `event_loop` stays under clippy's
/// `too_many_arguments`. `inbox` is the single shared receiver; `load_tx`/`wave_tx` dispatch to
/// the loader thread and a fresh diff-wave thread respectively; `repo_path` is what any
/// newly-spawned wave thread opens its own `Repository` handle against (a refresh can queue a
/// wave well after startup, so this is kept around for the whole loop, not just its setup).
#[derive(Clone, Copy)]
struct Pipeline<'a> {
    inbox: &'a mpsc::Receiver<InboxMessage>,
    load_tx: &'a mpsc::Sender<LoadRequest>,
    wave_tx: &'a mpsc::Sender<InboxMessage>,
    repo_path: &'a Path,
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
    ReloadConfig,
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
    ToggleMaximize,
    ToggleSplitFocus,
    Refresh,
    StageHunk,
    StageFile,
    DiscardHunk,
    DiscardFile,
    StartSelection,
    ExpandGap,
    ExpandGapAll,
    ResetGaps,
    ExpandAllGaps,
    HscrollLeft,
    HscrollRight,
    ToggleOutline,
    OutlineMoveBy(i64),
    OutlineConfirm,
    OutlineCycleMode,
    FocusOutline,
    FocusDiff,
    OutlineTop,
    OutlineBottom,
    OutlineStage,
    OutlineDiscard,
    OutlineHscrollLeft,
    OutlineHscrollRight,
    OutlineNextChangeset,
    OutlinePrevChangeset,
    OutlineCollapseAll,
    OutlineExpandAll,
    OutlineFilterFocus,
    SearchFocus,
    SearchNext,
    SearchPrev,
    CopyLines,
    CopyLocation,
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
        Command::ReloadConfig => Action::ReloadConfig,
        Command::CursorDown => Action::MoveCursorBy(1),
        Command::CursorUp => Action::MoveCursorBy(-1),
        Command::HalfPageDown => Action::MoveCursorBy(half_page),
        Command::HalfPageUp => Action::MoveCursorBy(-half_page),
        Command::ScrollTop => Action::ScrollTop,
        Command::ScrollBottom => Action::ScrollBottom,
        Command::ToggleLayout => Action::ToggleLayout,
        Command::ToggleMaximize => Action::ToggleMaximize,
        Command::ToggleSplitFocus => Action::ToggleSplitFocus,
        Command::Refresh => Action::Refresh,
        Command::StageHunk => Action::StageHunk,
        Command::StageFile => Action::StageFile,
        Command::DiscardHunk => Action::DiscardHunk,
        Command::DiscardFile => Action::DiscardFile,
        Command::StartSelection => Action::StartSelection,
        Command::ExpandGap => Action::ExpandGap,
        Command::ExpandGapAll => Action::ExpandGapAll,
        Command::ResetGaps => Action::ResetGaps,
        Command::ExpandAllGaps => Action::ExpandAllGaps,
        Command::HscrollLeft => Action::HscrollLeft,
        Command::HscrollRight => Action::HscrollRight,
        Command::Search => Action::SearchFocus,
        Command::SearchNext => Action::SearchNext,
        Command::SearchPrev => Action::SearchPrev,
        Command::CopyLines => Action::CopyLines,
        Command::CopyLocation => Action::CopyLocation,
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
        Command::FocusOutline => Action::FocusOutline,
        Command::FocusDiff => Action::FocusDiff,
        Command::OutlineTop => Action::OutlineTop,
        Command::OutlineBottom => Action::OutlineBottom,
        Command::OutlineStage => Action::OutlineStage,
        Command::OutlineDiscard => Action::OutlineDiscard,
        Command::OutlineHscrollLeft => Action::OutlineHscrollLeft,
        Command::OutlineHscrollRight => Action::OutlineHscrollRight,
        Command::OutlineNextChangeset => Action::OutlineNextChangeset,
        Command::OutlinePrevChangeset => Action::OutlinePrevChangeset,
        Command::OutlineCollapseAll => Action::OutlineCollapseAll,
        Command::OutlineExpandAll => Action::OutlineExpandAll,
        Command::OutlineFilter => Action::OutlineFilterFocus,
    }
}

/// Map one key press to an [`Action`] through the resolved [`Keymap`], given `pending` (the
/// in-flight multi-key sequence buffer — generalized from the old `]`/`[` bracket chord to ANY
/// bound sequence), the current pane height (for the half-page deltas), and whether the outline
/// pane currently has focus/is open.
///
/// Dispatch order:
/// 1. The keymap ([`Keymap::advance`]) consumes the key. A bound sequence fires its command; a
///    strict prefix reports [`Dispatch::Pending`] and holds the buffer for the next key; an
///    unrecognized suffix mid-sequence drops the buffer without re-processing (the old
///    bracket-drop behavior, now general).
/// 2. `Esc` stays HARDCODED (ADR-034: the whole `Esc`-precedence cascade is never routed through
///    the registry). Reached only as a fresh, otherwise-unbound key, it walks outward: the outline
///    having focus quits (same terminal leaf as `q`); otherwise, with the outline open, it focuses
///    the outline (home-base model: `h`/`FocusOutline`'s effect); otherwise it quits.
///
/// `outline_focused` selects the keymap's outline vs diff context; the global bindings (`q`/`o`)
/// are active in both, so `o` toggles and `q` quits from either pane.
fn map_key(
    keymap: &Keymap,
    pending: &mut Vec<KeyPress>,
    key: KeyEvent,
    pane_height: usize,
    outline_focused: bool,
    outline_open: bool,
) -> Action {
    match keymap.advance(outline_focused, pending, key) {
        Dispatch::Command(command) => command_to_action(command, pane_height),
        Dispatch::Pending => Action::None,
        Dispatch::Unmatched { mid_sequence } => {
            if !mid_sequence && key.code == KeyCode::Esc {
                if outline_focused {
                    Action::Quit
                } else if outline_open {
                    Action::FocusOutline
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
/// `PrevFile`, `NextChangeset`, `PrevChangeset`, `ToggleMaximize`, and the outline nav/confirm actions),
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
            | Action::ExpandGap
            | Action::ExpandGapAll
            | Action::ResetGaps
            | Action::ExpandAllGaps
            | Action::SearchNext
            | Action::SearchPrev
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
        Action::ReloadConfig => app.request_config_reload(),
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
        Action::ToggleMaximize => app.toggle_maximize(),
        Action::ToggleSplitFocus => app.toggle_split_focus(),
        Action::Refresh => app.coordinated_refresh(),
        Action::StageHunk => app.stage_hunk(),
        Action::StageFile => app.stage_file(),
        Action::DiscardHunk => app.discard_hunk(),
        Action::DiscardFile => app.discard_file(),
        Action::StartSelection => app.start_selection(),
        Action::ExpandGap => app.expand_gap_at_cursor(false),
        Action::ExpandGapAll => app.expand_gap_at_cursor(true),
        Action::ResetGaps => app.reset_gaps(),
        Action::ExpandAllGaps => app.expand_all_gaps(),
        Action::HscrollLeft => app.hscroll_left(),
        Action::HscrollRight => app.hscroll_right(),
        Action::ToggleOutline => app.toggle_outline(),
        Action::OutlineMoveBy(delta) => app.outline_move_by(delta),
        Action::OutlineConfirm => app.outline_confirm(),
        Action::OutlineCycleMode => app.outline_cycle_mode(),
        // `h`/`left` pans the diff back to column 0 first (mirroring the outline's own home
        // position) and only actually focuses the outline once there — see the handoff's locked
        // decision #2. Implemented here rather than in `App::focus_outline` itself, since that
        // method is also called from the outline toggle (`App::toggle_outline`) and the mouse
        // click/wheel paths (`App::handle_click`/`handle_wheel`), none of which should gain pan
        // behavior.
        Action::FocusOutline => {
            if app.hscroll > 0 {
                app.hscroll_left();
            } else {
                app.focus_outline();
            }
        }
        Action::FocusDiff => app.focus_diff(),
        Action::OutlineTop => app.outline_top(),
        Action::OutlineBottom => app.outline_bottom(),
        Action::OutlineStage => app.outline_stage(),
        Action::OutlineDiscard => app.outline_discard(),
        Action::OutlineHscrollLeft => app.outline_hscroll_left(),
        Action::OutlineHscrollRight => app.outline_hscroll_right(),
        Action::OutlineNextChangeset => app.outline_next_changeset(),
        Action::OutlinePrevChangeset => app.outline_prev_changeset(),
        Action::OutlineCollapseAll => app.outline_collapse_all(),
        Action::OutlineExpandAll => app.outline_expand_all(),
        Action::OutlineFilterFocus => app.outline_filter_focus(),
        Action::SearchFocus => app.search_focus(),
        Action::SearchNext => app.search_next(),
        Action::SearchPrev => app.search_prev(),
        Action::CopyLines => app.copy_lines(),
        Action::CopyLocation => app.copy_location(),
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

/// Resolve one `Key` event to a [`KeyOutcome`], given the caller has already ruled out the
/// modal cases (a pending discard confirm, the help overlay, the outline-filter input, the
/// search prompt) — this is cases 5-9 of `update`'s documented Esc-precedence cascade, extracted
/// so [`update`] and [`update_batch`] share the exact same resolution instead of duplicating it.
///
/// Clears any showing footer notice as a side effect, exactly like `update`'s cases 5-9 do (the
/// confirm, help, and prompt modals deliberately do not — that stays in their own arms, not
/// here).
fn resolve_key(
    app: &mut App,
    keymap: &Keymap,
    pending: &mut Vec<KeyPress>,
    key: KeyEvent,
) -> KeyOutcome {
    if key.code == KeyCode::Esc && !app.outline_focused() {
        if app.selection_anchor.is_some() {
            app.clear_notice();
            app.cancel_selection();
            return KeyOutcome::Handled;
        }
        // M11 CS3 (`diff-search`): Esc with an ACCEPTED search active (the prompt itself already
        // closed — see [`apply_search_input_key`]'s own Esc arm for the prompt-open case) clears
        // it, ranked in this same tier (before the outline-focused-quit/focus-outline arms below —
        // see `update`'s doc comment).
        if app.search_active() {
            app.clear_notice();
            app.search_clear();
            return KeyOutcome::Handled;
        }
    }
    // CS2 (outline-filter): with the outline focused (the input row does NOT have capture —
    // that's `update`'s case-3 modal arm) and a query actively narrowing the list, Esc unwinds
    // the filter instead of quitting — mirroring how the selection-Esc arm above unwinds the
    // diff's innermost mode before Esc's outer meanings apply. Only the NEXT Esc reaches the
    // outline's terminal quit leaf.
    if key.code == KeyCode::Esc && app.outline_focused() && !app.outline_filter_query().is_empty() {
        app.clear_notice();
        app.outline_filter_clear();
        return KeyOutcome::Handled;
    }
    app.clear_notice();
    KeyOutcome::Action(map_key(
        keymap,
        pending,
        key,
        app.pane_height,
        app.outline_focused(),
        app.outline_open(),
    ))
}

/// `update`'s case-3 modal arm: apply one key press while the CS2 outline-filter INPUT has
/// keyboard capture (see [`App::outline_filter_focused`]). Every branch calls straight into an
/// `App::outline_filter_*` method — no [`Action`]/[`map_key`] indirection, mirroring the
/// confirm/help modals' own direct `key.code` matches just above this arm's call site, rather
/// than routing through the rebindable [`Keymap`] (the filter input's editing keys are readline
/// muscle memory, not a rebindable action set, matching [`crate::prompt`]'s own module doc).
///
/// Ctrl-modified letters are checked before the plain-`Char` catch-all so `Ctrl-a`/`Ctrl-c`/
/// `Ctrl-e`/`Ctrl-n`/`Ctrl-p`/`Ctrl-u`/`Ctrl-w` never fall through and get inserted as literal
/// text. `Alt`-modified chars are excluded from the catch-all too (there is no bound behavior for
/// them here, and inserting an Alt-chorded char as plain text would be surprising).
/// One readline-style prompt edit, decoded from a key event by [`prompt_edit_for_key`] —
/// everything [`apply_filter_input_key`] and [`apply_search_input_key`] do that ISN'T specific to
/// which prompt is focused (their own Enter/Esc, and the filter's outline-list-navigation extras).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptEdit {
    InsertChar(char),
    Backspace,
    Delete,
    MoveLeft,
    MoveRight,
    MoveHome,
    MoveEnd,
    ClearToStart,
    DeleteWordBack,
}

/// Decode a key event into the [`PromptEdit`] it means, or `None` for a key neither prompt modal
/// arm handles (left for that arm's own extras, or unbound). Shared by
/// [`apply_filter_input_key`]/[`apply_search_input_key`] — previously two independent copies of
/// this same decode (ctrl-chord extraction, `Ctrl-a`/`e`/`u`/`w`, the plain-`Char` guard excluding
/// ctrl/alt, `Backspace`/`Delete`/`Left`/`Right`/`Home`/`End`).
fn prompt_edit_for_key(key: KeyEvent) -> Option<PromptEdit> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('a') if ctrl => Some(PromptEdit::MoveHome),
        KeyCode::Char('e') if ctrl => Some(PromptEdit::MoveEnd),
        KeyCode::Char('u') if ctrl => Some(PromptEdit::ClearToStart),
        KeyCode::Char('w') if ctrl => Some(PromptEdit::DeleteWordBack),
        KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(PromptEdit::InsertChar(c))
        }
        KeyCode::Backspace => Some(PromptEdit::Backspace),
        KeyCode::Delete => Some(PromptEdit::Delete),
        KeyCode::Left => Some(PromptEdit::MoveLeft),
        KeyCode::Right => Some(PromptEdit::MoveRight),
        KeyCode::Home => Some(PromptEdit::MoveHome),
        KeyCode::End => Some(PromptEdit::MoveEnd),
        _ => None,
    }
}

/// `update`'s case-3 modal arm: apply one key press while the CS2 outline-filter INPUT has
/// keyboard capture (see [`App::outline_filter_focused`]). Handles its own Enter/Esc and the
/// outline-list-navigation extras (`Ctrl-c`/`Ctrl-n`/`Ctrl-p`/`Down`/`Up`) directly, then
/// delegates every other key to [`prompt_edit_for_key`] — every branch calls straight into an
/// `App::outline_filter_*` method, no [`Action`]/[`map_key`] indirection, matching
/// [`crate::prompt`]'s own module doc that this is readline muscle memory, not a rebindable action
/// set.
fn apply_filter_input_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter | KeyCode::Esc => return app.outline_filter_unfocus(),
        KeyCode::Char('c') if ctrl => return app.outline_filter_clear(),
        KeyCode::Char('n') if ctrl => return app.outline_move_by(1),
        KeyCode::Char('p') if ctrl => return app.outline_move_by(-1),
        KeyCode::Down => return app.outline_move_by(1),
        KeyCode::Up => return app.outline_move_by(-1),
        _ => {}
    }
    match prompt_edit_for_key(key) {
        Some(PromptEdit::InsertChar(c)) => app.outline_filter_insert_char(c),
        Some(PromptEdit::Backspace) => app.outline_filter_backspace(),
        Some(PromptEdit::Delete) => app.outline_filter_delete(),
        Some(PromptEdit::MoveLeft) => app.outline_filter_move_left(),
        Some(PromptEdit::MoveRight) => app.outline_filter_move_right(),
        Some(PromptEdit::MoveHome) => app.outline_filter_move_home(),
        Some(PromptEdit::MoveEnd) => app.outline_filter_move_end(),
        Some(PromptEdit::ClearToStart) => app.outline_filter_clear_to_start(),
        Some(PromptEdit::DeleteWordBack) => app.outline_filter_delete_word_back(),
        None => {}
    }
}

/// `update`'s search-prompt modal arm (M11 CS3, `diff-search`): apply one key press while the
/// diff-view search prompt has keyboard capture (see [`App::search_focused`]). Mirrors
/// [`apply_filter_input_key`]'s shape (own Enter/Esc first, then [`prompt_edit_for_key`]) but
/// WITHOUT that arm's outline-list-navigation extras: the search prompt has no outline-list-
/// underneath to keep navigating while typing (the plan is explicit that typing previews
/// highlights but never moves the cursor), and `Esc` alone already covers "abandon this edit."
fn apply_search_input_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => return app.search_accept(),
        KeyCode::Esc => return app.search_abort(),
        _ => {}
    }
    match prompt_edit_for_key(key) {
        Some(PromptEdit::InsertChar(c)) => app.search_insert_char(c),
        Some(PromptEdit::Backspace) => app.search_backspace(),
        Some(PromptEdit::Delete) => app.search_delete(),
        Some(PromptEdit::MoveLeft) => app.search_move_left(),
        Some(PromptEdit::MoveRight) => app.search_move_right(),
        Some(PromptEdit::MoveHome) => app.search_move_home(),
        Some(PromptEdit::MoveEnd) => app.search_move_end(),
        Some(PromptEdit::ClearToStart) => app.search_clear_to_start(),
        Some(PromptEdit::DeleteWordBack) => app.search_delete_word_back(),
        None => {}
    }
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
/// CS2 outline-filter input having capture > the M11 CS3 search prompt having capture > an active
/// line selection OR an active search (diff-focused) > an active outline-filter query
/// (outline-focused) > the outline having focus > the diff having focus with the outline open >
/// the normal key map (where Esc quits). Concretely — the home-base model: the outline is where
/// Esc always eventually lands you before it quits, unwinding any inner mode (selection, search,
/// filter) along the way.
///
/// 1. A pending discard confirm captures the keyboard FIRST (before the notice clear and the
///    normal key map): `y` accepts, `n`/`Esc` cancels, and every other key is swallowed — a modal
///    that neither clears the notice nor runs a normal action while it's up.
/// 2. Otherwise, the help overlay (`?`) captures the keyboard next, mirroring the confirm modal's
///    swallow: `?`/`q`/`Esc` close it, every other key is a no-op (nothing on the diff behind it
///    reacts). Ranked just below the confirm modal — in practice the two are never up
///    together, since opening help doesn't run through a confirm, but the confirm winning keeps
///    a destructive prompt from ever being silently dismissed by a stray overlay key.
/// 3. Otherwise, the CS2 outline-filter INPUT (`/`, while it has capture — see
///    [`App::outline_filter_focused`]) captures next, mirroring the same swallow: typing/editing
///    keys reach [`crate::prompt::PromptState`], `Enter`/`Esc` return capture to the outline row
///    list KEEPING the query, `Ctrl-c` clears it and returns capture too, and `Down`/`Up`/
///    `Ctrl-n`/`Ctrl-p` move the outline selection without leaving the input. Ranked below help
///    (opening help while filtering isn't reachable today — `?` isn't part of the input's own key
///    set — but the ordering still says which would win if that ever changed) and above every
///    other case, since none of them should observe a key the filter input itself consumes.
/// 4. Otherwise, the M11 CS3 search prompt (`/` in the diff view, while it has capture — see
///    [`App::search_focused`]) captures next, mirroring the outline-filter input's swallow:
///    typing/editing keys reach [`crate::prompt::PromptState`] (live-previewing highlights, never
///    moving the cursor), `Enter` accepts and jumps, `Esc` aborts back to whatever search (or
///    none) was active before `/` was pressed. Ranked below the outline-filter input for the same
///    "can't actually collide today, but the ordering says who'd win" reason — the two prompts
///    can never both have capture (one requires outline focus, the other diff focus).
/// 5. Otherwise, with the diff focused, Esc CANCELS an active line selection OR clears an active
///    search (selection wins if, somehow, both are active) instead of moving focus or quitting
///    (`q` still quits). This arm is guarded to defer to case 7 when the outline has focus. Other
///    keys fall through to the normal map — `j`/`k` extend a selection, `n`/`N` step a search.
/// 6. Otherwise, with the outline focused and a NON-EMPTY filter query (capture on the row list,
///    not the input — that's case 3), Esc CLEARS the filter ([`App::outline_filter_clear`])
///    instead of quitting — the outline-side mirror of case 5's unwind-the-innermost-mode rule;
///    only the next Esc reaches case 7's quit leaf.
/// 7. Otherwise, while the outline pane has focus, Esc QUITS — same terminal leaf as `q`. The
///    outline is home base; there's nowhere further out to walk to.
/// 8. Otherwise, with the diff focused and the outline OPEN, Esc walks outward one step: it
///    focuses the outline (same effect as `h`/[`App::focus_outline`]) rather than quitting.
/// 9. Otherwise (diff focused, outline closed) the normal map applies, where Esc (like `q`) quits
///    — there's no outline to walk out to.
///
/// A `Key` event clears any showing footer notice before applying its own action (cases 5-9); the
/// confirm, help, and the two prompt modals (cases 1-4) deliberately do not. Cases 5-9 are
/// delegated to [`resolve_key`], shared with [`update_batch`].
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
        AppEvent::Key(key) if app.outline_filter_focused() => {
            apply_filter_input_key(app, key);
            false
        }
        AppEvent::Key(key) if app.search_focused() => {
            apply_search_input_key(app, key);
            false
        }
        AppEvent::Key(key) => match resolve_key(app, keymap, pending, key) {
            KeyOutcome::Handled => false,
            KeyOutcome::Action(action) => apply_action(app, action),
        },
        // CS10: all four modals swallow mouse input exactly like they swallow keys (cases 1-4
        // above) — a click/wheel while a discard confirm, the help overlay, the CS2 outline-filter
        // input, or the M11 CS3 search prompt is up does nothing.
        AppEvent::Mouse(_)
            if app.pending_confirm.is_some()
                || app.help_visible
                || app.outline_filter_focused()
                || app.search_focused() =>
        {
            false
        }
        AppEvent::Mouse(m) => {
            app.clear_notice();
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => app.handle_click(m.column, m.row),
                MouseEventKind::ScrollDown => app.handle_wheel(m.column, m.row, 3),
                MouseEventKind::ScrollUp => app.handle_wheel(m.column, m.row, -3),
                // 4 columns per tick — finer than `HSCROLL_STEP` since trackpads emit streams of
                // ticks (see `App::handle_hwheel`'s doc comment).
                MouseEventKind::ScrollRight => app.handle_hwheel(m.column, m.row, 4),
                MouseEventKind::ScrollLeft => app.handle_hwheel(m.column, m.row, -4),
                _ => {}
            }
            false
        }
        AppEvent::Tick => {
            app.on_tick();
            false
        }
        AppEvent::Resize(_, _) => false,
        AppEvent::FileReady {
            gen,
            cs_idx,
            file_idx,
            result,
        } => {
            app.apply_file_ready(gen, cs_idx, file_idx, result);
            false
        }
        AppEvent::ChangesetReady { gen, idx, result } => {
            app.apply_changeset_ready(gen, idx, result);
            false
        }
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
            // The coalescable path: no modal is up (CS2's outline-filter input and the M11 CS3
            // search prompt both included — a key while either has capture must reach
            // `apply_filter_input_key`/`apply_search_input_key` via the catch-all arm's `update`
            // delegation below, never `resolve_key`/the coalescing path), and this isn't the
            // selection-cancel/search-clear Esc guard (a context change — an "Esc cascade" — so it
            // falls to the catch-all arm below, which flushes first and delegates the whole event
            // to `update`). Notice-clearing still happens per key via `resolve_key`.
            AppEvent::Key(key)
                if app.pending_confirm.is_none()
                    && !app.help_visible
                    && !app.outline_filter_focused()
                    && !app.search_focused()
                    && !(key.code == KeyCode::Esc
                        && !app.outline_focused()
                        && (app.selection_anchor.is_some() || app.search_active())) =>
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
        // CS10: disable mouse capture unconditionally, same as `Tui::restore` — a stray disable
        // sequence when capture was never enabled (a panic before `Tui::acquire` reaches its own
        // `EnableMouseCapture`) is harmless, and there's no cheaper way from here to know whether
        // capture is currently on.
        let _ = execute!(out, DisableMouseCapture, LeaveAlternateScreen);
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
        // CS10: capture the mouse for the whole session — `map_terminal_event` only ever lets a
        // left-click or wheel-scroll through, so this doesn't cost the terminal's normal
        // text-selection UX beyond what most terminals' shift-click bypass already covers.
        execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
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
    /// first frame shows CS4's placeholder until the ADR-037 loader thread answers (or a
    /// force-completion chokepoint loads it synchronously first); a caller that never turned defer
    /// mode on gets eager behavior.
    ///
    /// `repo_path` opens the loader thread's OWN `Repository` handle — a second handle onto the
    /// same on-disk repo `app` already holds one of, exactly like `crate::acquire::diff_changesets`'s
    /// worker threads (`app` can't hand its handle across threads: `git2::Repository` is `Send`
    /// but not `Sync`).
    ///
    /// Builds the single ADR-037 inbox HERE and spawns both the input thread and the loader thread
    /// against clones of its sender — after the terminal is fully acquired (`self` already exists,
    /// so raw mode and the alternate screen are live) and after every earlier tty consumer
    /// (`main.rs`'s theme probe and its stray-input flush) has already run, since those must own
    /// the tty before crossterm's event stream has a reader racing them. Neither thread is joined:
    /// when `run` returns, `main` returns, and the process takes both down (ADR-037's kill-on-exit
    /// lifecycle — neither thread ever writes, so an abandoned one can't corrupt anything).
    ///
    /// `keymap`/`theme` are taken BY VALUE (not `&Keymap`/`&Palette`) — a `reload-config` request
    /// (`R`) needs to swap both mid-session, which needs owned locals `event_loop` can hold a
    /// `&mut` into; `palette_ctx` is what a reload re-resolves `theme = auto` against (see
    /// [`PaletteContext`]'s doc comment) rather than re-probing the terminal.
    pub fn run(
        &mut self,
        app: &mut App,
        mut keymap: Keymap,
        mut theme: Palette,
        repo_path: PathBuf,
        palette_ctx: &PaletteContext,
    ) -> io::Result<()> {
        let (tx, rx) = mpsc::channel::<InboxMessage>();
        spawn_input_thread(tx.clone());
        let load_tx = spawn_loader_thread(repo_path.clone(), tx.clone());
        let pipeline = Pipeline {
            inbox: &rx,
            load_tx: &load_tx,
            wave_tx: &tx,
            repo_path: &repo_path,
        };
        let result = event_loop(
            &mut self.terminal,
            app,
            &mut keymap,
            &mut theme,
            palette_ctx,
            &pipeline,
        );
        let restored = self.restore();
        result.and(restored)
    }

    /// ADR-037's streamed-launch counterpart to [`Self::run`]: for a stack of MORE than one
    /// changeset, `main.rs` calls this instead — `app` is already constructible from
    /// resolved-but-undiffed changesets (every slot `Pending`), and this is what starts the
    /// diffing itself, alongside the input/loader threads `run` always spawns. No splash: the
    /// first frame `event_loop` draws IS the live outline with `Pending` rows (see
    /// `main.rs`'s block comment on the `changesets.len()` fork).
    ///
    /// `changesets` is the SAME resolved list `app`'s `Pending` slots were built from — handed
    /// here (rather than re-read off `app`) since `App` only keeps [`workon_review::app::
    /// ChangesetView`]s, not the bare [`Changeset`]s the wave diffs against.
    pub fn run_streamed(
        &mut self,
        app: &mut App,
        mut keymap: Keymap,
        mut theme: Palette,
        repo_path: PathBuf,
        changesets: Vec<Changeset>,
        palette_ctx: &PaletteContext,
    ) -> io::Result<()> {
        let (tx, rx) = mpsc::channel::<InboxMessage>();
        spawn_input_thread(tx.clone());
        let load_tx = spawn_loader_thread(repo_path.clone(), tx.clone());
        // `App::from_changesets` (which built `app`'s all-`Pending` slots) picked `current_cs`
        // via the same lib-`current` lookup this enumeration mirrors, so `app.current_cs()` IS
        // that changeset's index into `to_diff` here — no separate lookup needed.
        let to_diff: Vec<(usize, Changeset)> = changesets.into_iter().enumerate().collect();
        spawn_wave_thread(
            repo_path.clone(),
            tx.clone(),
            to_diff,
            app.generation(),
            Some(app.current_cs()),
        );
        let pipeline = Pipeline {
            inbox: &rx,
            load_tx: &load_tx,
            wave_tx: &tx,
            repo_path: &repo_path,
        };
        let result = event_loop(
            &mut self.terminal,
            app,
            &mut keymap,
            &mut theme,
            palette_ctx,
            &pipeline,
        );
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
        // CS10: disable mouse capture before leaving the alternate screen — same ordering
        // convention as the raw-mode/alternate-screen pair, undone in the reverse order acquire
        // set them up in.
        execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        )?;
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
    keymap: &mut Keymap,
    theme: &mut Palette,
    palette_ctx: &PaletteContext,
    pipeline: &Pipeline<'_>,
) -> io::Result<()> {
    let Pipeline {
        inbox,
        load_tx,
        wave_tx,
        repo_path,
    } = *pipeline;
    let mut pending: Vec<KeyPress> = Vec::new();
    let mut quit = false;

    loop {
        terminal.draw(|f| render::render(f, app, keymap, theme))?;

        if quit {
            return Ok(());
        }

        // While an open is pending, wait on the short debounce window instead of the regular
        // 200ms redraw beat, so the deferred load's request goes out promptly once input goes
        // quiet — a plain timeout (no new inbox message) is what "quiet" means here. This borrows
        // the same `Tick` beat the M4 index watcher already polls on (see the module doc); the
        // watcher occasionally running ~120ms early during a debounce window is harmless (its own
        // doc comment already tolerates an "unseen" signature settling one tick late).
        let timeout = if app.open_pending() {
            OPEN_DEBOUNCE
        } else {
            Duration::from_millis(200)
        };

        let event = recv_event(inbox, timeout)?;
        // ADR-037: the debounce-fired deferred open is now an ASYNC `LoadFile` request rather
        // than a synchronous `complete_pending_open` — the placeholder keeps rendering until the
        // loader's `FileReady` result lands (or a force-completion chokepoint loads it
        // synchronously first, e.g. the user presses `j` before the loader answers).
        // `take_pending_load_spec` is idempotent across repeated debounce-window Ticks: it
        // returns `None` once a request has already gone out for the current pending open.
        if matches!(event, AppEvent::Tick) && app.open_pending() {
            if let Some((gen, cs_idx, file_idx, spec)) = app.take_pending_load_spec() {
                let _ = load_tx.send(LoadRequest {
                    gen,
                    cs_idx,
                    file_idx,
                    spec,
                });
            }
        }
        let mut batch = vec![event];
        drain_pending(inbox, &mut batch)?;
        quit = update_batch(app, keymap, &mut pending, batch);

        // ADR-037 "Refresh": every refresh trigger (`r`, the on-tick index watcher, a
        // post-staging drain) runs through `App::refresh` somewhere inside the `update_batch`
        // call above, however deeply nested — `App` itself never touches a thread, so it just
        // queues the span-keyed-reuse leftovers on `Self::pending_wave` for whoever's holding the
        // thread-spawning ability to pick up. This ONE checkpoint, run after every batch, is that
        // pickup: it covers every refresh trigger uniformly with no per-trigger wiring.
        if let Some((gen, to_diff)) = app.take_pending_wave() {
            spawn_wave_thread(
                repo_path.to_path_buf(),
                wave_tx.clone(),
                to_diff,
                gen,
                Some(app.current_cs()),
            );
        }

        // `reload-config` (`R`): re-read the whole `workon.review.*` tree through `App`'s own
        // repo handle and swap it into the keymap/palette the render/dispatch calls above already
        // hold `&mut` into — `App` itself flagged this via `request_config_reload` (it can't do
        // the swap itself, see that method's doc comment). The immutable `app.repo()` borrow ends
        // with `resolve_runtime`'s return, before `app` is touched mutably below.
        if app.take_config_reload_request() {
            let runtime = config::resolve_runtime(app.repo(), palette_ctx);
            *keymap = runtime.keymap;
            *theme = runtime.palette;
            // A half-entered chord against the OLD keymap is meaningless once the bindings under
            // it have changed.
            pending.clear();
            let view_warnings = app.reload_view_config(&runtime.view_config);
            let mut extra_warnings = runtime.warnings;
            extra_warnings.extend(view_warnings);
            // `crate::surface_warnings` merges the keymap/view-config/theme-override warnings the
            // same way `main.rs`'s `seat_app` does at startup and shows them as a notice. A
            // reload with nothing to warn about still owes the user a signal that it worked.
            if !crate::surface_warnings(app, keymap, extra_warnings) {
                app.notify("config reloaded", Severity::Info);
            }
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

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 5,
            row: 7,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn map_terminal_event_skips_release_repeat_paste_and_focus() {
        use crossterm::event::KeyEventState;

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

        assert_eq!(map_terminal_event(Event::Paste("pasted".to_string())), None);
        assert_eq!(map_terminal_event(Event::FocusGained), None);
        assert_eq!(map_terminal_event(Event::FocusLost), None);
    }

    /// CS10: `map_terminal_event` maps ONLY a left-click-down or a wheel-scroll to
    /// `AppEvent::Mouse`; every other mouse kind — drag, move, button-up, and non-left buttons —
    /// is still dropped, exactly like the pre-CS10 version dropped every mouse event outright.
    /// This supersedes the old `map_terminal_event_skips_release_repeat_mouse_paste_and_focus`
    /// pin (split above into the non-mouse skip cases, which are unchanged by CS10).
    #[test]
    fn map_terminal_event_maps_left_down_and_scroll_but_drops_other_mouse_kinds() {
        let left_down = mouse(MouseEventKind::Down(MouseButton::Left));
        assert!(matches!(
            map_terminal_event(Event::Mouse(left_down)),
            Some(AppEvent::Mouse(m)) if m == left_down
        ));

        let scroll_up = mouse(MouseEventKind::ScrollUp);
        assert!(matches!(
            map_terminal_event(Event::Mouse(scroll_up)),
            Some(AppEvent::Mouse(m)) if m == scroll_up
        ));

        let scroll_down = mouse(MouseEventKind::ScrollDown);
        assert!(matches!(
            map_terminal_event(Event::Mouse(scroll_down)),
            Some(AppEvent::Mouse(m)) if m == scroll_down
        ));

        // Dropped: drag, move, button-up, and a right-click-down.
        assert_eq!(
            map_terminal_event(Event::Mouse(mouse(MouseEventKind::Drag(MouseButton::Left)))),
            None
        );
        assert_eq!(
            map_terminal_event(Event::Mouse(mouse(MouseEventKind::Moved))),
            None
        );
        assert_eq!(
            map_terminal_event(Event::Mouse(mouse(MouseEventKind::Up(MouseButton::Left)))),
            None
        );
        assert_eq!(
            map_terminal_event(Event::Mouse(mouse(MouseEventKind::Down(
                MouseButton::Right
            )))),
            None
        );
    }

    /// Mouse h-wheel follow-up: `ScrollLeft`/`ScrollRight` (trackpad h-scroll, or a shift-wheel
    /// the terminal reports this way) map through exactly like the vertical `ScrollUp`/
    /// `ScrollDown` pair above.
    #[test]
    fn map_terminal_event_maps_scroll_left_and_right() {
        let scroll_left = mouse(MouseEventKind::ScrollLeft);
        assert!(matches!(
            map_terminal_event(Event::Mouse(scroll_left)),
            Some(AppEvent::Mouse(m)) if m == scroll_left
        ));

        let scroll_right = mouse(MouseEventKind::ScrollRight);
        assert!(matches!(
            map_terminal_event(Event::Mouse(scroll_right)),
            Some(AppEvent::Mouse(m)) if m == scroll_right
        ));
    }

    /// `AppEvent` dropped `PartialEq`/`Eq` in ADR-037 (`FileReady`'s `LoadedViews` payload wraps
    /// `FileView`, which has neither) — this test-only helper is the `matches!`-based replacement
    /// for the `assert_eq!(event, AppEvent::Key(key(...)))` shape used throughout this module's
    /// tests. Only compares `code`/`modifiers`/`kind` (what `key(...)`/`ctrl_key(...)` set), same
    /// fields a `PartialEq` derive on `KeyEvent` itself would have compared.
    fn is_key_event(event: &AppEvent, expected: KeyEvent) -> bool {
        matches!(event, AppEvent::Key(k) if *k == expected)
    }

    #[test]
    fn recv_event_yields_tick_on_a_plain_timeout() {
        let (_tx, rx) = mpsc::channel::<InboxMessage>();
        let event = recv_event(&rx, Duration::from_millis(5)).expect("timeout is not an error");
        assert!(matches!(event, AppEvent::Tick));
    }

    #[test]
    fn recv_event_forwards_a_sent_event_before_the_timeout() {
        let (tx, rx) = mpsc::channel::<InboxMessage>();
        tx.send(Ok(AppEvent::Key(key(KeyCode::Char('q'))))).unwrap();
        let event = recv_event(&rx, Duration::from_secs(1)).unwrap();
        assert!(is_key_event(&event, key(KeyCode::Char('q'))));
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
        assert_eq!(batch.len(), 2);
        assert!(is_key_event(&batch[0], key(KeyCode::Char('a'))));
        assert!(is_key_event(&batch[1], key(KeyCode::Char('b'))));
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
            map_key(&km, &mut pending, key(KeyCode::Char('q')), 20, false, false),
            Action::Quit
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Esc), 20, false, false),
            Action::Quit
        );
    }

    #[test]
    fn scroll_keys_map_by_one_line() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('j')), 20, false, false),
            Action::MoveCursorBy(1)
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Down), 20, false, false),
            Action::MoveCursorBy(1)
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('k')), 20, false, false),
            Action::MoveCursorBy(-1)
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Up), 20, false, false),
            Action::MoveCursorBy(-1)
        );
    }

    #[test]
    fn ctrl_d_u_scroll_by_half_the_pane_height() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, ctrl_key('d'), 21, false, false),
            Action::MoveCursorBy(10)
        );
        assert_eq!(
            map_key(&km, &mut pending, ctrl_key('u'), 21, false, false),
            Action::MoveCursorBy(-10)
        );
        // A pane height of 1 still scrolls by at least one line.
        assert_eq!(
            map_key(&km, &mut pending, ctrl_key('d'), 1, false, false),
            Action::MoveCursorBy(1)
        );
    }

    #[test]
    fn g_and_shift_g_map_to_top_and_bottom() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('g')), 20, false, false),
            Action::ScrollTop
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('G')), 20, false, false),
            Action::ScrollBottom
        );
    }

    #[test]
    fn g_and_shift_g_map_to_outline_top_and_bottom_when_outline_focused() {
        // CS2: `g`/`G` are bound per-view (`scroll-top`/`scroll-bottom` in both View::Diff and
        // View::Outline), so the SAME key must resolve to a different Action depending on which
        // pane has focus — outline-focused maps to the outline jump, not the diff scroll.
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('g')), 20, true, true),
            Action::OutlineTop
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('G')), 20, true, true),
            Action::OutlineBottom
        );
        // Diff-focused (`outline_focused = false`) still maps to the diff's own scroll actions,
        // even with the outline open — see `g_and_shift_g_map_to_top_and_bottom` above.
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('g')), 20, false, true),
            Action::ScrollTop
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('G')), 20, false, true),
            Action::ScrollBottom
        );
    }

    #[test]
    fn enter_and_shift_e_map_to_expand_gap_in_diff_context() {
        // CS8: `enter`/`E` are bound in View::Diff only (`expand-gap`/`expand-gap-all`) — Enter
        // stays `OutlineConfirm` when the outline has focus (see the next test).
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Enter), 20, false, false),
            Action::ExpandGap
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('E')), 20, false, false),
            Action::ExpandGapAll
        );
        // Still diff-scoped even with the outline open, as long as it isn't focused.
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Enter), 20, false, true),
            Action::ExpandGap
        );
    }

    #[test]
    fn enter_still_maps_to_outline_confirm_when_outline_focused() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Enter), 20, true, true),
            Action::OutlineConfirm
        );
    }

    #[test]
    fn shift_l_maps_to_toggle_layout() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('L')), 20, false, false),
            Action::ToggleLayout
        );
    }

    #[test]
    fn shift_z_and_w_map_to_toggle_maximize_and_split_focus() {
        // diff-fold-keys: `toggle-maximize` moved off bare `z` to `Z` — `z` now anchors the
        // `zM`/`zR` gap fold-all chords in this view (see `z_m_and_z_r_map_to_reset_and_expand_all_gaps`
        // below), and a bare-key binding can't coexist with a longer chord sharing its prefix.
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('Z')), 20, false, false),
            Action::ToggleMaximize
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('w')), 20, false, false),
            Action::ToggleSplitFocus
        );
    }

    #[test]
    fn z_m_and_z_r_map_to_reset_and_expand_all_gaps() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('z')), 20, false, false),
            Action::None,
            "the first key of a chord reports no action yet (Pending)"
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('M')), 20, false, false),
            Action::ResetGaps
        );

        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('z')), 20, false, false),
            Action::None
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('R')), 20, false, false),
            Action::ExpandAllGaps
        );
    }

    #[test]
    fn r_maps_to_refresh() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('r')), 20, false, false),
            Action::Refresh
        );
    }

    #[test]
    fn tab_and_backtab_map_to_file_nav() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Tab), 20, false, false),
            Action::NextFile
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::BackTab), 20, false, false),
            Action::PrevFile
        );
    }

    #[test]
    fn bracket_f_maps_to_file_nav() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char(']')), 20, false, false),
            Action::None
        );
        // The buffer holds the in-flight chord prefix (generalized from the old `Option<char>`).
        assert_eq!(pending, vec![KeyPress::from_event(key(KeyCode::Char(']')))]);
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('f')), 20, false, false),
            Action::NextFile
        );
        assert!(pending.is_empty());

        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('[')), 20, false, false),
            Action::None
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('f')), 20, false, false),
            Action::PrevFile
        );
    }

    #[test]
    fn bracket_h_maps_to_hunk_nav() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        map_key(&km, &mut pending, key(KeyCode::Char(']')), 20, false, false);
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('h')), 20, false, false),
            Action::NextHunk
        );

        map_key(&km, &mut pending, key(KeyCode::Char('[')), 20, false, false);
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('h')), 20, false, false),
            Action::PrevHunk
        );
    }

    #[test]
    fn unrecognized_bracket_suffix_drops_pending_without_side_effect() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        map_key(&km, &mut pending, key(KeyCode::Char(']')), 20, false, false);
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('x')), 20, false, false),
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

    // ── ADR-037: the loader job's pure body ──────────────────────────────────────

    #[test]
    fn panic_message_reads_a_str_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_message(payload), "boom");
    }

    #[test]
    fn panic_message_reads_a_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom".to_string());
        assert_eq!(panic_message(payload), "boom");
    }

    #[test]
    fn panic_message_falls_back_for_a_non_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(42_i32);
        assert_eq!(panic_message(payload), "loader job panicked");
    }

    #[test]
    fn run_load_job_result_matches_a_synchronous_ensure_loaded() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::Role;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\n", "one\nCHANGED\n")
            .build()
            .unwrap();

        let mut eager = app_from_fixture(&fixture);
        eager.ensure_loaded(0);
        let eager_view = eager.current_view_ref().expect("eager view loaded");
        let eager_old_text = eager_view.old_text().to_string();
        let eager_new_text = eager_view.new_text().to_string();

        // Same two-handle shape the real loader thread uses: `app`'s own repo builds the spec,
        // a SEPARATE repo + highlighter (standing in for `spawn_loader_thread`'s own) runs the
        // job.
        let mut app = app_from_fixture(&fixture);
        app.set_defer_loads(true);
        app.open_current();
        let (gen, cs_idx, file_idx, spec) = app
            .take_pending_load_spec()
            .expect("a fresh pending open has an undispatched spec");

        let repo = fixture.repo().unwrap();
        let loader_repo = Repository::open(repo.workdir().unwrap()).unwrap();
        let mut loader_ts = TsHighlighter::new();
        let event = run_load_job(
            &loader_repo,
            &mut loader_ts,
            LoadRequest {
                gen,
                cs_idx,
                file_idx,
                spec,
            },
        );

        let AppEvent::FileReady {
            gen: got_gen,
            cs_idx: got_cs_idx,
            file_idx: got_file_idx,
            result,
        } = event
        else {
            panic!("run_load_job must return a FileReady event");
        };
        assert_eq!(got_gen, gen);
        assert_eq!(got_cs_idx, cs_idx);
        assert_eq!(got_file_idx, file_idx);

        let LoadedViews::Single(role, Some(view)) = result.expect("job must not fail") else {
            panic!("expected a loaded single-role view");
        };
        assert_eq!(role, Role::Unstaged, "a.txt has only an unstaged change");
        assert_eq!(view.old_text(), eager_old_text);
        assert_eq!(view.new_text(), eager_new_text);
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
            map_key(&km, &mut pending, key(KeyCode::Char('s')), 20, false, false),
            Action::StageHunk
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('S')), 20, false, false),
            Action::StageFile
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('d')), 20, false, false),
            Action::DiscardHunk
        );
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('D')), 20, false, false),
            Action::DiscardFile
        );
        // Ctrl-d keeps its half-page meaning — the plain-`d` staging arm must not shadow it.
        assert_eq!(
            map_key(&km, &mut pending, ctrl_key('d'), 20, false, false),
            Action::MoveCursorBy(10)
        );
    }

    #[test]
    fn v_maps_to_start_selection() {
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(
            map_key(&km, &mut pending, key(KeyCode::Char('v')), 20, false, false),
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
    fn esc_cancels_a_selection_before_focusing_the_outline() {
        // Even with the outline open (so a bare Esc would otherwise walk out to it), an active
        // selection still wins — the outline-focus move is a lower-precedence fallback, not an
        // alternative to selection-cancel.
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        // Selection needs a stageable (uncommitted) change; a single changeset seeds the
        // outline closed, so open it and hand focus back to the diff.
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.toggle_outline();
        app.focus_diff();
        assert!(app.outline_open() && !app.outline_focused());
        app.start_selection();
        assert!(app.selection_anchor.is_some());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );

        assert!(!quit, "Esc must not quit while a selection is active");
        assert!(
            app.selection_anchor.is_none(),
            "Esc cancels the active selection first"
        );
        assert!(
            !app.outline_focused(),
            "the outline-focus move only happens on a LATER Esc, once the selection is gone"
        );
    }

    #[test]
    fn esc_ladder_search_prompt_then_active_search_then_outline_focus() {
        // M11 CS3 (`diff-search`): three Esc presses in sequence, each landing on the NEXT lower
        // tier of the ladder once the higher one no longer applies — the prompt-open case first
        // (Esc aborts the EDIT, keeping the accepted query and its highlights), then the
        // accepted-search-active case (Esc clears the search entirely), then the ordinary
        // diff-with-outline-open case (Esc walks out to the outline).
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\n", "one\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.toggle_outline();
        app.focus_diff();
        assert!(app.outline_open() && !app.outline_focused());

        // Seed an accepted search, then reopen the prompt and type a DIFFERENT, uncommitted edit.
        app.search_focus();
        app.search_insert_char('C'); // "CHANGED" — matches the new-side line
        app.search_accept();
        assert!(app.search_active());
        app.search_focus();
        app.search_insert_char('x');
        assert!(app.search_focused(), "precondition: prompt has capture");

        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        // 1. Esc while the prompt is focused: aborts the EDIT only.
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );
        assert!(!quit);
        assert!(!app.search_focused(), "the prompt must close");
        assert!(
            app.search_active(),
            "aborting the prompt must restore the previously ACCEPTED search, not clear it"
        );

        // 2. Esc with the search still active (diff focused): clears the search.
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );
        assert!(!quit);
        assert!(
            !app.search_active(),
            "this Esc must clear the active search"
        );
        assert!(
            !app.outline_focused(),
            "clearing the search must not ALSO walk out to the outline in the same keypress"
        );

        // 3. Esc with nothing left to clear: walks out to the outline, same as the pre-existing
        //    ladder's diff-focused/outline-open case.
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );
        assert!(!quit);
        assert!(
            app.outline_focused(),
            "with nothing higher-precedence left, Esc must fall through to focus-outline"
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

    /// CS10: a pending discard confirm swallows a mouse event exactly like it swallows a key —
    /// mirrors `pending_confirm_captures_y_and_n_and_ignores_other_keys` above. A click inside a
    /// live hit region must not move the cursor or resolve the confirm.
    #[test]
    fn pending_confirm_swallows_a_mouse_click() {
        use workon_review::app::{PendingOp, Region};

        let fixture = git_workon_fixture::prelude::FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "one\ntwo\nthree\n", "one\nCHANGED\nthree\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.pane_height = 10;
        app.hit_regions.single = Some(Region {
            x: 0,
            y: 0,
            w: 40,
            h: 10,
        });
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });
        let cursor_before = app.cursor;
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Left))),
        );

        assert!(!quit);
        assert!(
            app.pending_confirm.is_some(),
            "a mouse event must not resolve the confirm"
        );
        assert_eq!(
            app.cursor, cursor_before,
            "a swallowed click must not move the cursor"
        );
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
    fn o_key_is_a_pure_show_hide_toggle() {
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
        assert!(
            !app.outline_open() && !app.outline_focused(),
            "o from open+unfocused closes the pane"
        );

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
            !app.outline_open() && !app.outline_focused(),
            "o from open+focused closes the pane — the toggle only ever tracks visibility"
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
    fn h_from_the_diff_opens_and_focuses_a_closed_outline() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline(); // close
        assert!(!app.outline_open() && !app.outline_focused());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('h'))),
        );

        assert!(
            app.outline_open() && app.outline_focused(),
            "h from the diff with the outline closed opens AND focuses it"
        );
        let items = app.outline_items();
        assert!(
            matches!(
                items[app.outline_cursor()],
                workon_review::outline::OutlineItem::File { cs_idx, file_idx, .. }
                    if cs_idx == app.current_cs() && file_idx == app.current
            ),
            "opening via h syncs the outline cursor to the current diff position"
        );
    }

    #[test]
    fn h_from_the_diff_with_the_outline_already_open_focuses_without_moving_the_cursor() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline(); // close
        app.toggle_outline(); // open + focus, synced
        app.outline_move_by(-1); // manually reposition
                                 // Return focus to the diff without going through `o` (mirrors `l`), so the outline
                                 // stays open but the diff has keyboard focus.
        update(
            &mut app,
            &Keymap::defaults(),
            &mut Vec::new(),
            AppEvent::Key(key(KeyCode::Char('l'))),
        );
        assert!(app.outline_open() && !app.outline_focused());
        let cursor_before = app.outline_cursor();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('h'))),
        );

        assert!(app.outline_focused(), "h focuses the already-open outline");
        assert_eq!(
            app.outline_cursor(),
            cursor_before,
            "h on an already-open outline must not stomp a manually positioned cursor"
        );
    }

    #[test]
    fn l_from_the_outline_focuses_the_diff_and_leaves_the_outline_open() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline(); // close
        app.toggle_outline(); // open + focus
        assert!(app.outline_open() && app.outline_focused());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('l'))),
        );

        assert!(
            app.outline_open() && !app.outline_focused(),
            "l focuses the diff but leaves the outline open"
        );
    }

    #[test]
    fn esc_quits_while_the_outline_has_focus() {
        // Home-base model: the outline has nowhere further out to walk to, so Esc there is the
        // terminal leaf of the cascade — same as `q`.
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.focus_outline();
        assert!(app.outline_focused());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );

        assert!(quit, "Esc while the outline has focus quits, like q");
    }

    #[test]
    fn esc_focuses_the_outline_from_the_diff_when_open() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        // Default: open, unfocused (diff has focus).
        assert!(app.outline_open() && !app.outline_focused());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );

        assert!(
            !quit,
            "Esc must not quit when it can walk out to the outline instead"
        );
        assert!(
            app.outline_focused(),
            "Esc from the diff with the outline open focuses the outline"
        );
        assert!(app.outline_open(), "Esc must not close the pane");
    }

    #[test]
    fn esc_quits_from_the_diff_when_the_outline_is_closed() {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.toggle_outline(); // close
        assert!(!app.outline_open() && !app.outline_focused());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );

        assert!(
            quit,
            "Esc from the diff with the outline closed has nowhere to walk to, so it quits"
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
        // CS3: pin BaseFirst explicitly — this test exercises Enter's File-row jump + focus
        // return, which is orthogonal to display order, but the row offset below assumes the
        // base->head row layout.
        app.set_outline_order(workon_review::outline::OutlineOrder::BaseFirst);
        app.toggle_outline(); // close
        app.toggle_outline(); // open + focus, cursor synced onto cs-b's file row
        assert!(app.outline_focused());
        // Move the outline cursor onto cs-a's FILE row (rows, BaseFirst: [Header a, File a.txt,
        // Header b, File b.txt] — cursor starts at 3; -2 lands on File a.txt at row 1). CS5
        // (`outline-fold`) removed Enter's old header-jump behavior — see
        // `enter_on_a_header_row_toggles_fold_and_keeps_focus` below for that case — so this
        // keybinding-dispatch test needs a File row to still exercise a real jump+unfocus.
        app.outline_move_by(-2);
        assert_eq!(
            app.current_cs(),
            0,
            "sanity: the move itself already landed on cs-a's file (moving onto a File row \
             always jumps, per `outline_move_by`'s own contract) — Enter below re-confirms the \
             same jump through the real keybinding-dispatch path"
        );
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
            "Enter on a File row must (still) land on cs-a"
        );
        assert_eq!(app.current, 0, "...landing on its file");
        assert!(
            !app.outline_focused(),
            "Enter on a File row returns focus to the diff"
        );
    }

    #[test]
    fn enter_on_a_header_row_toggles_fold_and_keeps_focus() {
        // CS5 (`outline-fold`): Enter on a Header/Dir row no longer jumps+unfocuses — it toggles
        // that row's fold and deliberately keeps focus. This is the header-row counterpart to
        // `enter_confirms_an_outline_jump_and_returns_focus_to_the_diff` above, verified through
        // the same real keybinding-dispatch path (`update`/`map_key`), not a direct
        // `App::outline_confirm()` call.
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let mut app = two_committed_changesets_app(&fixture);
        app.set_outline_order(workon_review::outline::OutlineOrder::BaseFirst);
        app.toggle_outline(); // close
        app.toggle_outline(); // open + focus, cursor synced onto cs-b's file row
                              // Move the outline cursor onto cs-a's header row (rows, BaseFirst: [Header a, File a.txt,
                              // Header b, File b.txt] — cursor starts at 3; -3 lands on Header a at row 0, which never
                              // jumps).
        app.outline_move_by(-3);
        let before_cs = app.current_cs();
        let before_file = app.current;
        let rows_before = app.outline_items().len();

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
            before_cs,
            "Enter on a header must NOT jump the diff (CS5)"
        );
        assert_eq!(app.current, before_file);
        assert!(
            app.outline_focused(),
            "Enter on a header toggles its fold and keeps focus (CS5), rather than confirming a \
             jump"
        );
        assert!(
            app.outline_items().len() < rows_before,
            "cs-a's file row must now be hidden under its collapsed header"
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
        // `AppEvent` isn't `Clone` (ADR-037: `FileReady`'s payload wraps a non-`Clone`
        // `FileView`), so the batch/sequential runs each build their own copy of the same
        // three-key press sequence rather than sharing one `Vec` via `.clone()`.
        let build_events = || {
            vec![
                AppEvent::Key(key(KeyCode::Char('j'))),
                AppEvent::Key(key(KeyCode::Char('j'))),
                AppEvent::Key(key(KeyCode::Char('j'))),
            ]
        };

        update_batch(&mut app_batch, &km, &mut pending_batch, build_events());
        for event in build_events() {
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
        // See the sibling test above for why this builds two independent copies rather than
        // cloning one `Vec<AppEvent>`.
        let build_events = || {
            vec![
                AppEvent::Key(key(KeyCode::Char('j'))),
                AppEvent::Key(key(KeyCode::Char('j'))),
                AppEvent::Key(key(KeyCode::Char('j'))),
                AppEvent::Key(key(KeyCode::Char('k'))),
            ]
        };

        update_batch(&mut app_batch, &km, &mut pending_batch, build_events());
        for event in build_events() {
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
        let build_boundary_events = || {
            vec![
                AppEvent::Key(key(KeyCode::Char('k'))),
                AppEvent::Key(key(KeyCode::Char('j'))),
            ]
        };

        update_batch(
            &mut app_batch2,
            &km,
            &mut pending_batch2,
            build_boundary_events(),
        );
        for event in build_boundary_events() {
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
        let build_events = || {
            vec![
                AppEvent::Key(key(KeyCode::Char('j'))), // swallowed by the confirm modal
                AppEvent::Key(key(KeyCode::Char('n'))), // cancels the confirm
            ]
        };

        update_batch(&mut app_batch, &km, &mut pending_batch, build_events());
        for event in build_events() {
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

    // ── ADR-037: real-thread integration smoke ─────────────────────────────────

    /// ADR-037's Testing layer 3, part one: the ONE test in this module that spawns the REAL
    /// [`spawn_loader_thread`]/[`spawn_wave_thread`] against real `mpsc` channels — everything
    /// else in this file drives `update`/`update_batch` with synthetic events specifically to
    /// avoid real threads (see the ADR's "Testing" decision: layers 1-2 are thread-free by
    /// design). This is the one exception, confined here.
    ///
    /// Mirrors `main.rs`'s streamed-launch shape exactly: `App::from_changesets` over
    /// all-`Pending` slots, `set_defer_loads(true)`, `open_current()`, then the same
    /// `spawn_wave_thread` call `Tui::run_streamed` makes. From there this test plays the event
    /// loop's OWN role by hand — draining the shared inbox and routing `ChangesetReady`/
    /// `FileReady` through the exact chokepoints `update`'s match arms call
    /// (`App::apply_changeset_ready`/`App::apply_file_ready`), plus the same post-batch
    /// `take_pending_load_spec` dispatch `event_loop` runs on every idle tick while an open is
    /// pending — except here it's driven the instant the active changeset seats, not gated behind
    /// a real debounce `Tick`, since nothing here is racing real terminal input.
    ///
    /// Bounded by `recv_timeout` per receive (not a wall-clock test deadline): a wedged thread
    /// times out and fails loudly rather than hanging the suite, but a healthy run's actual
    /// duration is however long the real diff/load work takes — no sleeping, no fixed budget, so
    /// this stays load-tolerant enough to run unconditionally (unlike `pty_responsiveness.rs`'s
    /// `#[ignore]` siblings, which assert actual elapsed wall-clock time).
    #[test]
    fn real_threads_stream_a_wave_and_complete_a_deferred_file_open() {
        use git_workon_fixture::prelude::*;
        use workon::{Changeset, ChangesetSpan};
        use workon_review::app::ChangesetView;

        // A 4-changeset committed stack, each adding one file — the streamed-launch "multi-
        // changeset fixture stack" the plan calls for, deep enough that the wave has real
        // fan-out work (`spawn_wave_thread` stripes across `available_parallelism` workers) and
        // that the ACTIVE (last, `current: true`) changeset has a real, uncached file for the
        // deferred-open assertion.
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .build()
            .unwrap();
        let root = fixture
            .commit("main")
            .file("root.txt", "r\n")
            .create("root")
            .unwrap();
        let c1 = fixture
            .commit("main")
            .file("a.txt", "a\n")
            .create("c1")
            .unwrap();
        let c2 = fixture
            .commit("main")
            .file("b.txt", "b\n")
            .create("c2")
            .unwrap();
        let c3 = fixture
            .commit("main")
            .file("c.txt", "c\n")
            .create("c3")
            .unwrap();
        let c4 = fixture
            .commit("main")
            .file("d.txt", "d\n")
            .create("c4")
            .unwrap();

        let bare = |name: &str, base, head, current| Changeset {
            name: name.to_string(),
            span: ChangesetSpan::Committed { base, head },
            title: None,
            current,
            needs_restack: false,
        };
        let changesets = vec![
            bare("cs-1", root, c1, false),
            bare("cs-2", c1, c2, false),
            bare("cs-3", c2, c3, false),
            bare("cs-4", c3, c4, true),
        ];

        let repo = fixture.repo().unwrap();
        let repo_path = repo.workdir().unwrap().to_path_buf();
        let owned = Repository::open(&repo_path).unwrap();

        let pending_views: Vec<ChangesetView> = changesets
            .iter()
            .cloned()
            .map(ChangesetView::pending)
            .collect();
        let mut app = App::from_changesets(owned, pending_views);
        app.set_defer_loads(true);
        app.open_current();

        let active_idx = app.current_cs();
        assert_eq!(
            active_idx, 3,
            "the lib-current changeset (cs-4) opens active"
        );
        assert!(app.is_current_pending(), "every slot starts Pending");

        let (tx, rx) = mpsc::channel::<InboxMessage>();
        let load_tx = spawn_loader_thread(repo_path.clone(), tx.clone());
        let to_diff: Vec<(usize, Changeset)> = changesets.into_iter().enumerate().collect();
        spawn_wave_thread(
            repo_path.clone(),
            tx.clone(),
            to_diff,
            app.generation(),
            Some(active_idx),
        );
        drop(tx); // this test's only senders now are the two spawned threads

        let deadline_per_recv = Duration::from_secs(15);
        let mut changesets_ready = vec![false; app.changeset_count()];
        let mut active_file_loaded = false;
        let mut dispatched_active_load = false;

        loop {
            if changesets_ready.iter().all(|&r| r) && active_file_loaded {
                break;
            }
            let event = rx
                .recv_timeout(deadline_per_recv)
                .expect("loader/wave thread must answer within the deadline")
                .expect("neither real thread should forward a read error in this test");

            match event {
                AppEvent::ChangesetReady { gen, idx, result } => {
                    assert!(
                        result.is_ok(),
                        "a committed changeset diff must not fail here"
                    );
                    app.apply_changeset_ready(gen, idx, result);
                    changesets_ready[idx] = true;
                }
                AppEvent::FileReady {
                    gen,
                    cs_idx,
                    file_idx,
                    result,
                } => {
                    assert!(result.is_ok(), "a real file load must not fail here");
                    app.apply_file_ready(gen, cs_idx, file_idx, result);
                    if cs_idx == active_idx && file_idx == 0 {
                        active_file_loaded = true;
                    }
                }
                other => panic!("unexpected event in the real-thread smoke: {other:?}"),
            }

            // The same post-batch checkpoint `event_loop` runs on every idle `Tick` while an
            // open is pending — dispatched here the instant it's possible (right after the
            // active changeset seats) rather than gated behind a real debounce, since nothing in
            // this test races real terminal input.
            if !dispatched_active_load && app.open_pending() {
                if let Some((gen, cs_idx, file_idx, spec)) = app.take_pending_load_spec() {
                    load_tx
                        .send(LoadRequest {
                            gen,
                            cs_idx,
                            file_idx,
                            spec,
                        })
                        .expect("loader thread must still be alive to receive the dispatch");
                    dispatched_active_load = true;
                }
            }
        }

        assert!(
            changesets_ready.iter().all(|&r| r),
            "every slot must land Ready: {changesets_ready:?}"
        );
        assert!(
            !app.is_current_pending(),
            "the active changeset must be seated once its wave result lands"
        );
        assert_eq!(
            app.current_cs(),
            active_idx,
            "seating must not move which changeset is active"
        );
        assert!(
            active_file_loaded,
            "the active changeset's deferred file open must complete via a real FileReady"
        );
        assert!(
            !app.open_pending(),
            "a completed FileReady must clear the pending-open flag"
        );
        assert!(
            app.current_view_ref().is_some(),
            "the active file's view must be cached after its FileReady lands"
        );
    }

    // ── `reload-config` (`R`) ───────────────────────────────────────────────────

    #[test]
    fn reload_config_command_maps_to_the_reload_action_and_sets_the_app_flag() {
        assert_eq!(
            command_to_action(Command::ReloadConfig, 20),
            Action::ReloadConfig
        );

        use git_workon_fixture::prelude::*;
        let fixture = FixtureBuilder::new().build().unwrap();
        let mut app = app_from_fixture(&fixture);
        assert!(!app.take_config_reload_request());

        apply_action(&mut app, Action::ReloadConfig);
        assert!(
            app.take_config_reload_request(),
            "Action::ReloadConfig must raise App's request flag"
        );
        assert!(!app.take_config_reload_request(), "the flag is one-shot");
    }

    // ── diff-hscroll: `Action::FocusOutline` pans home before focusing ─────────────

    /// Locked decision #2: `h`/`left` (`Action::FocusOutline`) pans the diff back toward column
    /// `0` first while panned, and only actually focuses the outline once there — implemented in
    /// this dispatch arm rather than in `App::focus_outline` itself (see that arm's comment), so
    /// this is only testable at the `apply_action` layer, not through `App` alone.
    #[test]
    fn focus_outline_action_pans_home_before_focusing_when_panned() {
        use git_workon_fixture::prelude::*;

        let long_line = "x".repeat(200);
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("a.txt", "short\n", &format!("{long_line}\n"))
            .build()
            .unwrap();

        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.hscroll_right();
        assert!(
            app.hscroll > 0,
            "the long line must give hscroll room to pan"
        );
        assert!(!app.outline_focused());

        // Panned: the first press pans back toward column 0 rather than focusing the outline.
        apply_action(&mut app, Action::FocusOutline);
        assert_eq!(
            app.hscroll, 0,
            "one press from a single hscroll step returns to column 0"
        );
        assert!(
            !app.outline_focused(),
            "still unfocused — this press only panned"
        );

        // Already at column 0: the next press focuses the outline as normal.
        apply_action(&mut app, Action::FocusOutline);
        assert!(app.outline_focused());
    }

    /// Mouse h-wheel follow-up: a `ScrollRight` event reaches `App::handle_hwheel` (not the
    /// vertical `App::handle_wheel`) when dispatched through the full `update` path — mirroring
    /// how the existing vertical-wheel tests exercise `App::handle_wheel` directly, but this one
    /// goes through `map_terminal_event` + `update`'s mouse arm to also pin the event mapping.
    #[test]
    fn scroll_right_event_reaches_handle_hwheel_via_update() {
        use git_workon_fixture::prelude::*;
        use workon_review::app::Region;

        let lines: String = (1..=40).map(|n| format!("l{n}\n")).collect();
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("big.txt", &lines)
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.open_current();
        app.pane_height = 10;
        app.hit_regions.single = Some(Region {
            x: 0,
            y: 0,
            w: 40,
            h: 10,
        });
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        assert_eq!(app.hscroll, 0);

        let raw = MouseEvent {
            kind: MouseEventKind::ScrollRight,
            column: 10,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        // Round-trip through the real mapping first, matching how the input thread feeds `update`.
        let mapped = map_terminal_event(Event::Mouse(raw)).expect("ScrollRight must map");
        update(&mut app, &km, &mut pending, mapped);

        assert!(
            app.hscroll > 0,
            "a ScrollRight event over the diff pane must pan App::hscroll via handle_hwheel"
        );
    }

    // ── CS2 (`outline-filter`, M11): the filter-input modal cascade arm ──────────

    /// A single (uncommitted) changeset with three distinct files, outline open+focused in Flat
    /// mode (no header row in the way) — CS2's cascade tests just need "type `/`, then some keys,
    /// assert `App` state," and Flat mode keeps the row math simple (every row is a `File`).
    fn filter_test_app() -> App {
        use git_workon_fixture::prelude::*;
        use workon_review::outline::OutlineMode;

        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .unstaged_file("apple.txt", "a\n", "a\nCHANGED\n")
            .unstaged_file("banana.txt", "b\n", "b\nCHANGED\n")
            .unstaged_file("cherry.txt", "c\n", "c\nCHANGED\n")
            .build()
            .unwrap();
        let mut app = app_from_fixture(&fixture);
        app.set_outline_mode(OutlineMode::Flat);
        app.toggle_outline(); // closed -> open+focused
        app
    }

    #[test]
    fn slash_focuses_the_filter_input_from_the_outline() {
        let mut app = filter_test_app();
        assert!(
            app.outline_focused(),
            "outline must have focus for `/` to dispatch"
        );
        assert!(!app.outline_filter_focused());
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('/'))),
        );

        assert!(
            app.outline_filter_focused(),
            "`/` must focus the filter input"
        );
    }

    #[test]
    fn typing_while_the_filter_is_focused_narrows_the_outline_and_never_falls_through_to_a_bound_key(
    ) {
        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();

        // 'j' is bound to `cursor-down` in the Outline view — while the filter input has capture
        // it must be inserted as literal text instead of moving the outline cursor.
        for c in "an".chars() {
            update(
                &mut app,
                &km,
                &mut pending,
                AppEvent::Key(key(KeyCode::Char(c))),
            );
        }

        assert_eq!(app.outline_filter_query(), "an");
        let items = app.outline_items();
        assert_eq!(items.len(), 1, "only banana.txt fuzzy-matches 'an'");
        assert!(matches!(
            &items[0],
            workon_review::outline::OutlineItem::File { path, .. } if path == "banana.txt"
        ));
    }

    #[test]
    fn enter_returns_focus_to_the_list_and_keeps_the_query() {
        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();
        app.outline_filter_insert_char('a');

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Enter)),
        );

        assert!(
            !app.outline_filter_focused(),
            "Enter must hand capture back to the list"
        );
        assert_eq!(app.outline_filter_query(), "a", "Enter must KEEP the query");
    }

    #[test]
    fn esc_returns_focus_to_the_list_and_keeps_the_query_same_as_enter() {
        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();
        app.outline_filter_insert_char('a');

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );

        assert!(!app.outline_filter_focused());
        assert_eq!(app.outline_filter_query(), "a");
    }

    #[test]
    fn esc_on_the_list_clears_an_active_filter_before_the_next_esc_quits() {
        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();
        app.outline_filter_insert_char('a');
        app.outline_filter_unfocus();
        assert!(app.outline_focused(), "list (not input) must have capture");
        assert_eq!(app.outline_filter_query(), "a");

        // First Esc: unwind the filter (ladder case 6) — clear the query, do NOT quit.
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );
        assert!(!quit, "Esc with an active filter must not quit the review");
        assert!(
            app.outline_filter_query().is_empty(),
            "Esc on the list must clear the active filter query"
        );

        // Second Esc: the filter is gone, so the outline's terminal quit leaf (case 7) applies.
        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Esc)),
        );
        assert!(
            quit,
            "with no filter left to unwind, Esc quits from the outline"
        );
    }

    #[test]
    fn ctrl_c_clears_the_query_and_returns_focus_to_the_list() {
        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();
        app.outline_filter_insert_char('a');
        assert!(!app.outline_filter_query().is_empty());

        update(&mut app, &km, &mut pending, AppEvent::Key(ctrl_key('c')));

        assert!(!app.outline_filter_focused());
        assert!(
            app.outline_filter_query().is_empty(),
            "Ctrl-c must clear the query, unlike Enter/Esc"
        );
    }

    #[test]
    fn down_moves_the_outline_selection_without_leaving_the_filter_input() {
        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();
        // No query typed: every file row still matches, so all three rows are still reachable to
        // move across.
        let cursor_before = app.outline_cursor();

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Down)),
        );

        assert!(
            app.outline_filter_focused(),
            "Down must NOT leave the filter input"
        );
        assert_eq!(
            app.outline_cursor(),
            cursor_before + 1,
            "Down must move the outline selection while capture stays on the input"
        );
    }

    #[test]
    fn ctrl_n_and_ctrl_p_also_move_the_outline_selection_from_the_filter_input() {
        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();

        update(&mut app, &km, &mut pending, AppEvent::Key(ctrl_key('n')));
        assert_eq!(app.outline_cursor(), 1);
        assert!(app.outline_filter_focused());

        update(&mut app, &km, &mut pending, AppEvent::Key(ctrl_key('p')));
        assert_eq!(app.outline_cursor(), 0);
        assert!(app.outline_filter_focused());
    }

    #[test]
    fn a_pending_confirm_wins_over_the_filter_input_capture() {
        use workon_review::app::PendingOp;

        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();
        app.request_confirm("Discard? (y/n)", PendingOp::DiscardFile { file_idx: 0 });

        update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Key(key(KeyCode::Char('y'))),
        );

        assert!(
            app.pending_confirm.is_none(),
            "the confirm modal must capture y first, per the documented Esc-precedence ladder"
        );
        assert!(
            app.outline_filter_focused(),
            "the confirm arm must not have touched filter focus"
        );
    }

    #[test]
    fn a_mouse_event_is_swallowed_while_the_filter_input_has_capture() {
        let mut app = filter_test_app();
        let km = Keymap::defaults();
        let mut pending: Vec<KeyPress> = Vec::new();
        app.outline_filter_focus();
        let cursor_before = app.outline_cursor();

        let quit = update(
            &mut app,
            &km,
            &mut pending,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Left))),
        );

        assert!(!quit);
        assert!(
            app.outline_filter_focused(),
            "the click must not close the input"
        );
        assert_eq!(app.outline_cursor(), cursor_before);
    }
}
