use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::cargo_bin_cmd;
use expectrl::{
    session::{OsProcess, OsStream},
    Expect, Session,
};
use git_workon_fixture::prelude::*;

#[test]
fn prune_with_no_stale_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Run prune - should report nothing to prune
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("No worktrees to prune"));

    Ok(())
}

#[test]
fn prune_removes_worktree_for_deleted_branch() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize a repository with a 'feature' worktree
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Verify worktree exists
    let feature_dir = fixture.cwd()?;
    feature_dir.assert(predicate::path::is_dir());

    // Delete the branch manually
    let repo = fixture.repo()?;
    repo.find_reference("refs/heads/feature")?.delete()?;

    // Run prune - should remove the worktree
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify worktree directory is gone
    feature_dir.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_dry_run_does_not_remove_anything() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize a repository with a 'feature' worktree
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Verify worktree exists
    let feature_dir = fixture.cwd()?;
    feature_dir.assert(predicate::path::is_dir());

    // Delete the branch manually
    let repo = fixture.repo()?;
    repo.find_reference("refs/heads/feature")?.delete()?;

    // Run prune with --dry-run
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Dry run - no changes made"));

    // Verify worktree still exists
    feature_dir.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_handles_multiple_stale_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize a repository with a 'feature' worktree
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .worktree("feature-3")
        .build()?;

    let repo = fixture.repo()?;

    // Delete all the feature branches
    for name in &["feature-1", "feature-2", "feature-3"] {
        // Verify worktree dir exists
        fixture
            .root()?
            .child(name)
            .assert(predicate::path::is_dir());

        // Manually delete the branch
        repo.find_reference(&format!("refs/heads/{}", name))?
            .delete()?;
    }

    // Run prune
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 3 worktree"));

    // Verify all worktrees are gone
    for name in &["feature-1", "feature-2", "feature-3"] {
        fixture
            .root()?
            .child(name)
            .assert(predicate::path::missing());
    }

    Ok(())
}

#[test]
fn prune_preserves_worktrees_with_existing_branches() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("keep-me")
        .worktree("delete-me")
        .build()?;

    // Delete only one branch
    fixture
        .repo()?
        .find_reference("refs/heads/delete-me")?
        .delete()?;

    // Run prune
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify delete-me is gone
    fixture
        .root()?
        .child("delete-me")
        .assert(predicate::path::missing());

    // Verify keep-me still exists
    fixture
        .root()?
        .child("keep-me")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_with_gone_flag_removes_worktrees_with_deleted_remote_branch(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    // Delete reference to remote branch
    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Run prune without --gone - should NOT remove the worktree
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("No worktrees to prune"));

    // Verify worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    // Run prune with --gone - should remove the worktree (feature is merged into main)
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"))
        .stderr(predicate::str::contains("remote gone"));

    // Verify worktree directory is gone
    fixture.cwd()?.assert(predicate::path::missing());

    // Verify local branch ref was also deleted
    fixture.assert(predicate::repo::has_branch("feature").not());

    Ok(())
}

#[test]
fn prune_gone_skips_branches_without_upstream() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Run prune with --gone - should not remove worktree without upstream
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("No worktrees to prune"));

    // Verify worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_gone_dry_run() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    // Delete reference to remote branch
    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Run prune with --gone and --dry-run (feature is merged into main, no --allow-unmerged needed)
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Dry run - no changes made"))
        .stderr(predicate::str::contains("remote gone"));

    // Verify worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_skips_dirty_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Create a file in the worktree (uncommitted change)
    std::fs::write(fixture.cwd()?.join("test.txt"), "test content")?;

    // Delete the branch
    fixture
        .repo()?
        .find_reference("refs/heads/feature")?
        .delete()?;

    // Run prune - should skip dirty worktree
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped worktrees"))
        .stderr(predicate::str::contains("uncommitted changes"))
        .stderr(predicate::str::contains("No worktrees to prune"));

    // Verify worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_with_allow_dirty_removes_dirty_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Create a file in the worktree (uncommitted change)
    std::fs::write(fixture.cwd()?.join("test.txt"), "test content")?;

    // Delete the branch
    fixture
        .repo()?
        .find_reference("refs/heads/feature")?
        .delete()?;

    // Run prune with --allow-dirty
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--allow-dirty")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify worktree is gone
    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_gone_removes_worktrees_with_unmerged_commits() -> Result<(), Box<dyn std::error::Error>> {
    // A gone upstream is strong evidence the PR was merged/closed upstream.
    // prune --gone should not require --allow-unmerged in this case.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    // Delete reference to remote branch (simulate upstream gone after PR merge)
    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Create a new commit in the worktree (not reachable from main via graph)
    fixture
        .commit("feature")
        .file("test.txt", "test")
        .create("New commit")?;

    // Run prune --gone (without --allow-unmerged) — should succeed without skipping
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify worktree was pruned
    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_gone_with_allow_unmerged_removes_worktrees_with_unmerged_commits(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    // Delete reference to remote branch
    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Create a new commit in the worktree (unmerged into main)
    fixture
        .commit("feature")
        .file("test.txt", "test")
        .create("New commit")?;

    // Run prune --gone with --allow-unmerged
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--allow-unmerged")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify worktree is gone
    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_merged_removes_merged_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let feature_commit_oid = fixture
        .commit("feature")
        .file("feature.txt", "feature")
        .create("Feature commit")?;

    // Fast-forward main to include the feature commit (simulating merge)
    let repo = fixture.repo()?;
    let feature_commit = repo.find_commit(feature_commit_oid)?;
    repo.find_branch("main", git2::BranchType::Local)?
        .get_mut()
        .set_target(feature_commit.id(), "Fast-forward to feature")?;

    // Run prune --merged
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--merged")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"))
        .stderr(predicate::str::contains("merged into main"));

    // Verify worktree is gone
    fixture.cwd()?.assert(predicate::path::missing());

    // Verify local branch ref was also deleted
    fixture.assert(predicate::repo::has_branch("feature").not());

    Ok(())
}

#[test]
fn prune_merged_skips_unmerged_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    fixture
        .commit("feature")
        .file("feature.txt", "feature")
        .create("Feature commit")?;

    // Run prune --merged (should not prune unmerged branch)
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--merged")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("No worktrees to prune"));

    // Verify worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_merged_with_specific_target() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("develop")
        .worktree("feature")
        .build()?;

    let feature_commit_oid = fixture
        .commit("feature")
        .file("feature.txt", "feature")
        .create("Feature commit")?;

    // Fast-forward develop to include the feature commit
    let repo = fixture.repo()?;
    let feature_commit = repo.find_commit(feature_commit_oid)?;
    repo.find_branch("develop", git2::BranchType::Local)?
        .get_mut()
        .set_target(feature_commit.id(), "Fast-forward to feature")?;

    // Run prune --merged=develop
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--merged=develop")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"))
        .stderr(predicate::str::contains("merged into develop"));

    // Verify feature worktree is gone
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());

    // Verify main worktree still exists
    fixture
        .root()?
        .child("main")
        .assert(predicate::path::is_dir());

    // Verify develop worktree still exists
    fixture
        .root()?
        .child("develop")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_skips_protected_branch_exact_match() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .config("workon.pruneProtectedBranches", "develop")
        .build()?;

    let develop_dir = fixture.cwd()?;
    develop_dir.assert(predicate::path::is_dir());

    // Delete the develop branch to make it a prune candidate
    let repo = fixture.repo()?;
    repo.find_reference("refs/heads/develop")?.delete()?;

    // Run prune - should skip protected branch
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped"))
        .stderr(predicate::str::contains(
            "protected by workon.pruneProtectedBranches",
        ));

    // Verify worktree still exists
    develop_dir.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_skips_protected_branch_with_glob_pattern() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .config("workon.pruneProtectedBranches", "release/*")
        .build()?;

    let repo = fixture.repo()?;

    // Manually create worktrees with slashes using add_worktree
    use workon::{add_worktree, BranchType};
    add_worktree(repo, "release/1.0", None, BranchType::Normal, None, false)?;
    add_worktree(repo, "release/2.0", None, BranchType::Normal, None, false)?;
    add_worktree(repo, "feature/test", None, BranchType::Normal, None, false)?;

    // Delete all branches to make them prune candidates
    repo.find_reference("refs/heads/release/1.0")?.delete()?;
    repo.find_reference("refs/heads/release/2.0")?.delete()?;
    repo.find_reference("refs/heads/feature/test")?.delete()?;

    // Run prune - should skip release/* but prune feature/test
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped"))
        .stderr(predicate::str::contains("release/1.0"))
        .stderr(predicate::str::contains("release/2.0"))
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify release worktrees still exist
    fixture
        .root()?
        .child("release/1.0")
        .assert(predicate::path::is_dir());
    fixture
        .root()?
        .child("release/2.0")
        .assert(predicate::path::is_dir());

    // Verify feature worktree is gone
    fixture
        .root()?
        .child("feature/test")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_skips_protected_branch_with_dashed_glob_pattern() -> Result<(), Box<dyn std::error::Error>>
{
    // Regression test: a pattern without a slash (e.g. "release-*") used to silently
    // match nothing under the old hand-rolled glob_match, meaning the branch got no
    // protection at all. glob::Pattern must match it correctly.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .config("workon.pruneProtectedBranches", "release-*")
        .build()?;

    let repo = fixture.repo()?;

    use workon::{add_worktree, BranchType};
    add_worktree(repo, "release-v3", None, BranchType::Normal, None, false)?;
    add_worktree(repo, "feature/test", None, BranchType::Normal, None, false)?;

    // Delete all branches to make them prune candidates
    repo.find_reference("refs/heads/release-v3")?.delete()?;
    repo.find_reference("refs/heads/feature/test")?.delete()?;

    // Run prune - should skip release-v3 but prune feature/test
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped"))
        .stderr(predicate::str::contains("release-v3"))
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify release-v3 worktree still exists
    fixture
        .root()?
        .child("release-v3")
        .assert(predicate::path::is_dir());

    // Verify feature worktree is gone
    fixture
        .root()?
        .child("feature/test")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_rejects_invalid_protected_pattern() -> Result<(), Box<dyn std::error::Error>> {
    // Malformed patterns must fail loudly (naming the pattern) rather than silently
    // matching nothing, which would leave every branch unprotected.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .config("workon.pruneProtectedBranches", "[")
        .build()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .failure()
        .stderr(predicate::str::contains("["))
        .stderr(predicate::str::contains("workon.pruneProtectedBranches"));

    Ok(())
}

#[test]
fn prune_respects_multiple_protected_patterns() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .worktree("staging")
        .config("workon.pruneProtectedBranches", "develop")
        .config("workon.pruneProtectedBranches", "staging")
        .config("workon.pruneProtectedBranches", "release/*")
        .build()?;

    let repo = fixture.repo()?;

    // Manually create worktrees with slashes using add_worktree
    use workon::{add_worktree, BranchType};
    add_worktree(repo, "release/1.0", None, BranchType::Normal, None, false)?;
    add_worktree(repo, "feature/test", None, BranchType::Normal, None, false)?;

    // Delete all branches to make them prune candidates
    repo.find_reference("refs/heads/develop")?.delete()?;
    repo.find_reference("refs/heads/staging")?.delete()?;
    repo.find_reference("refs/heads/release/1.0")?.delete()?;
    repo.find_reference("refs/heads/feature/test")?.delete()?;

    // Run prune - should skip all protected branches but prune feature/test
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped"))
        .stderr(predicate::str::contains("develop"))
        .stderr(predicate::str::contains("staging"))
        .stderr(predicate::str::contains("release/1.0"))
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify protected worktrees still exist
    fixture
        .root()?
        .child("develop")
        .assert(predicate::path::is_dir());
    fixture
        .root()?
        .child("staging")
        .assert(predicate::path::is_dir());
    fixture
        .root()?
        .child("release/1.0")
        .assert(predicate::path::is_dir());

    // Verify feature worktree is gone
    fixture
        .root()?
        .child("feature/test")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_without_protected_config_prunes_all_candidates() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .build()?;

    let repo = fixture.repo()?;

    // Delete both branches
    repo.find_reference("refs/heads/feature-1")?.delete()?;
    repo.find_reference("refs/heads/feature-2")?.delete()?;

    // Run prune without any protection config - should prune both
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 2 worktree"));

    // Verify both worktrees are gone
    fixture
        .root()?
        .child("feature-1")
        .assert(predicate::path::missing());
    fixture
        .root()?
        .child("feature-2")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_single_named_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .build()?;

    // Prune feature-1 by name
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature-1")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"))
        // A worktree fresh off the default branch has no unique commits, so it is
        // trivially "merged into main" — that signal is always computed and shown,
        // even though naming bypasses the --merged flag gate.
        .stderr(predicate::str::contains("merged into main"));

    // Verify feature-1 is gone, feature-2 still exists
    fixture
        .root()?
        .child("feature-1")
        .assert(predicate::path::missing());
    fixture
        .root()?
        .child("feature-2")
        .assert(predicate::path::is_dir());

    // Verify branch ref was deleted
    fixture.assert(predicate::repo::has_branch("feature-1").not());
    fixture.assert(predicate::repo::has_branch("feature-2"));

    Ok(())
}

#[test]
fn prune_multiple_named_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .worktree("feature-3")
        .build()?;

    // Prune feature-1 and feature-2 by name
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature-1")
        .arg("feature-2")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 2 worktree"));

    // Verify feature-1 and feature-2 are gone, feature-3 still exists
    fixture
        .root()?
        .child("feature-1")
        .assert(predicate::path::missing());
    fixture
        .root()?
        .child("feature-2")
        .assert(predicate::path::missing());
    fixture
        .root()?
        .child("feature-3")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_duplicate_names_prune_once() -> Result<(), Box<dyn std::error::Error>> {
    // Repeating a name must dedupe to a single prune, not fail on the second pass
    // after the first deregistered the worktree.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("feature")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());
    fixture.assert(predicate::repo::has_worktree("feature").not());
    fixture.assert(predicate::repo::has_branch("feature").not());

    Ok(())
}

#[test]
fn prune_named_worktree_narrows_never_additive() -> Result<(), Box<dyn std::error::Error>> {
    // Naming strictly narrows the scope: an unnamed worktree with a deleted branch
    // (which would be a bare-mode candidate) must NOT be swept in just because some
    // other worktree was named on the same invocation.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .worktree("feature-3")
        .build()?;

    let repo = fixture.repo()?;

    // Delete feature-2's branch — a bare-mode candidate, but not named here.
    repo.find_reference("refs/heads/feature-2")?.delete()?;

    // Prune feature-1 by name only.
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature-1")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify feature-1 is gone, feature-2 and feature-3 are untouched.
    fixture
        .root()?
        .child("feature-1")
        .assert(predicate::path::missing());
    fixture
        .root()?
        .child("feature-2")
        .assert(predicate::path::is_dir());
    fixture
        .root()?
        .child("feature-3")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_named_worktree_not_found() -> Result<(), Box<dyn std::error::Error>> {
    // An unmatched name is a hard error (nonzero exit) — nothing is touched, even if
    // other names on the same invocation did match.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .build()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("does-not-exist")
        .arg("--yes")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "worktree(s) not found: does-not-exist",
        ));

    // Verify feature-1 still exists
    fixture
        .root()?
        .child("feature-1")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_named_worktree_lists_all_misses() -> Result<(), Box<dyn std::error::Error>> {
    // Multiple unmatched names are all listed together, and even a name that DOES
    // match is left untouched because the whole invocation fails before pruning.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .build()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature-1")
        .arg("missing-one")
        .arg("missing-two")
        .arg("--yes")
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing-one"))
        .stderr(predicate::str::contains("missing-two"));

    fixture
        .root()?
        .child("feature-1")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_named_worktree_respects_protected_branches() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .worktree("feature")
        .config("workon.pruneProtectedBranches", "develop")
        .build()?;

    // Try to prune protected branch by name
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("develop")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped"))
        .stderr(predicate::str::contains(
            "protected by workon.pruneProtectedBranches",
        ))
        .stderr(predicate::str::contains("No worktrees to prune"));

    // Verify develop still exists
    fixture
        .root()?
        .child("develop")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_named_worktree_respects_dirty_check() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Make feature worktree dirty
    let feature_dir = fixture.cwd()?;
    std::fs::write(feature_dir.join("dirty.txt"), "uncommitted")?;

    // Try to prune dirty worktree by name
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped"))
        .stderr(predicate::str::contains(
            "has uncommitted changes, use --allow-dirty",
        ))
        .stderr(predicate::str::contains("No worktrees to prune"));

    // Verify feature still exists
    feature_dir.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_named_worktree_with_allow_dirty() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Make feature worktree dirty
    let feature_dir = fixture.cwd()?;
    std::fs::write(feature_dir.join("dirty.txt"), "uncommitted")?;

    // Prune dirty worktree with --allow-dirty
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--allow-dirty")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify feature is gone
    feature_dir.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_named_worktree_dry_run() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let feature_dir = fixture.cwd()?;

    // Dry run prune of named worktree
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Prune analysis"))
        .stderr(predicate::str::contains("feature"))
        .stderr(predicate::str::contains("Dry run - no changes made"));

    // Verify feature still exists
    feature_dir.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_force_overrides_protected_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .config("workon.pruneProtectedBranches", "develop")
        .build()?;

    let develop_dir = fixture.cwd()?;
    develop_dir.assert(predicate::path::is_dir());

    // Prune protected branch with --force
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("develop")
        .arg("--force")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify develop is gone
    develop_dir.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_force_overrides_dirty_check() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Make feature worktree dirty
    let feature_dir = fixture.cwd()?;
    std::fs::write(feature_dir.join("dirty.txt"), "uncommitted")?;

    // Prune dirty worktree with --force
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--force")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify feature is gone
    feature_dir.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_force_overrides_unmerged_check() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    // Delete reference to remote branch
    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Create a new commit in the worktree (unmerged into main)
    fixture
        .commit("feature")
        .file("test.txt", "test")
        .create("New commit")?;

    // Prune worktree with unmerged commits using --force
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--gone")
        .arg("--force")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify feature is gone
    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_default_worktree_never_matches_even_with_force() -> Result<(), Box<dyn std::error::Error>>
{
    // The default worktree is excluded from the candidate pool entirely — "never
    // appears" is unconditional, unlike the protected/locked guards which --force can
    // override. Naming it is therefore an unmatched-name error even with --force.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("main")
        .arg("--force")
        .arg("--yes")
        .assert()
        .failure()
        .stderr(predicate::str::contains("worktree(s) not found: main"));

    // Verify main worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

// --- Branch deletion tests ---

#[test]
fn prune_keep_branch_preserves_local_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Prune with --keep-branch
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--keep-branch")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify worktree directory is gone
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());

    // Verify branch ref is still present
    fixture.assert(predicate::repo::has_branch("feature"));

    Ok(())
}

#[test]
fn prune_deletes_branch_for_gone_upstream() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    // Delete reference to remote branch
    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Prune with --gone (default: delete branch)
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify local branch ref was deleted
    fixture.assert(predicate::repo::has_branch("feature").not());

    Ok(())
}

#[test]
fn prune_deletes_branch_for_merged() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let feature_commit_oid = fixture
        .commit("feature")
        .file("feature.txt", "feature")
        .create("Feature commit")?;

    // Fast-forward main to include the feature commit
    let repo = fixture.repo()?;
    let feature_commit = repo.find_commit(feature_commit_oid)?;
    repo.find_branch("main", git2::BranchType::Local)?
        .get_mut()
        .set_target(feature_commit.id(), "Fast-forward to feature")?;

    // Prune with --merged (default: delete branch)
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--merged")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify local branch ref was deleted
    fixture.assert(predicate::repo::has_branch("feature").not());

    Ok(())
}

#[test]
fn prune_dry_run_does_not_delete_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Dry run prune
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Dry run - no changes made"));

    // Verify branch ref still exists
    fixture.assert(predicate::repo::has_branch("feature"));

    Ok(())
}

#[test]
fn prune_json_includes_branch_deleted_field() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Prune in JSON mode (default: delete branch)
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    let output = prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--json")
        .arg("feature")
        .arg("--yes")
        .output()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout)?;
    let json: serde_json::Value = serde_json::from_str(&stdout)?;
    let pruned = json["pruned"].as_array().expect("pruned should be array");
    assert_eq!(pruned.len(), 1);
    assert_eq!(
        pruned[0]["branch_deleted"],
        serde_json::Value::Bool(true),
        "branch_deleted should be true"
    );

    Ok(())
}

#[test]
fn prune_skips_locked_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Lock the worktree
    let repo = fixture.repo()?;
    repo.find_worktree("feature")?.lock(None)?;

    // Delete the branch to make it a prune candidate
    repo.find_reference("refs/heads/feature")?.delete()?;

    // Run prune - should skip locked worktree
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped worktrees"))
        .stderr(predicate::str::contains("locked"))
        .stderr(predicate::str::contains("No worktrees to prune"));

    // Verify worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_include_locked_removes_locked_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Lock the worktree
    let repo = fixture.repo()?;
    repo.find_worktree("feature")?.lock(None)?;

    // Delete the branch to make it a prune candidate
    repo.find_reference("refs/heads/feature")?.delete()?;

    // Run prune with --include-locked - should prune despite lock
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .arg("--include-locked")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // Verify worktree is gone
    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

// --- Orphaned stash tests ---

fn create_labeled_stash(
    fixture: &Fixture,
    worktree: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let wt_path = fixture.root()?.child(worktree).path().to_path_buf();
    fixture
        .commit(worktree)
        .file("f.txt", "v1")
        .create("init")?;
    std::fs::write(wt_path.join("f.txt"), "local-edit")?;
    let mut wt_repo = git2::Repository::open(&wt_path)?;
    let sig = git2::Signature::now("test", "test@test.com")?;
    wt_repo.stash_save2(
        &sig,
        Some(&format!("workon-autostash: {} @ {}", worktree, worktree)),
        Some(git2::StashFlags::INCLUDE_UNTRACKED),
    )?;
    Ok(())
}

#[test]
fn prune_warns_about_orphaned_stashes() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    create_labeled_stash(&fixture, "feature")?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--allow-unmerged")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("orphans shelved changes"))
        .stderr(predicate::str::contains(
            "workon-autostash: feature @ feature",
        ))
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_json_includes_orphaned_stashes() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    create_labeled_stash(&fixture, "feature")?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    let output = prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--json")
        .arg("feature")
        .arg("--allow-unmerged")
        .arg("--yes")
        .output()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout)?;
    let json: serde_json::Value = serde_json::from_str(&stdout)?;
    let pruned = json["pruned"].as_array().expect("pruned should be array");
    assert_eq!(pruned.len(), 1);
    let stashes = pruned[0]["orphaned_stashes"]
        .as_array()
        .expect("orphaned_stashes should be array");
    assert!(!stashes.is_empty(), "expected at least one orphaned stash");
    assert!(
        stashes[0]
            .as_str()
            .unwrap_or("")
            .contains("workon-autostash: feature @ feature"),
        "stash label should match"
    );

    Ok(())
}

#[test]
fn prune_no_orphan_warning_when_no_stash() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("orphans shelved").not())
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());

    Ok(())
}

// --- Interactive PTY tests ---

fn cargo_bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_git-workon"))
}

fn spawn_interactive(cwd: &Path, args: &[&str]) -> Session<OsProcess, OsStream> {
    let mut cmd = std::process::Command::new(cargo_bin_path());
    cmd.current_dir(cwd);
    for arg in args {
        cmd.arg(arg);
    }
    let mut session = expectrl::Session::spawn(cmd).expect("Failed to spawn in PTY");
    session.set_expect_timeout(Some(Duration::from_secs(10)));
    session
}

#[test]
fn prune_interactive_picker_confirm_yes() -> Result<(), Box<dyn std::error::Error>> {
    // The picker replaces the old list+confirm flow: Enter accepts the pre-checked
    // defaults (a fresh multi-select with nothing toggled), then a single summary
    // confirm finalizes the deletion.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let feature_dir = fixture.cwd()?;
    feature_dir.assert(predicate::path::is_dir());

    // Delete the branch to make it a prune candidate (active by default: no flags needed)
    let repo = fixture.repo()?;
    repo.find_reference("refs/heads/feature")?.delete()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["prune"]);

    session.expect("Select worktrees to prune")?;
    // Rows render in the find/list style with a dim trailing prune annotation.
    session.expect("branch deleted")?;
    session.send("\r")?;
    session.expect("Prune 1 worktree")?;
    session.send("y\r")?;

    session.expect(expectrl::Eof)?;

    // Verify worktree directory is gone
    feature_dir.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_interactive_picker_confirm_no() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let feature_dir = fixture.cwd()?;
    feature_dir.assert(predicate::path::is_dir());

    let repo = fixture.repo()?;
    repo.find_reference("refs/heads/feature")?.delete()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["prune"]);

    session.expect("Select worktrees to prune")?;
    session.send("\r")?;
    session.expect("Prune 1 worktree")?;
    session.send("n\r")?;

    session.expect(expectrl::Eof)?;

    // Verify worktree directory still exists
    feature_dir.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_interactive_picker_space_deselects() -> Result<(), Box<dyn std::error::Error>> {
    // Space toggles the pre-checked row off; Enter with nothing selected exits
    // without a confirm prompt and without deleting anything.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let feature_dir = fixture.cwd()?;
    feature_dir.assert(predicate::path::is_dir());

    let repo = fixture.repo()?;
    repo.find_reference("refs/heads/feature")?.delete()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["prune"]);

    session.expect("Select worktrees to prune")?;
    session.send(" ")?;
    session.send("\r")?;
    session.expect("No worktrees selected")?;

    session.expect(expectrl::Eof)?;

    // Verify worktree directory still exists
    feature_dir.assert(predicate::path::is_dir());

    Ok(())
}

// --- Dry-run analysis tests (replaces the old gone-hint tests: annotations are now
// always visible via --dry-run instead of a hint appended to a bare, non-dry run) ---

#[test]
fn prune_dry_run_annotates_prechecked_signal() -> Result<(), Box<dyn std::error::Error>> {
    // A gone-upstream worktree is always shown by --dry-run, annotated with its
    // signal, whether or not --gone is active.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Without --gone: shown but only "selectable" (not an active criterion yet).
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("[selectable]"))
        .stderr(predicate::str::contains("remote gone"));

    // With --gone: the same row becomes pre-checked.
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("[pre-checked]"))
        .stderr(predicate::str::contains("remote gone"));

    // Nothing was touched by either dry run.
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_dry_run_bare_hides_signal_less_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    // Bare mode's dry-run table only ever includes rows carrying a signal — a freshly
    // created worktree with no unique history (trivially merged, but that's the same
    // as any other worktree) still carries a Merged signal, so exercise the "nothing
    // to show" branch with a worktree deep enough that even Merged can't apply: the
    // default worktree, which is excluded outright.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("No worktrees to prune"));

    Ok(())
}

#[test]
fn prune_dry_run_named_shows_healthy_worktree_as_not_prunable(
) -> Result<(), Box<dyn std::error::Error>> {
    // A named worktree with a divergent, unmerged commit carries no signal but is not
    // "healthy" either (it has real unmerged work) — the annotation should say so
    // rather than "not prunable".
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    fixture
        .commit("feature")
        .file("feature.txt", "feature")
        .create("Feature commit")?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("[selectable]"))
        .stderr(predicate::str::contains("unmerged"));

    Ok(())
}

#[test]
fn prune_dry_run_json_includes_signals() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    fixture
        .repo()?
        .find_reference("refs/heads/feature")?
        .delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    let output = prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--dry-run")
        .arg("--json")
        .output()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let json: serde_json::Value = serde_json::from_str(&stdout)?;
    assert_eq!(json["dry_run"], serde_json::json!(true));
    let pruned = json["pruned"].as_array().expect("pruned should be array");
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0]["branch_deleted"], serde_json::json!(false));
    let signals = pruned[0]["signals"]
        .as_array()
        .expect("signals should be array");
    assert!(signals.iter().any(|s| s.as_str() == Some("branch deleted")));

    // Nothing was actually pruned.
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_named_healthy_worktree_skipped_without_force() -> Result<(), Box<dyn std::error::Error>> {
    // A named worktree with no signal, no dirty changes, and nothing unmerged (i.e.
    // trivially caught up with the default branch) is "healthy" — naming it pulls it
    // into view, but it still needs --force to actually be pruned.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--merged=develop-does-not-exist")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped"))
        .stderr(predicate::str::contains("not prunable"))
        .stderr(predicate::str::contains("No worktrees to prune"));

    fixture.cwd()?.assert(predicate::path::is_dir());

    // --force prunes it anyway.
    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--merged=develop-does-not-exist")
        .arg("--force")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_named_fetch_narrows_to_named_remotes() -> Result<(), Box<dyn std::error::Error>> {
    // Naming a worktree narrows the prune-fetch to remotes tracked by that worktree
    // only — an unreachable remote tracked solely by an un-named worktree must not be
    // contacted (and therefore must not produce a fetch-failure warning).
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .remote("unreachable", "git://192.0.2.1/does-not-exist.git")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .worktree("other")
        .upstream("other", "unreachable/other")
        .build()?;

    fixture
        .repo()?
        .find_reference("refs/heads/feature")?
        .delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("feature")
        .arg("--fetch")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("could not fetch unreachable").not())
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());
    fixture
        .root()?
        .child("other")
        .assert(predicate::path::is_dir());

    Ok(())
}

// --- Config-driven gone/fetch tests ---

#[test]
fn prune_config_prune_gone_removes_gone_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    // workon.pruneGone=true makes bare prune behave like prune --gone.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .config("workon.pruneGone", "true")
        .build()?;

    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"))
        .stderr(predicate::str::contains("remote gone"));

    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_no_gone_flag_suppresses_config() -> Result<(), Box<dyn std::error::Error>> {
    // --no-gone overrides workon.pruneGone=true for one run.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        .upstream("feature", "origin/feature")
        .config("workon.pruneGone", "true")
        .build()?;

    fixture
        .repo()?
        .find_reference("refs/remotes/origin/feature")?
        .delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--no-gone")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("No worktrees to prune"));

    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_fetch_failure_warns_and_continues() -> Result<(), Box<dyn std::error::Error>> {
    // When --fetch is passed but the remote is not a valid git repo, prune should
    // warn and continue (still pruning BranchDeleted candidates on cached refs).
    // Use /dev/null as the remote URL: not a git repository, so fetch fails fast.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .worktree("feature")
        // Track origin/feature so remotes_tracked_by_worktrees includes "origin"
        .upstream("feature", "origin/feature")
        .build()?;

    // Delete the branch to create a BranchDeleted prune candidate
    fixture
        .repo()?
        .find_reference("refs/heads/feature")?
        .delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--fetch")
        .arg("--yes")
        .assert()
        .success()
        // fetch failed but prune continued and removed the BranchDeleted candidate
        .stderr(predicate::str::contains("could not fetch origin"))
        .stderr(predicate::str::contains("stale refs"))
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_fetch_flag_with_no_remotes_is_harmless() -> Result<(), Box<dyn std::error::Error>> {
    // --fetch when no worktree branches track any remote should be a no-op.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    fixture
        .repo()?
        .find_reference("refs/heads/feature")?
        .delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--fetch")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_no_fetch_flag_suppresses_config() -> Result<(), Box<dyn std::error::Error>> {
    // --no-fetch overrides workon.pruneFetch=true for one run, preventing the network call.
    // We use an unreachable remote to verify: if --no-fetch didn't work, fetch would fail
    // noisily; since the worktree also has a deleted branch it should still be pruned.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "git://192.0.2.1/does-not-exist.git")
        .worktree("feature")
        .config("workon.pruneFetch", "true")
        .build()?;

    fixture
        .repo()?
        .find_reference("refs/heads/feature")?
        .delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--no-fetch")
        .arg("--yes")
        .assert()
        .success()
        // No fetch warning because --no-fetch suppressed the fetch entirely
        .stderr(predicate::str::contains("could not fetch").not())
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_removes_empty_namespace_dir() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let repo = fixture.repo()?;

    use workon::{add_worktree, BranchType};
    add_worktree(repo, "claude/foo", None, BranchType::Normal, None, false)?;
    repo.find_reference("refs/heads/claude/foo")?.delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // The emptied namespace dir is cleaned up along with the worktree
    fixture
        .root()?
        .child("claude")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_keeps_namespace_dir_with_remaining_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let repo = fixture.repo()?;

    use workon::{add_worktree, BranchType};
    add_worktree(repo, "release/1.0", None, BranchType::Normal, None, false)?;
    add_worktree(repo, "release/2.0", None, BranchType::Normal, None, false)?;
    repo.find_reference("refs/heads/release/1.0")?.delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture
        .root()?
        .child("release/1.0")
        .assert(predicate::path::missing());
    // The namespace dir survives because a sibling worktree still lives in it
    fixture
        .root()?
        .child("release/2.0")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_removes_deeply_nested_namespace_dirs() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let repo = fixture.repo()?;

    use workon::{add_worktree, BranchType};
    add_worktree(repo, "a/b/c", None, BranchType::Normal, None, false)?;
    repo.find_reference("refs/heads/a/b/c")?.delete()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    // The whole emptied namespace chain is cleaned up
    fixture
        .root()?
        .child("a")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_keeps_namespace_dir_with_stray_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let repo = fixture.repo()?;

    use workon::{add_worktree, BranchType};
    add_worktree(repo, "claude/foo", None, BranchType::Normal, None, false)?;
    repo.find_reference("refs/heads/claude/foo")?.delete()?;

    fixture
        .root()?
        .child("claude/notes.txt")
        .write_str("keep")?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    fixture
        .root()?
        .child("claude/foo")
        .assert(predicate::path::missing());
    // The namespace dir is not empty (stray file), so it is preserved
    fixture
        .root()?
        .child("claude/notes.txt")
        .assert(predicate::path::is_file());

    Ok(())
}

// ── PR-merged signal ──────────────────────────────────────────────────────────

/// The current tip of `branch` in `fixture`'s repo, as a hex string, for stubbing a
/// `gh` `headRefOid` that actually matches (or deliberately doesn't match) the
/// worktree's real tip.
fn branch_tip(fixture: &Fixture, branch: &str) -> Result<String, Box<dyn std::error::Error>> {
    let repo = fixture.repo()?;
    let oid = repo
        .find_branch(branch, git2::BranchType::Local)?
        .get()
        .target()
        .ok_or("branch has no target")?;
    Ok(oid.to_string())
}

/// A `gh` stub that answers `--version` with success and `pr list` with a merged PR
/// for any branch, recording every invocation the fixture builder logs.
fn gh_stub_reports_merged_pr(pr_number: u32, head_oid: &str) -> String {
    format!(
        "if [ \"$1\" = \"--version\" ]; then\n  exit 0\nfi\necho '[{{\"number\":{pr_number},\"mergedAt\":\"2024-01-01T00:00:00Z\",\"headRefOid\":\"{head_oid}\"}}]'\n"
    )
}

/// A `gh` stub whose `--version` succeeds (so the availability gate passes) but
/// whose `pr list` always fails, for exercising per-row lookup degradation.
fn gh_stub_pr_list_fails() -> String {
    "if [ \"$1\" = \"--version\" ]; then\n  exit 0\nfi\nexit 1\n".to_string()
}

/// What a stubbed `gh pr list` should do for a given branch, keyed by `--head`'s
/// value (`gh pr list --head <branch> ...`, so the branch lands in `$4`).
enum GhOutcome {
    /// Exit non-zero, as if the lookup itself failed (rate limit, DNS blip, ...).
    Fail,
    /// Print an empty array, as if the branch never had a PR.
    Empty,
    /// Print a single merged PR with the given number and `headRefOid`.
    Merged(u32, String),
}

/// A `gh` stub whose `--version` succeeds and whose `pr list --head <branch>`
/// response varies per branch, per `mapping`. Lets a single test exercise multiple
/// rows hitting different outcomes in one pass.
fn gh_stub_pr_outcomes(mapping: &[(&str, GhOutcome)]) -> String {
    let mut script = String::from(
        "if [ \"$1\" = \"--version\" ]; then\n  exit 0\nfi\nbranch=\"$4\"\ncase \"$branch\" in\n",
    );
    for (branch, outcome) in mapping {
        let body = match outcome {
            GhOutcome::Fail => "    exit 1\n".to_string(),
            GhOutcome::Empty => "    echo '[]'\n".to_string(),
            GhOutcome::Merged(number, head_oid) => format!(
                "    echo '[{{\"number\":{number},\"mergedAt\":\"2024-01-01T00:00:00Z\",\"headRefOid\":\"{head_oid}\"}}]'\n"
            ),
        };
        script.push_str(&format!("  {branch})\n{body}    ;;\n"));
    }
    script.push_str("esac\n");
    script
}

/// A worktree whose branch is diverged from `main` (so `Merged` doesn't fire) with a
/// local branch and no upstream (so neither `BranchDeleted` nor `RemoteGone` fires),
/// leaving the PR-merged lookup as the only possible signal. Carries a GitHub remote
/// so the PR pass actually runs (it's skipped entirely without one).
fn build_signal_less_diverged_worktree() -> Result<Fixture, Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "https://github.com/test/test.git")
        .worktree("feature")
        .build()?;

    fixture
        .commit("feature")
        .file("feature.txt", "feature")
        .create("Feature commit")?;

    Ok(fixture)
}

#[test]
fn prune_dry_run_reports_merged_pr_signal() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = build_signal_less_diverged_worktree()?;
    let tip = branch_tip(&fixture, "feature")?;
    let stub = PathStub::new()?.binary("gh", &gh_stub_reports_merged_pr(66, &tip))?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("PR #66 merged"));

    Ok(())
}

#[test]
fn prune_yes_prunes_merged_pr_worktree_and_deletes_branch() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = build_signal_less_diverged_worktree()?;
    let tip = branch_tip(&fixture, "feature")?;
    let stub = PathStub::new()?.binary("gh", &gh_stub_reports_merged_pr(66, &tip))?;

    let feature_dir = fixture.cwd()?;
    feature_dir.assert(predicate::path::is_dir());

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned 1 worktree"));

    feature_dir.assert(predicate::path::missing());
    fixture.assert(predicate::repo::has_branch("feature").not());

    Ok(())
}

#[test]
fn prune_degrades_quietly_when_gh_missing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = build_signal_less_diverged_worktree()?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    let output = prune_cmd
        .current_dir(&fixture)
        .env("PATH", PathStub::path_without("gh"))
        .arg("prune")
        .arg("--dry-run")
        .output()?;

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("No worktrees to prune"), "stderr: {stderr}");
    assert!(!stderr.to_lowercase().contains("warn"), "stderr: {stderr}");

    Ok(())
}

#[test]
fn prune_degrades_quietly_when_gh_pr_list_fails() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = build_signal_less_diverged_worktree()?;
    let stub = PathStub::new()?.binary("gh", &gh_stub_pr_list_fails())?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    let output = prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .arg("prune")
        .arg("--dry-run")
        .output()?;

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("No worktrees to prune"), "stderr: {stderr}");
    assert!(!stderr.to_lowercase().contains("warn"), "stderr: {stderr}");

    Ok(())
}

/// A `gh` that fails on every call still costs at most 3 subprocesses (the
/// consecutive-failure bound), rather than one per worktree.
#[test]
fn prune_stops_pr_lookups_after_3_consecutive_gh_failures() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "https://github.com/test/test.git")
        .worktree("feature-one")
        .worktree("feature-two")
        .worktree("feature-three")
        .worktree("feature-four")
        .worktree("feature-five")
        .build()?;

    for branch in [
        "feature-one",
        "feature-two",
        "feature-three",
        "feature-four",
        "feature-five",
    ] {
        fixture
            .commit(branch)
            .file(&format!("{branch}.txt"), branch)
            .create("Feature commit")?;
    }

    let stub = PathStub::new()?.binary("gh", &gh_stub_pr_list_fails())?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("No worktrees to prune"));

    // Five worktrees against a stub that always fails, so the bound has to stop the
    // pass two rows early. Three worktrees wouldn't prove anything here: the count
    // would be 3 whether the bound existed or not.
    let pr_list_calls = stub
        .invocations("gh")
        .into_iter()
        .filter(|line| line.starts_with("pr list"))
        .count();
    assert_eq!(
        pr_list_calls, 3,
        "expected exactly 3 pr list calls at the consecutive-failure bound, got {pr_list_calls}"
    );

    Ok(())
}

/// A single `gh` failure degrades that one row but doesn't stop the pass: it's below
/// the consecutive-failure bound, so later rows still get looked up.
#[test]
fn prune_does_not_stop_pass_after_a_single_gh_failure() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "https://github.com/test/test.git")
        .worktree("feature-one")
        .worktree("feature-two")
        .worktree("feature-three")
        .build()?;

    for branch in ["feature-one", "feature-two", "feature-three"] {
        fixture
            .commit(branch)
            .file(&format!("{branch}.txt"), branch)
            .create("Feature commit")?;
    }

    let feature_three_tip = branch_tip(&fixture, "feature-three")?;
    let stub = PathStub::new()?.binary(
        "gh",
        &gh_stub_pr_outcomes(&[
            ("feature-one", GhOutcome::Fail),
            ("feature-two", GhOutcome::Empty),
            ("feature-three", GhOutcome::Merged(66, feature_three_tip)),
        ]),
    )?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("PR #66 merged"));

    let pr_list_calls = stub
        .invocations("gh")
        .into_iter()
        .filter(|line| line.starts_with("pr list"))
        .count();
    assert_eq!(
        pr_list_calls, 3,
        "a single failure should not stop the pass, got {pr_list_calls} pr list calls"
    );

    Ok(())
}

/// An empty result for one branch (the common real case — a branch that never had a
/// PR) doesn't stop the pass, and `find_merged_pr`'s array extraction handles it.
#[test]
fn prune_gh_empty_list_for_one_branch_does_not_stop_pass() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "https://github.com/test/test.git")
        .worktree("no-pr")
        .worktree("merged")
        .build()?;

    for branch in ["no-pr", "merged"] {
        fixture
            .commit(branch)
            .file(&format!("{branch}.txt"), branch)
            .create("Feature commit")?;
    }

    let merged_tip = branch_tip(&fixture, "merged")?;
    let stub = PathStub::new()?.binary(
        "gh",
        &gh_stub_pr_outcomes(&[
            ("no-pr", GhOutcome::Empty),
            ("merged", GhOutcome::Merged(66, merged_tip)),
        ]),
    )?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("PR #66 merged"))
        .stderr(predicate::str::contains("no-pr").not());

    let pr_list_calls = stub
        .invocations("gh")
        .into_iter()
        .filter(|line| line.starts_with("pr list"))
        .count();
    assert_eq!(
        pr_list_calls, 2,
        "expected both branches to be looked up, got {pr_list_calls} pr list calls"
    );

    Ok(())
}

#[test]
fn prune_skips_pr_lookup_for_branch_already_deleted() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "https://github.com/test/test.git")
        .worktree("feature")
        .build()?;

    fixture
        .repo()?
        .find_reference("refs/heads/feature")?
        .delete()?;

    // The branch is already deleted, so `fill_pr_merged` never calls `gh` for this
    // row; the head oid here is never read.
    let stub = PathStub::new()?.binary(
        "gh",
        &gh_stub_reports_merged_pr(66, "0000000000000000000000000000000000000000"),
    )?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("branch deleted"));

    let invocations = stub.invocations("gh");
    assert!(
        invocations.iter().all(|line| !line.starts_with("pr")),
        "gh pr list should not have been invoked for a signal-less-free row: {invocations:?}"
    );

    Ok(())
}

#[test]
fn prune_json_includes_pr_merged_signal() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = build_signal_less_diverged_worktree()?;
    let tip = branch_tip(&fixture, "feature")?;
    let stub = PathStub::new()?.binary("gh", &gh_stub_reports_merged_pr(66, &tip))?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    let output = prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .arg("prune")
        .arg("--dry-run")
        .arg("--json")
        .output()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let json: serde_json::Value = serde_json::from_str(&stdout)?;
    let pruned = json["pruned"].as_array().expect("pruned should be array");
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0]["reason"], serde_json::json!("PR #66 merged"));
    let signals = pruned[0]["signals"]
        .as_array()
        .expect("signals should be array");
    assert!(signals.iter().any(|s| s.as_str() == Some("PR #66 merged")));

    Ok(())
}

/// A gitflow-style branch that was once a PR's head but has since moved on (its tip
/// now has commits past the merged PR's `headRefOid`) gets no `PrMerged` signal: the
/// SHA comparison catches the stale match, and the ordinary `unmerged` check applies
/// again instead.
#[test]
fn prune_dry_run_ignores_merged_pr_when_branch_moved_on() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = build_signal_less_diverged_worktree()?;
    // Capture the tip as it stands (as if this were the commit the PR merged at),
    // then move `feature` past it, the way a gitflow release/hotfix branch keeps
    // advancing after each PR into it merges.
    let merged_head_oid = branch_tip(&fixture, "feature")?;
    fixture
        .commit("feature")
        .file("more.txt", "more")
        .create("Commit after the PR merged")?;

    let stub = PathStub::new()?.binary("gh", &gh_stub_reports_merged_pr(66, &merged_head_oid))?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("PR #66 merged").not())
        // Bare dry-run only shows rows carrying a signal; a signal-less row (the
        // stale match produced no signal) doesn't appear at all.
        .stderr(predicate::str::contains("feature").not());

    Ok(())
}

/// The same stale-head scenario as above, but naming the worktree so it appears
/// despite carrying no signal — proving `unmerged` was never cleared, since the row
/// still gets blocked as unmerged rather than treated as safe to prune.
#[test]
fn prune_named_merged_pr_stale_head_is_blocked_as_unmerged(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = build_signal_less_diverged_worktree()?;
    let merged_head_oid = branch_tip(&fixture, "feature")?;
    fixture
        .commit("feature")
        .file("more.txt", "more")
        .create("Commit after the PR merged")?;

    let stub = PathStub::new()?.binary("gh", &gh_stub_reports_merged_pr(66, &merged_head_oid))?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("feature")
        .arg("--yes")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "has unmerged commits, use --allow-unmerged to override",
        ));

    fixture.assert(predicate::repo::has_branch("feature"));

    Ok(())
}

/// A merged PR whose `headRefOid` is a local descendant of the branch's current tip —
/// the PR was pushed from elsewhere and carries everything local plus more — still
/// fires `PrMerged`, since the branch tip is fully covered by the merged head.
#[test]
fn prune_dry_run_reports_merged_pr_when_tip_is_ancestor_of_head(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = build_signal_less_diverged_worktree()?;
    let repo = fixture.repo()?;
    let tip_oid = git2::Oid::from_str(&branch_tip(&fixture, "feature")?)?;
    let tip_commit = repo.find_commit(tip_oid)?;

    // Build a descendant commit object without moving `feature` itself, and park it
    // under a remote-tracking ref — the same place a real merged head can live when
    // it was pushed from elsewhere and never fetched as a local branch.
    let sig = git2::Signature::now("test", "test@test.com")?;
    let descendant_oid = repo.commit(
        None,
        &sig,
        &sig,
        "Descendant of the local tip",
        &tip_commit.tree()?,
        &[&tip_commit],
    )?;
    fixture.create_remote_ref("origin/feature", descendant_oid)?;

    let stub = PathStub::new()?.binary(
        "gh",
        &gh_stub_reports_merged_pr(66, &descendant_oid.to_string()),
    )?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("PR #66 merged"));

    Ok(())
}

/// A merged PR payload missing `headRefOid` entirely produces no signal — there's
/// nothing to compare the branch tip against, and the standing contract is to
/// under-report rather than guess.
#[test]
fn prune_dry_run_ignores_merged_pr_without_head_oid() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = build_signal_less_diverged_worktree()?;
    let stub = PathStub::new()?.binary(
        "gh",
        "if [ \"$1\" = \"--version\" ]; then\n  exit 0\nfi\necho '[{\"number\":66,\"mergedAt\":\"2024-01-01T00:00:00Z\"}]'\n",
    )?;

    let mut prune_cmd = cargo_bin_cmd!("git-workon");
    prune_cmd
        .current_dir(&fixture)
        .env("PATH", stub.path())
        .env("NO_COLOR", "1")
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("PR #66 merged").not())
        .stderr(predicate::str::contains("feature").not());

    Ok(())
}
