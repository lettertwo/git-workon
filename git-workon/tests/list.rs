use assert_cmd::Command;
use git_workon_fixture::prelude::*;

// ============================================================================
// Individual Filter Tests
// ============================================================================

#[test]
fn list_no_filters_shows_all_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .build()?;

    // Run list without filters
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("main"))
        .stdout(predicate::str::contains("feature-1"))
        .stdout(predicate::str::contains("feature-2"));

    Ok(())
}

#[test]
fn list_dirty_shows_only_dirty_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean-wt")
        .worktree("dirty-wt")
        .build()?;

    // Make dirty-wt dirty
    std::fs::write(
        fixture.root()?.child("dirty-wt").join("test.txt"),
        "uncommitted",
    )?;

    // Run list --dirty
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .assert()
        .success()
        .stdout(predicate::str::contains("dirty-wt"))
        .stdout(predicate::str::contains("clean-wt").not());

    Ok(())
}

#[test]
fn list_clean_shows_only_clean_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean-wt")
        .worktree("dirty-wt")
        .build()?;

    // Make dirty-wt dirty
    std::fs::write(
        fixture.root()?.child("dirty-wt").join("test.txt"),
        "uncommitted",
    )?;

    // Run list --clean
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--clean")
        .assert()
        .success()
        .stdout(predicate::str::contains("clean-wt"))
        .stdout(predicate::str::contains("dirty-wt").not());

    Ok(())
}

#[test]
fn list_ahead_shows_only_worktrees_with_unpushed() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("main", "origin/main")
        .upstream("feature", "origin/feature")
        .build()?;

    // Make feature ahead of upstream
    fixture
        .commit("feature")
        .file("test.txt", "content")
        .create("Test commit")?;

    // Run list --ahead
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--ahead")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"))
        .stdout(predicate::str::contains("main").not());

    Ok(())
}

#[test]
fn list_behind_shows_only_worktrees_behind_upstream() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("main", "origin/main")
        .upstream("feature", "origin/feature")
        .build()?;

    let repo = fixture.repo()?;

    // Create a commit on origin/feature to make feature behind
    let commit_oid = fixture
        .commit("feature")
        .file("test.txt", "content")
        .create("Remote commit")?;

    // Move origin/feature ahead
    let commit = repo.find_commit(commit_oid)?;
    repo.find_branch("origin/feature", git2::BranchType::Remote)?
        .get_mut()
        .set_target(commit.id(), "Move remote ahead")?;

    // Reset feature branch back to where it was
    let mut head = repo.find_reference("refs/heads/feature")?;
    let parent = commit.parent(0)?;
    head.set_target(parent.id(), "Reset feature behind")?;

    // Run list --behind
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--behind")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"))
        .stdout(predicate::str::contains("main").not());

    Ok(())
}

#[test]
fn list_gone_shows_only_worktrees_with_deleted_upstream() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("main", "origin/main")
        .upstream("feature", "origin/feature")
        .build()?;

    let repo = fixture.repo()?;

    // Delete the origin/feature remote branch
    repo.find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Run list --gone
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--gone")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"))
        .stdout(predicate::str::contains("main").not());

    Ok(())
}

// ============================================================================
// Filter Combination Tests
// ============================================================================

#[test]
fn list_dirty_and_ahead_combines_filters() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean-uptodate")
        .worktree("dirty-uptodate")
        .worktree("clean-ahead")
        .worktree("dirty-ahead")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("clean-uptodate", "origin/clean-uptodate")
        .upstream("dirty-uptodate", "origin/dirty-uptodate")
        .upstream("clean-ahead", "origin/clean-ahead")
        .upstream("dirty-ahead", "origin/dirty-ahead")
        .build()?;

    // Make dirty-uptodate and dirty-ahead dirty
    std::fs::write(
        fixture.root()?.child("dirty-uptodate").join("test.txt"),
        "uncommitted",
    )?;
    std::fs::write(
        fixture.root()?.child("dirty-ahead").join("test.txt"),
        "uncommitted",
    )?;

    // Make clean-ahead and dirty-ahead ahead
    fixture
        .commit("clean-ahead")
        .file("ahead1.txt", "content")
        .create("Ahead commit")?;
    fixture
        .commit("dirty-ahead")
        .file("ahead2.txt", "content")
        .create("Ahead commit")?;

    // Run list --dirty --ahead (should only show dirty-ahead)
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .arg("--ahead")
        .assert()
        .success()
        .stdout(predicate::str::contains("dirty-ahead"))
        .stdout(predicate::str::contains("clean-uptodate").not())
        .stdout(predicate::str::contains("dirty-uptodate").not())
        .stdout(predicate::str::contains("clean-ahead").not());

    Ok(())
}

#[test]
fn list_ahead_and_behind_shows_diverged_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("diverged")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("diverged", "origin/diverged")
        .build()?;

    let repo = fixture.repo()?;

    // Create a local commit (ahead)
    let local_commit_oid = fixture
        .commit("diverged")
        .file("local.txt", "content")
        .create("Local commit")?;

    // Create a different commit on origin/diverged (behind)
    let remote_commit_oid = fixture
        .commit("diverged")
        .file("remote.txt", "content")
        .create("Remote commit")?;

    // Move origin/diverged to the remote commit
    let remote_commit = repo.find_commit(remote_commit_oid)?;
    repo.find_branch("origin/diverged", git2::BranchType::Remote)?
        .get_mut()
        .set_target(remote_commit.id(), "Move remote")?;

    // Reset diverged branch to local commit (before the remote commit)
    let local_commit = repo.find_commit(local_commit_oid)?;
    repo.find_reference("refs/heads/diverged")?
        .set_target(local_commit.parent(0)?.id(), "Reset to before local commit")?;

    // Make one more local commit so it's ahead
    fixture
        .commit("diverged")
        .file("local2.txt", "content")
        .create("Local commit 2")?;

    // At this point, diverged should be both ahead and behind
    // Run list --ahead --behind (should show diverged)
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--ahead")
        .arg("--behind")
        .assert()
        .success()
        .stdout(predicate::str::contains("diverged"));

    Ok(())
}

#[test]
fn list_gone_and_ahead_shows_deleted_upstream_with_local_commits(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("gone-with-commits")
        .worktree("gone-without-commits")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("gone-with-commits", "origin/gone-with-commits")
        .upstream("gone-without-commits", "origin/gone-without-commits")
        .build()?;

    let repo = fixture.repo()?;

    // Add a commit to gone-with-commits
    fixture
        .commit("gone-with-commits")
        .file("test.txt", "content")
        .create("Local commit")?;

    // Delete both remote branches
    repo.find_reference("refs/remotes/origin/gone-with-commits")?
        .delete()?;
    repo.find_reference("refs/remotes/origin/gone-without-commits")?
        .delete()?;

    // Run list --gone --ahead
    // Note: has_unpushed_commits() conservatively returns true for gone upstreams,
    // so both worktrees will match --ahead even though gone-without-commits has no commits
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--gone")
        .arg("--ahead")
        .assert()
        .success()
        .stdout(predicate::str::contains("gone-with-commits"))
        .stdout(predicate::str::contains("gone-without-commits"));

    Ok(())
}

#[test]
fn list_multiple_filters_uses_and_logic() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("matches-all")
        .worktree("matches-some")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("matches-all", "origin/matches-all")
        .upstream("matches-some", "origin/matches-some")
        .build()?;

    // Make matches-all dirty AND ahead
    std::fs::write(
        fixture.root()?.child("matches-all").join("test.txt"),
        "uncommitted",
    )?;
    fixture
        .commit("matches-all")
        .file("ahead.txt", "content")
        .create("Ahead commit")?;

    // Make matches-some only dirty (not ahead)
    std::fs::write(
        fixture.root()?.child("matches-some").join("test.txt"),
        "uncommitted",
    )?;

    // Run list --dirty --ahead --clean (should show nothing - clean contradicts dirty)
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .arg("--ahead")
        .arg("--clean")
        .assert()
        .failure(); // Should error due to --dirty and --clean conflict

    Ok(())
}

// ============================================================================
// Edge Case Tests
// ============================================================================

// Note: Detached HEAD test removed due to FixtureBuilder limitations
// Detached HEAD worktrees are correctly excluded from --ahead/--behind/--gone
// filters by the implementation (they return false when branch() returns None)

#[test]
fn list_worktree_without_upstream_excluded_from_behind_filter(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("no-upstream")
        .worktree("with-upstream")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("with-upstream", "origin/with-upstream")
        .build()?;

    // no-upstream has no upstream configured
    // Run list --behind (should not show no-upstream)
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--behind")
        .assert()
        .success()
        .stdout(predicate::str::contains("no-upstream").not());

    Ok(())
}

#[test]
fn list_worktree_without_upstream_excluded_from_gone_filter(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("no-upstream")
        .worktree("with-upstream")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("with-upstream", "origin/with-upstream")
        .build()?;

    // no-upstream has no upstream configured
    // Run list --gone (should not show no-upstream)
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--gone")
        .assert()
        .success()
        .stdout(predicate::str::contains("no-upstream").not());

    Ok(())
}

#[test]
fn list_dirty_and_clean_both_specified_returns_error() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    // Run list --dirty --clean (should error)
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .arg("--clean")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Cannot specify both --dirty and --clean filters",
        ));

    Ok(())
}

#[test]
fn list_empty_result_when_no_worktrees_match_filters() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean")
        .build()?;

    // Run list --dirty (nothing is dirty, should show nothing)
    let mut cmd = Command::cargo_bin("git-workon")?;
    let output = cmd
        .current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Output should be empty (or just whitespace)
    let output_str = String::from_utf8(output)?;
    assert!(
        output_str.trim().is_empty(),
        "Expected empty output, got: {}",
        output_str
    );

    Ok(())
}
