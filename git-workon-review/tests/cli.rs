use assert_cmd::cargo_bin_cmd;
use git_workon_fixture::prelude::*;

/// Locked design decision #7 (M3 plan): a clean worktree prints "nothing to review" to stderr
/// and exits 0 without ever entering the TUI — no raw-mode/alternate-screen setup, so this stays
/// a plain `assert_cmd` invocation (no PTY needed).
#[test]
fn clean_worktree_prints_nothing_to_review_and_exits_success() {
    let fixture = FixtureBuilder::new().build().unwrap();
    let repo = fixture.repo().unwrap();
    let workdir = repo.workdir().unwrap();

    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.current_dir(workdir)
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("nothing to review"));
}

#[test]
fn help_shows_usage_and_succeeds() {
    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("git-workon-review"));
}
