//! PTY responsiveness smoke tests for the 2026-07 performance pass — the launch path, the
//! rapid-outline-nav path, and (ADR-031) the streamed-startup path, driven against the real
//! binary in a pseudo-terminal.
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
//! - **Streamed startup (ADR-031):** before the progressive pipeline, a multi-changeset launch
//!   diffed the WHOLE stack sequentially-then-in-parallel before the first frame ever drew —
//!   the wait scaled with stack depth. The streamed-startup test bounds spawn→quit on a deep
//!   stack so a reintroduced "wait for the full wave" launch fails loudly; see that test's doc
//!   comment for the measured before/after numbers that sized its bound.
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
//! cargo test -p git-workon-review --test pty -- --ignored
//! ```
//!
//! Frame-content assertions are deliberately absent — capturing ratatui frame TEXT through a
//! PTY is unreliable (only escape sequences survive dependably); rendering is covered by the
//! `TestBackend` tests in `render.rs`/`tui.rs`.

use crate::pty_support::spawn_review;

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

/// Upper bound on spawn→quit for `streamed_startup_lands_before_a_full_wave_could_have_finished`
/// (ADR-031). Sized from real measurements on this fixture (`q` sent right after
/// alternate-screen entry — same protocol as `LAUNCH_RESPONSIVE`, so `q` buffers in the PTY
/// until the event loop actually starts polling input):
///
/// - The CURRENT (streamed) binary: ~1.0-1.3s (debug build; ~1.0s release) — dominated by fixed
///   per-launch costs (`gt --version`'s ~350ms subprocess spawn, checking out
///   `STREAMED_STARTUP_STACK_SIZE` branches' worth of files) plus entering the alternate screen
///   and starting the event loop, which — this is the whole point of ADR-031 — happens BEFORE
///   any changeset is diffed: the wave runs on a background thread, off the spawn→interactive
///   critical path entirely. Re-measured after fixing the fixture setup (F4) that was building
///   every `cs{i}`'s base against main's tip instead of its own predecessor's head; since none
///   of a changeset's diff cost is on this critical path either way, the number barely moved —
///   confirming spawn→quit here really is a fixed-cost bound, not a diff-size one.
/// - The PRE-ADR-031 binary (commit `2214558`, built and run against an equivalent fixture
///   pre-F4): ~6.4s — it blocks on the full stack's diff wave before the event loop ever starts,
///   so `q` sits in the PTY the whole time. Not re-measured against the F4-corrected fixture
///   (would need rebuilding that historical commit); a synchronous full-wave wait over 150
///   real per-changeset diffs is unambiguously far past this bound either way.
///
/// This bound sits comfortably (~2-3×) above the healthy number and well below the regressed
/// one — the same blunt, load-tolerant margin philosophy as `LAUNCH_RESPONSIVE`/
/// `BURST_RESPONSIVE`, just re-measured for this fixture rather than reused, since the streamed
/// path's fixed costs (checkout of a much deeper stack) differ from the single-changeset launch
/// test's.
const STREAMED_STARTUP_RESPONSIVE: Duration = Duration::from_secs(3);

/// Depth of the Graphite stack `streamed_startup_lands_before_a_full_wave_could_have_finished`
/// builds — deep and wide enough (with `STREAMED_STARTUP_FILE_LINES`) that the full wave's
/// total diff cost is many seconds, well clear of `STREAMED_STARTUP_RESPONSIVE`, while a single
/// changeset's diff (what the streamed path actually waits on before its first frame) stays
/// well under it. See `STREAMED_STARTUP_RESPONSIVE`'s doc comment for the measurements that
/// picked this size.
const STREAMED_STARTUP_STACK_SIZE: usize = 150;

/// Lines per generated fixture file in the streamed-startup stack — see
/// `STREAMED_STARTUP_STACK_SIZE`.
const STREAMED_STARTUP_FILE_LINES: usize = 3_000;

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
#[ignore = "PTY smoke — run explicitly: cargo test -p git-workon-review --test pty -- --ignored"]
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
#[ignore = "PTY smoke — run explicitly: cargo test -p git-workon-review --test pty -- --ignored"]
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

/// Commit `path`/`content` as a child of `parent`, without moving any branch ref — mirrors
/// `diff_model.rs`'s `commit_onto`, duplicated here rather than shared: this file needs it only
/// to build one deep real Graphite chain, not worth a cross-test-file dependency for.
fn commit_onto(
    repo: &git2::Repository,
    parent: &git2::Commit,
    path: &str,
    content: &str,
) -> git2::Oid {
    let mut treebuilder = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
    let blob_oid = repo.blob(content.as_bytes()).unwrap();
    treebuilder
        .insert(path, blob_oid, git2::FileMode::Blob.into())
        .unwrap();
    let tree_oid = treebuilder.write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(None, &sig, &sig, "test commit", &tree, &[parent])
        .unwrap()
}

/// ADR-031's Testing layer 3, part two: the `pty_responsiveness` extension the ADR calls for —
/// "asserting the first interactive frame lands before a full wave could have finished". A real
/// `STREAMED_STARTUP_STACK_SIZE`-deep Graphite stack, each node adding one
/// `STREAMED_STARTUP_FILE_LINES`-line file (real content, so each changeset's diff is real,
/// non-trivial work — the cost being guarded), checked out on the TIP branch so the real
/// binary's no-argument auto-detect (`StackModel::detect` + `assemble_changesets`) sees the
/// whole stack, exactly like a real multi-changeset Graphite review.
///
/// The oracle is the SAME shape as `launch_reaches_the_tui_and_quits_promptly`'s: `q` sent the
/// instant the alternate screen appears, bounding spawn→quit — `q` buffers in the PTY until the
/// event loop starts polling input, so this elapsed time IS "time until the event loop is
/// actually running and responsive", not just "time to first draw". That is precisely what a
/// de-streaming regression breaks: reverting `Tui::run_streamed` to block on the full diff wave
/// (e.g. joining `spawn_wave_thread`, or calling `diff_changesets` synchronously like the
/// pre-ADR-031 multi-changeset path did) delays the event loop's first `recv_event` by the
/// WHOLE wave's cost, not just the active changeset's — so `q` sits unanswered in the PTY for
/// the full wave's duration. See `STREAMED_STARTUP_RESPONSIVE`'s doc comment for the actual
/// measured numbers (streamed ~1.1-1.4s, reverted-to-pre-ADR-031 ~6.4s on this exact fixture)
/// that sized the bound and confirm this assertion fails on the regressed shape, the same
/// validation discipline `BURST_RESPONSIVE`'s doc comment describes.
#[test]
#[ignore = "PTY smoke — run explicitly: cargo test -p git-workon-review --test pty -- --ignored"]
fn streamed_startup_lands_before_a_full_wave_could_have_finished() {
    use workon::{assemble_changesets, StackModel, UncommittedLayer};

    let n = STREAMED_STARTUP_STACK_SIZE;
    let mut builder = FixtureBuilder::new()
        .config("core.autocrlf", "false")
        .config("workon.review.theme", "dark")
        .graphite_config(&["main"]);
    for i in 0..n {
        let parent = if i == 0 {
            "main".to_string()
        } else {
            format!("cs{}", i - 1)
        };
        builder = builder.branch_metadata(&format!("cs{i}"), &parent);
    }
    let fixture = builder.build().expect("fixture");
    let repo = fixture.repo().expect("fixture repo");

    // `branch_metadata` creates each branch ref at build() time (co-located with `main`'s tip);
    // advance each one to a REAL, distinct commit forming an actual linear chain — same pattern
    // `diff_model.rs`'s Graphite tests use.
    let main_tip = repo
        .find_branch("main", git2::BranchType::Local)
        .expect("main branch")
        .get()
        .target()
        .expect("main tip");
    let mut parent_oid = main_tip;
    for i in 0..n {
        let parent_commit = repo.find_commit(parent_oid).expect("parent commit");
        let head = commit_onto(
            repo,
            &parent_commit,
            &format!("f{i}.rs"),
            &rust_source(i, STREAMED_STARTUP_FILE_LINES),
        );
        fixture
            .update_branch(&format!("cs{i}"), head)
            .expect("advance branch");
        parent_oid = head;
    }

    // Sanity: the stack really does resolve to `n` real changesets, each with a real diff to
    // do — a fixture bug here (e.g. all branches landing on the same commit) would make the
    // bound pass for the wrong reason.
    let changesets = assemble_changesets(
        repo,
        &format!("cs{}", n - 1),
        StackModel::Graphite,
        UncommittedLayer::Include,
    )
    .expect("assemble the real Graphite stack");
    let cs_changesets: Vec<&workon::Changeset> = changesets
        .iter()
        .filter(|c| c.name.starts_with("cs"))
        .collect();
    assert_eq!(
        cs_changesets.len(),
        n,
        "fixture setup must produce every tracked changeset"
    );
    // F4: pin the intended shape — each `cs{i}`'s span is base→head against its OWN immediate
    // predecessor (cs0's base is main's tip), not every changeset cumulatively based on main.
    // `branch_metadata`'s revisions resolve once at `build()` time (before the `update_branch`
    // loop above moves any ref), so a fixture that doesn't keep those recorded revisions in
    // sync makes `resolve_graphite_base` fall back to main's tip for every one of them —
    // cumulative 150-file spans and `needs_restack = true` everywhere, contradicting both this
    // shape and the "one changeset's diff" cost this test's bound is sized against.
    let mut expected_base = main_tip;
    for (i, cs) in cs_changesets.iter().enumerate() {
        assert_eq!(
            cs.name,
            format!("cs{i}"),
            "changesets must come back in base→head order"
        );
        match cs.span {
            workon::ChangesetSpan::Committed { base, head } => {
                assert_eq!(
                    base, expected_base,
                    "cs{i}'s base must be its own predecessor's head, not main's tip"
                );
                expected_base = head;
            }
            other => panic!("cs{i} must be a Committed span, got {other:?}"),
        }
        assert!(
            !cs.needs_restack,
            "cs{i} must not need a restack — its recorded parent revision must track its \
             parent's live tip"
        );
    }

    // Check out the tip branch as HEAD — the real binary's no-argument auto-detect reads
    // `repo.head()`'s shorthand, not an explicit `[SOURCE]` argument.
    repo.set_head(&format!("refs/heads/cs{}", n - 1))
        .expect("set HEAD to the tip branch");
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_head(Some(&mut checkout))
        .expect("checkout the tip branch");

    let launched = Instant::now();
    let mut session = spawn_review(&fixture);
    session
        .expect("\x1b[?1049h")
        .expect("TUI entered the alternate screen");

    // Same protocol as the launch test: `q` buffers in the PTY until the event loop polls
    // input, so spawn→quit IS time-to-interactive plus one quit — the full wave's cost, if the
    // event loop were blocked behind it, lands entirely inside this window.
    session.send("q").expect("send q");
    session.expect(expectrl::Eof).expect("app exited on q");

    let elapsed = launched.elapsed();
    eprintln!("streamed startup ({n} changesets)→quit: {elapsed:?}");
    assert!(
        elapsed < STREAMED_STARTUP_RESPONSIVE,
        "streamed-startup→quit took {elapsed:?} on a {n}-changeset stack — the event loop \
         looks blocked behind the full diff wave again (streamed launch regressed to a \
         synchronous wait)"
    );
}
