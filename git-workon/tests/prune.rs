use assert_cmd::Command;
use git_workon_fixture::prelude::*;

#[test]
fn prune_with_no_stale_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Run prune - should report nothing to prune
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees to prune"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 1 worktree"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run - no changes made"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 3 worktree"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 1 worktree"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees to prune"));

    // Verify worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    // Run prune with --gone - should remove the worktree
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--allow-unpushed")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 1 worktree"))
        .stdout(predicate::str::contains("remote gone"));

    // Verify worktree directory is gone
    fixture.cwd()?.assert(predicate::path::missing());

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees to prune"));

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

    // Run prune with --gone and --dry-run
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--allow-unpushed")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run - no changes made"))
        .stdout(predicate::str::contains("remote gone"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Skipped worktrees"))
        .stdout(predicate::str::contains("uncommitted changes"))
        .stdout(predicate::str::contains("No worktrees to prune"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--allow-dirty")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 1 worktree"));

    // Verify worktree is gone
    fixture.cwd()?.assert(predicate::path::missing());

    Ok(())
}

#[test]
fn prune_gone_skips_worktrees_with_unpushed_commits() -> Result<(), Box<dyn std::error::Error>> {
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

    // Create a new commit in the worktree (unpushed)
    fixture
        .commit("feature")
        .file("test.txt", "test")
        .create("New commit")?;

    // Run prune --gone (without --allow-unpushed)
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Skipped worktrees"))
        .stdout(predicate::str::contains("unpushed commits"))
        .stdout(predicate::str::contains("No worktrees to prune"));

    // Verify worktree still exists
    fixture.cwd()?.assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn prune_gone_with_allow_unpushed_removes_worktrees_with_unpushed_commits(
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

    // Create a new commit in the worktree (unpushed)
    fixture
        .commit("feature")
        .file("test.txt", "test")
        .create("New commit")?;

    // Run prune --gone with --allow-unpushed
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--gone")
        .arg("--allow-unpushed")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 1 worktree"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--merged")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 1 worktree"))
        .stdout(predicate::str::contains("merged into main"));

    // Verify worktree is gone
    fixture.cwd()?.assert(predicate::path::missing());

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--merged")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees to prune"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--merged=develop")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("default worktree"))
        .stdout(predicate::str::contains("Pruned 1 worktree"))
        .stdout(predicate::str::contains("merged into develop"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Skipped"))
        .stdout(predicate::str::contains(
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
    add_worktree(repo, "release/1.0", BranchType::Normal, None)?;
    add_worktree(repo, "release/2.0", BranchType::Normal, None)?;
    add_worktree(repo, "feature/test", BranchType::Normal, None)?;

    // Delete all branches to make them prune candidates
    repo.find_reference("refs/heads/release/1.0")?.delete()?;
    repo.find_reference("refs/heads/release/2.0")?.delete()?;
    repo.find_reference("refs/heads/feature/test")?.delete()?;

    // Run prune - should skip release/* but prune feature/test
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Skipped"))
        .stdout(predicate::str::contains("release/1.0"))
        .stdout(predicate::str::contains("release/2.0"))
        .stdout(predicate::str::contains("Pruned 1 worktree"));

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
    add_worktree(repo, "release/1.0", BranchType::Normal, None)?;
    add_worktree(repo, "feature/test", BranchType::Normal, None)?;

    // Delete all branches to make them prune candidates
    repo.find_reference("refs/heads/develop")?.delete()?;
    repo.find_reference("refs/heads/staging")?.delete()?;
    repo.find_reference("refs/heads/release/1.0")?.delete()?;
    repo.find_reference("refs/heads/feature/test")?.delete()?;

    // Run prune - should skip all protected branches but prune feature/test
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Skipped"))
        .stdout(predicate::str::contains("develop"))
        .stdout(predicate::str::contains("staging"))
        .stdout(predicate::str::contains("release/1.0"))
        .stdout(predicate::str::contains("Pruned 1 worktree"));

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
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&fixture)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 2 worktree"));

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
