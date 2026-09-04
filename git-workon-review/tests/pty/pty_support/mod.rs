//! Shared PTY-test support for the `pty_smoke` and `pty_responsiveness` modules of the `pty`
//! integration-test binary (`tests/pty/main.rs`).
//!
//! A `tests/pty/<name>/mod.rs` directory module so cargo does not build it as a test binary of
//! its own; `main.rs` declares `mod pty_support;` once and the suite modules reach it via
//! `crate::pty_support`. Keeping the spawn setup in one place means a change to the window size,
//! `TERM`, or expect timeout applies to every PTY suite at once — the two suites guard related
//! regressions, so silent drift here would matter.

use std::time::Duration;

use expectrl::{
    session::{OsProcess, OsStream},
    Session,
};
use git_workon_fixture::prelude::*;

/// Spawn the review binary in a PTY sized like a real terminal (an unsized PTY is 0×0 and
/// ratatui draws nothing), cwd'd into the fixture's worktree.
///
/// `WORKON_REVIEW_PROBE_CACHE` is pinned to a file inside the fixture's own tempdir (never the
/// real user cache dir): without this, the `theme = auto` silent-terminal cache (added alongside
/// this comment) would read and write the developer's/CI runner's actual
/// `dirs::cache_dir()/git-workon-review/silent-terminals.json` — a second run of the silent-PTY
/// test on the same real terminal would then hit a live cache entry from a PRIOR run and skip
/// the probe, breaking `theme_auto_stays_responsive_when_the_terminal_is_silent`'s timing
/// assumptions (and leaking test state into the real cache to boot). Deriving the path from the
/// fixture's workdir means repeat `spawn_review` calls against the SAME fixture share one cache
/// file, while different fixtures — different tempdirs — never collide.
pub fn spawn_review(fixture: &Fixture) -> Session<OsProcess, OsStream> {
    let workdir = fixture_workdir(fixture);
    let probe_cache = probe_cache_path(fixture);

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_git-workon-review"));
    cmd.current_dir(&workdir)
        .env("TERM", "xterm-256color")
        .env("WORKON_REVIEW_PROBE_CACHE", &probe_cache);

    let mut session = expectrl::Session::spawn(cmd).expect("spawn in PTY");
    session
        .get_process_mut()
        .set_window_size(120, 40)
        .expect("size PTY");
    session.set_expect_timeout(Some(Duration::from_secs(15)));
    session
}

/// The probe-cache file [`spawn_review`] pins for `fixture` — one shared definition so a test
/// that inspects what the binary recorded reads the same path the spawn wired up.
pub fn probe_cache_path(fixture: &Fixture) -> std::path::PathBuf {
    fixture_workdir(fixture).join(".git-workon-review-probe-cache.json")
}

fn fixture_workdir(fixture: &Fixture) -> std::path::PathBuf {
    let repo = fixture.repo().expect("fixture repo");
    let workdir = repo.workdir().expect("fixture workdir");
    workdir.to_path_buf()
}
