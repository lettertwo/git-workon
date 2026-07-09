//! PTY smoke tests for the `theme = auto` terminal probe (dogfood round 2 regression).
//!
//! These spawn the real binary under a pseudo-terminal and play the *terminal's* side of the
//! OSC color-query conversation — the one scenario unit tests can't reach, and the one that hid
//! the round-2 wedge: a terminal that ANSWERS the probe. When the probe mishandled its replies
//! they leaked into crossterm as phantom keystrokes (`r` in `rgb:` fired refresh storms; `d` in
//! hex specs opened the discard confirm, which swallows every key but y/n/Esc), freezing startup
//! for ~30s. The assertion here is deliberately blunt: after startup settles, `q` must still
//! quit promptly.
//!
//! **Not run by default** (`#[ignore]`): PTY tests are wall-clock-bound (settle windows, probe
//! deadline) and load-sensitive — under heavy parallel CPU load a slow spawn can eat into the
//! responsiveness margin (same caveat as git-workon's `checkout_conflict_interactive_*` PTY
//! test: re-run solo before treating a failure as a regression). Run them explicitly:
//!
//! ```text
//! cargo test -p git-workon-review --test pty_smoke -- --ignored
//! ```
//!
//! Color/SGR assertions are deliberately absent — capturing ratatui frames through a PTY is
//! unreliable; reply *parsing* is unit-tested in `terminal_query.rs`.

#![cfg(unix)]

use std::io::Write;
use std::time::{Duration, Instant};

use expectrl::{
    session::{OsProcess, OsStream},
    Expect, Session,
};
use git_workon_fixture::prelude::*;

/// How long `q` may take to terminate the app before we call startup unresponsive. Generous on
/// purpose: healthy is ~10ms, the regression was 30s–forever, and the slack absorbs CI load.
const RESPONSIVE: Duration = Duration::from_secs(5);

/// A repo with one uncommitted change (so the TUI actually opens) and `theme = auto` (so the
/// probe runs).
fn auto_theme_fixture() -> Fixture {
    FixtureBuilder::new()
        .config("workon.review.theme", "auto")
        .unstaged_file("file.txt", "a\nb\nc\n", "a\nCHANGED\nc\n")
        .build()
        .expect("fixture")
}

/// Spawn the review binary in a PTY sized like a real terminal (an unsized PTY is 0×0 and
/// ratatui draws nothing), cwd'd into the fixture's worktree.
fn spawn_review(fixture: &Fixture) -> Session<OsProcess, OsStream> {
    let repo = fixture.repo().expect("fixture repo");
    let workdir = repo.workdir().expect("fixture workdir").to_path_buf();

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_git-workon-review"));
    cmd.current_dir(workdir).env("TERM", "xterm-256color");

    let mut session = expectrl::Session::spawn(cmd).expect("spawn in PTY");
    session
        .get_process_mut()
        .set_window_size(120, 40)
        .expect("size PTY");
    session.set_expect_timeout(Some(Duration::from_secs(15)));
    session
}

/// Play a well-behaved answering terminal: reply to all 16 `OSC 4` color queries plus
/// `OSC 11`/`OSC 10`, then the DA1 sentinel. The replies deliberately contain the poison bytes
/// of the round-2 wedge — `r`/`g`/`b` (refresh binding) and `d` hex digits (discard binding) —
/// so any regression that leaks them into crossterm trips the discard-confirm modal and fails
/// the responsiveness assertion below.
fn answer_probe(session: &mut Session<OsProcess, OsStream>) {
    let mut replies = Vec::new();
    for n in 0..16 {
        let level = n * 16;
        replies.extend_from_slice(
            format!("\x1b]4;{n};rgb:{level:02x}{level:02x}/2020/4040\x1b\\").as_bytes(),
        );
    }
    replies.extend_from_slice(b"\x1b]11;rgb:1a1a/1a1a/1a1a\x1b\\"); // background (dark)
    replies.extend_from_slice(b"\x1b]10;rgb:d3d3/d0d0/c8c8\x1b\\"); // foreground ('d' poison)
    replies.extend_from_slice(b"\x1b[?62;22c"); // DA1 reply — the probe's stop sentinel
    session.write_all(&replies).expect("write probe replies");
    session.flush().expect("flush probe replies");
}

/// Wait for the TUI to be up (alternate screen entered), let any straggler reply bytes land,
/// then press `q` and require a prompt exit.
fn assert_q_quits_promptly(mut session: Session<OsProcess, OsStream>) {
    session
        .expect("\x1b[?1049h") // EnterAlternateScreen — tui::run has the terminal
        .expect("TUI entered the alternate screen");

    // Give leaked bytes (the regression case) time to reach crossterm before q, so a regressed
    // binary deterministically has its discard-confirm modal up — and swallows the q.
    std::thread::sleep(Duration::from_millis(500));

    let pressed_q = Instant::now();
    session.send("q").expect("send q");
    session.expect(expectrl::Eof).expect("app exited on q");
    let latency = pressed_q.elapsed();
    assert!(
        latency < RESPONSIVE,
        "q took {latency:?} to quit the app — startup input is wedged"
    );
}

#[test]
#[ignore = "PTY smoke — run explicitly: cargo test -p git-workon-review --test pty_smoke -- --ignored"]
fn theme_auto_stays_responsive_when_the_terminal_answers() {
    let fixture = auto_theme_fixture();
    let mut session = spawn_review(&fixture);

    // The probe's query burst ends with its DA1 request; seeing it means every OSC query has
    // been written and the terminal may answer.
    session
        .expect("\x1b[c")
        .expect("probe sent its query burst");
    answer_probe(&mut session);

    assert_q_quits_promptly(session);
}

#[test]
#[ignore = "PTY smoke — run explicitly: cargo test -p git-workon-review --test pty_smoke -- --ignored"]
fn theme_auto_stays_responsive_when_the_terminal_is_silent() {
    // The no-hang guarantee: a terminal that never answers (tmux without passthrough, CI) must
    // cost at most the probe deadline, then fall back to a curated theme and run normally.
    let fixture = auto_theme_fixture();
    let session = spawn_review(&fixture);

    assert_q_quits_promptly(session);
}
