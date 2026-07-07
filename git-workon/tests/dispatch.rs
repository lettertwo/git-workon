//! External-subcommand dispatch: `git workon <name> [args...]` → exec `git-workon-<name>` on
//! `$PATH`. See `git-workon/src/dispatch.rs` for the manual pre-parse intercept this exercises.

use assert_cmd::cargo_bin_cmd;
use assert_fs::TempDir;
use git_workon_fixture::prelude::*;

/// Pull the `cwd:<path>` line the stub prints and canonicalize it, so comparisons are robust to
/// `/tmp` vs `/private/tmp`-style symlink differences across platforms.
fn stub_cwd(stdout: &str) -> std::path::PathBuf {
    let raw = stdout
        .lines()
        .find_map(|l| l.strip_prefix("cwd:"))
        .expect("stub output must include a cwd: line");
    std::path::Path::new(raw)
        .canonicalize()
        .expect("stub-reported cwd must exist")
}

#[test]
fn dispatch_execs_external_with_passthrough_args() -> Result<(), Box<dyn std::error::Error>> {
    let stub = PathStub::new()?.command("foo")?;
    let cwd = TempDir::new()?;

    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&cwd)
        .env("PATH", stub.path())
        .arg("foo")
        .arg("a")
        .arg("b")
        .output()?;

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("arg:a"), "stdout was: {stdout}");
    assert!(stdout.contains("arg:b"), "stdout was: {stdout}");
    assert_eq!(stub_cwd(&stdout), cwd.path().canonicalize()?);

    Ok(())
}

#[test]
fn dispatch_flag_passthrough_does_not_get_eaten() -> Result<(), Box<dyn std::error::Error>> {
    let stub = PathStub::new()?.command("foo")?;
    let cwd = TempDir::new()?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&cwd)
        .env("PATH", stub.path())
        .arg("foo")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("arg:--help"));

    Ok(())
}

#[test]
fn no_matching_external_falls_through_to_find() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    // Scrub PATH to an empty dir so no `git-workon-*` external is discoverable — otherwise this
    // is non-deterministic: a developer of this very tool (or anyone who has run `cargo install
    // --path git-workon-review`, or once it's released) has `git-workon-review` on PATH, which
    // would make `git workon review` dispatch instead of fall through. With nothing to dispatch
    // to, `review` isn't a known subcommand, so the intercept misses and falls through to normal
    // parsing → `find.name` → find's own error.
    let empty_path = TempDir::new()?;
    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .env("PATH", empty_path.path())
        .arg("review")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No matching worktree found"));

    Ok(())
}

#[test]
fn builtin_subcommand_takes_precedence_over_same_named_external(
) -> Result<(), Box<dyn std::error::Error>> {
    let stub = PathStub::new()?.command("list")?;
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    // A stray `git-workon-list` on PATH must never shadow the built-in `list` subcommand.
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .arg("list")
        .output()?;

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        !stdout.contains("cwd:"),
        "the git-workon-list stub ran instead of the built-in: {stdout}"
    );
    assert!(stdout.contains("main"), "stdout was: {stdout}");

    Ok(())
}

#[test]
fn external_shadows_same_named_branch_but_find_still_reaches_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let stub = PathStub::new()?.command("review")?;
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .branch("review") // branch exists but has no worktree of its own
        .build()?;

    // `git workon review` dispatches to the external even though a branch named `review`
    // exists — installing the tool must guarantee this reaches it (locked decision 2).
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .arg("review")
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("cwd:"), "external did not run: {stdout}");

    // The explicit `find` subcommand is unaffected by the external's presence: `find` is a
    // known built-in, so the intercept never even considers `git-workon-review`. `find` runs
    // its normal branch-vs-worktree logic (no worktree exists for `review`, so it reports that
    // rather than silently dispatching).
    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .arg("find")
        .arg("review")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "No matching worktree found for 'review'",
        ));

    Ok(())
}

#[test]
fn leading_dash_flag_is_not_treated_as_dispatch() -> Result<(), Box<dyn std::error::Error>> {
    // Even with a `git-workon---version` stub (an absurd name, but proves the point: a leading
    // dash on the first token is always treated as a global flag, never looked up on PATH).
    cargo_bin_cmd!("git-workon")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("git-workon"));

    Ok(())
}
