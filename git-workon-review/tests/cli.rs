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

/// The binary answers the `COMPLETE=<shell>` dynamic-completion protocol (clap_complete's
/// `CompleteEnv`), so git-workon can delegate `git workon review <TAB>` completion to it (M6 CS3).
/// The review `Cli` has no args of its own yet, so the completer generates no candidates and exits
/// 2 ("no completion generated") — but the load-bearing contract is that `COMPLETE` mode
/// short-circuits into the completer *before* repository discovery or the TUI. Running from an
/// empty non-repo dir makes that concrete: an unwired binary would instead fail repo discovery;
/// getting clap_complete's own exit path proves the responder is in place. When M7 gives the
/// binary a real subcommand (`mcp`), upgrade this to assert that candidate.
#[test]
fn responds_to_complete_env_protocol_before_repo_discovery() {
    let non_repo = assert_fs::TempDir::new().unwrap();
    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.env("COMPLETE", "bash")
        .current_dir(&non_repo)
        .args(["--", "git-workon-review", ""])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("completion"));
}
