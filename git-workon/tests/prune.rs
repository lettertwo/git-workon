use assert_cmd::Command;
use assert_fs::TempDir;
use git2::Repository;
use git_workon_fixture::prelude::*;

#[test]
fn prune_with_no_stale_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // Initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Run prune - should report nothing to prune
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&temp)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees to prune"));

    temp.close()?;
    Ok(())
}

#[test]
fn prune_removes_worktree_for_deleted_branch() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // Initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Create a new worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&temp)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify worktree exists
    temp.child("feature").assert(predicate::path::is_dir());

    // Delete the branch manually using git2 (force delete since it's checked out in a worktree)
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.find_reference(&format!("refs/heads/feature"))?.delete()?;

    // Run prune - should remove the worktree
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&temp)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 1 worktree"));

    // Verify worktree directory is gone
    temp.child("feature").assert(predicate::path::missing());

    temp.close()?;
    Ok(())
}

#[test]
fn prune_dry_run_does_not_remove_anything() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // Initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Create a new worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&temp)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Delete the branch manually (force delete since it's checked out in a worktree)
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.find_reference(&format!("refs/heads/feature"))?.delete()?;

    // Run prune with --dry-run
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&temp)
        .arg("prune")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run - no changes made"));

    // Verify worktree still exists
    temp.child("feature").assert(predicate::path::is_dir());

    temp.close()?;
    Ok(())
}

#[test]
fn prune_handles_multiple_stale_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // Initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Create multiple worktrees
    for name in &["feature-1", "feature-2", "feature-3"] {
        let mut new_cmd = Command::cargo_bin("git-workon")?;
        new_cmd
            .current_dir(&temp)
            .arg("new")
            .arg(name)
            .assert()
            .success();
    }

    // Delete all the feature branches (force delete)
    let repo = Repository::open(temp.path().join(".bare"))?;
    for name in &["feature-1", "feature-2", "feature-3"] {
        repo.find_reference(&format!("refs/heads/{}", name))?.delete()?;
    }

    // Run prune
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&temp)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 3 worktree"));

    // Verify all worktrees are gone
    for name in &["feature-1", "feature-2", "feature-3"] {
        temp.child(name).assert(predicate::path::missing());
    }

    temp.close()?;
    Ok(())
}

#[test]
fn prune_preserves_worktrees_with_existing_branches() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // Initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Create two worktrees
    let mut new_cmd1 = Command::cargo_bin("git-workon")?;
    new_cmd1
        .current_dir(&temp)
        .arg("new")
        .arg("keep-me")
        .assert()
        .success();

    let mut new_cmd2 = Command::cargo_bin("git-workon")?;
    new_cmd2
        .current_dir(&temp)
        .arg("new")
        .arg("delete-me")
        .assert()
        .success();

    // Delete only one branch (force delete)
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.find_reference(&format!("refs/heads/delete-me"))?.delete()?;

    // Run prune
    let mut prune_cmd = Command::cargo_bin("git-workon")?;
    prune_cmd
        .current_dir(&temp)
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pruned 1 worktree"));

    // Verify keep-me still exists but delete-me is gone
    temp.child("keep-me").assert(predicate::path::is_dir());
    temp.child("delete-me").assert(predicate::path::missing());

    temp.close()?;
    Ok(())
}
