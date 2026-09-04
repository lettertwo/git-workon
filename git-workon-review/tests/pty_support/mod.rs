//! Shared PTY-test support for the `pty_smoke` and `pty_responsiveness` test binaries.
//!
//! A `tests/<name>/mod.rs` directory module so cargo does not build it as a test binary of its
//! own; each PTY suite declares `mod pty_support;`. Keeping the spawn setup in one place means
//! a change to the window size, `TERM`, or expect timeout applies to every PTY suite at once —
//! the two suites guard related regressions, so silent drift here would matter.

use std::time::Duration;

use expectrl::{
    session::{OsProcess, OsStream},
    Session,
};
use git_workon_fixture::prelude::*;

/// Spawn the review binary in a PTY sized like a real terminal (an unsized PTY is 0×0 and
/// ratatui draws nothing), cwd'd into the fixture's worktree.
pub fn spawn_review(fixture: &Fixture) -> Session<OsProcess, OsStream> {
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
