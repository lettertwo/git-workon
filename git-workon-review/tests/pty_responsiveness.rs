//! PTY responsiveness smoke tests for the 2026-07 performance pass — the launch path and the
//! rapid-outline-nav path, driven against the real binary in a pseudo-terminal.
//!
//! These guard the *regression classes* that pass fixed, not the milliseconds it measured:
//!
//! - **Launch:** startup once spent ~370ms spawning `gt --version` (a Node CLI) inside
//!   `StackModel::detect`, and ~230ms diffing the whole stack sequentially, all before the first
//!   frame. The launch test bounds spawn→quit so a reintroduced subprocess spawn or blocking
//!   probe in the launch path fails loudly.
//! - **Nav burst:** every outline `j` once ran a synchronous `FileView::load` (blob reads,
//!   alignment, two whole-file tree-sitter passes) for each file it crossed — 10-100ms per key.
//!   The burst test buffers a 40-key sweep over ~two dozen large Rust files and bounds
//!   burst→quit; if input coalescing (`update_batch`) or idle-deferred loads
//!   (`open_pending`/`OPEN_DEBOUNCE`) regress, the quit waits behind the sum of every
//!   intermediate file's load and blows the bound.
//!
//! The bounds are deliberately blunt (seconds, not milliseconds): absolute wall-clock
//! assertions flake under parallel CPU load, exactly like git-workon's
//! `checkout_conflict_interactive_*` PTY test — re-run solo before treating a failure as a
//! regression. Precise per-phase timings stay a manual workflow (temporary instrumentation +
//! an expect(1) driver), not CI assertions.
//!
//! **Not run by default** (`#[ignore]`) for the same wall-clock reasons as `pty_smoke.rs`. Run
//! explicitly:
//!
//! ```text
//! cargo test -p git-workon-review --test pty_responsiveness -- --ignored
//! ```
//!
//! Frame-content assertions are deliberately absent — capturing ratatui frame TEXT through a
//! PTY is unreliable (only escape sequences survive dependably); rendering is covered by the
//! `TestBackend` tests in `render.rs`/`tui.rs`.

#![cfg(unix)]

mod pty_support;
use pty_support::spawn_review;

use std::time::{Duration, Instant};

use expectrl::Expect;
use git_workon_fixture::prelude::*;

/// Upper bound on spawn→quit for a healthy launch (~120ms release, well under a second in
/// debug). Generous on purpose: the regression classes cost multiple seconds (a Node spawn per
/// detection, a sequential stack diff), and the slack absorbs CI load.
const LAUNCH_RESPONSIVE: Duration = Duration::from_secs(5);

/// Upper bound on burst-sent→quit. Healthy is near-instant: the burst coalesces to one outline
/// move, loads defer past the buffered `q`, and the app exits without ever loading the
/// intermediate files. The regressed shape loads every file the sweep crossed. The bound was
/// sized against measurements of BOTH sides on this fixture in a debug build: the actual
/// pre-fix code (`m7-complete`, one synchronous load per outline row) took ~2.6s; the fixed
/// code ~120ms. 1s sits ~8× above healthy and ~2.5× below regressed.
const BURST_RESPONSIVE: Duration = Duration::from_secs(1);

/// How many generated Rust files the nav-burst fixture carries, and how the burst is sized:
/// enough files (and enough lines per file — see `BURST_FILE_LINES`) that a load-per-key
/// regression accumulates seconds of tree-sitter work, few enough that fixture setup stays
/// cheap.
const BURST_FILES: usize = 36;

/// Lines per generated fixture file — see `BURST_FILES`.
const BURST_FILE_LINES: usize = 2_000;

/// A plausible-enough Rust source of ~`lines` lines, distinct per `seed`, so the tree-sitter
/// highlighter has real parsing work per file (the regression cost being guarded).
fn rust_source(seed: usize, lines: usize) -> String {
    let mut src = String::with_capacity(lines * 40);
    src.push_str(&format!("//! Generated fixture module {seed}.\n\n"));
    let mut n = 0;
    while src.lines().count() < lines {
        src.push_str(&format!(
            "pub fn item_{seed}_{n}(x: u64) -> u64 {{\n    let y = x.wrapping_mul({n}) + {seed};\n    y ^ (y >> 3)\n}}\n\n",
        ));
        n += 1;
    }
    src
}

#[test]
#[ignore = "PTY smoke — run explicitly: cargo test -p git-workon-review --test pty_responsiveness -- --ignored"]
fn launch_reaches_the_tui_and_quits_promptly() {
    // Theme pinned to dark so the `theme = auto` probe (and its deadline) stays out of this
    // bound — the probe's own responsiveness is pty_smoke.rs's job. One unstaged change so the
    // TUI actually opens; a plain (non-Graphite) repo keeps behavior identical whether or not
    // the machine has `gt` on PATH — and `StackModel::detect` still runs `detect_gt` first, so
    // a reintroduced subprocess spawn there is still inside the measured window.
    let fixture = FixtureBuilder::new()
        .config("workon.review.theme", "dark")
        .unstaged_file("file.txt", "a\nb\nc\n", "a\nCHANGED\nc\n")
        .build()
        .expect("fixture");

    let launched = Instant::now();
    let mut session = spawn_review(&fixture);

    // The alternate screen is the proof the launch reached the TUI — without this, an early
    // error exit (or "nothing to review") would sail through the quit assertion trivially.
    session
        .expect("\x1b[?1049h")
        .expect("TUI entered the alternate screen");

    // `q` buffers in the PTY until the event loop polls input, so send it immediately: the
    // elapsed spawn→exit time IS time-to-interactive plus one quit.
    session.send("q").expect("send q");
    session.expect(expectrl::Eof).expect("app exited on q");

    let elapsed = launched.elapsed();
    assert!(
        elapsed < LAUNCH_RESPONSIVE,
        "launch→quit took {elapsed:?} — something slow is blocking the launch path \
         (subprocess spawn? sequential stack diff? probe?)"
    );
}

#[test]
#[ignore = "PTY smoke — run explicitly: cargo test -p git-workon-review --test pty_responsiveness -- --ignored"]
fn rapid_outline_nav_burst_stays_responsive() {
    // Dozens of untracked multi-thousand-line Rust files: every outline row the burst crosses
    // is a file whose (regressed) synchronous load would cost real tree-sitter work.
    let mut builder = FixtureBuilder::new().config("workon.review.theme", "dark");
    let sources: Vec<(String, String)> = (0..BURST_FILES)
        .map(|i| (format!("src_{i:02}.rs"), rust_source(i, BURST_FILE_LINES)))
        .collect();
    for (path, content) in &sources {
        builder = builder.untracked_file(path, content);
    }
    let fixture = builder.build().expect("fixture");

    let mut session = spawn_review(&fixture);
    session
        .expect("\x1b[?1049h")
        .expect("TUI entered the alternate screen");

    // Buffer the whole interaction at once — focus the outline, sweep down across every file
    // row, quit. This is the buffered-burst shape the coalescing fix exists for: healthy code
    // merges the sweep into one outline move and quits before any deferred load fires;
    // regressed code loads each file it crosses before it ever reaches the `q`.
    let burst_sent = Instant::now();
    let mut input = String::from("o");
    input.push_str(&"j".repeat(BURST_FILES + 12)); // sweep past every file row, clamp at the end
    input.push('q');
    session.send(&input).expect("send nav burst");
    session.expect(expectrl::Eof).expect("app exited on q");

    let elapsed = burst_sent.elapsed();
    // Visible under `--nocapture`; also the number to check when triaging a failure.
    eprintln!("burst→quit: {elapsed:?}");
    assert!(
        elapsed < BURST_RESPONSIVE,
        "burst→quit took {elapsed:?} — outline nav is loading files synchronously again \
         (input coalescing or idle-deferred loads regressed)"
    );
}
