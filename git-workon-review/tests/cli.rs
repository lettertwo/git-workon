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

/// Drive clap_complete's dynamic `COMPLETE=bash` protocol for `git-workon-review <words>` (mirrors
/// `git-workon/tests/completions.rs`'s `bash_candidates` helper) and return the emitted candidate
/// values, one per line (no `_CLAP_IFS` override means `write_complete` falls back to `\n`).
fn bash_candidates(cwd: &std::path::Path, word: &str) -> Vec<String> {
    let output = cargo_bin_cmd!("git-workon-review")
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "1")
        .current_dir(cwd)
        .args(["--", "git-workon-review", word])
        .output()
        .expect("completion invocation");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .collect()
}

/// ADR-030 "Completion" section (CS5): the `stack`/`uncommitted` keywords plus local branch and
/// tag names, offline, via git2 ref enumeration — this is the SOURCE positional's dynamic
/// completer, exercised through the same `COMPLETE=bash` protocol M6 wired the binary to answer.
#[test]
fn source_completion_offers_keywords_and_local_refs() {
    let fixture = FixtureBuilder::new()
        .default_branch("main")
        .branch("feature-x")
        .build()
        .unwrap();
    let repo = fixture.repo().unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.tag_lightweight("v1", head.as_object(), false).unwrap();

    let candidates = bash_candidates(repo.workdir().unwrap(), "");

    assert!(candidates.contains(&"stack".to_string()), "{candidates:?}");
    assert!(
        candidates.contains(&"uncommitted".to_string()),
        "{candidates:?}"
    );
    assert!(
        candidates.contains(&"feature-x".to_string()),
        "{candidates:?}"
    );
    assert!(candidates.contains(&"v1".to_string()), "{candidates:?}");
}

/// A word containing `..`/`...` only completes the right-hand ref, reassembled with the
/// left-hand text (dots included) so shell prefix-matching keeps working on the whole word.
#[test]
fn source_completion_completes_range_rhs_with_lhs_prefix() {
    let fixture = FixtureBuilder::new()
        .default_branch("main")
        .branch("feature-x")
        .build()
        .unwrap();
    let repo = fixture.repo().unwrap();

    let candidates = bash_candidates(repo.workdir().unwrap(), "main..fe");

    assert!(
        candidates.contains(&"main..feature-x".to_string()),
        "{candidates:?}"
    );
    // Never a bare ref without the `main..` prefix, and never a keyword after a dot-range.
    assert!(
        !candidates.contains(&"feature-x".to_string()),
        "{candidates:?}"
    );
    assert!(!candidates.contains(&"stack".to_string()), "{candidates:?}");
}

/// The binary answers the `COMPLETE=<shell>` dynamic-completion protocol (clap_complete's
/// `CompleteEnv`), so git-workon can delegate `git workon review <TAB>` completion to it (M6 CS3).
/// A non-repo cwd degrades to keyword-only candidates (ADR-030: any git error → keywords only,
/// never a completion-path error) rather than failing repo discovery — the load-bearing contract
/// is that `COMPLETE` mode short-circuits into the completer *before* that discovery even runs.
#[test]
fn non_repo_cwd_completes_keywords_only_without_error() {
    let non_repo = assert_fs::TempDir::new().unwrap();

    let candidates = bash_candidates(&non_repo, "");

    assert!(candidates.contains(&"stack".to_string()), "{candidates:?}");
    assert!(
        candidates.contains(&"uncommitted".to_string()),
        "{candidates:?}"
    );
    // No ref candidates from a non-repo cwd — only the two keywords (plus clap's own
    // `--help`/`--version`, unrelated to the ref-enumeration arm under test here).
    assert_eq!(
        candidates
            .iter()
            .filter(|c| !c.starts_with('-'))
            .cloned()
            .collect::<Vec<_>>(),
        vec!["stack".to_string(), "uncommitted".to_string()],
        "a non-repo cwd must never surface ref candidates"
    );
}
