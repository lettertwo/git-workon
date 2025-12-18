use assert_cmd::Command;
use assert_fs::TempDir;
use git2::Repository;
use git_workon_fixture::prelude::*;

#[test]
fn new_creates_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // First initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Create a new worktree for a new branch
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&temp)
        .arg("new")
        .arg("feature-branch")
        .assert()
        .success();

    // Verify the new worktree directory exists
    temp.child("feature-branch").assert(predicate::path::is_dir());

    // Open the repository and verify git state
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("main"));
    repo.assert(predicate::repo::has_branch("feature-branch"));

    temp.close()?;
    Ok(())
}

#[test]
fn new_with_slashes_in_name() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // First initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Create a new worktree with slashes in the branch name
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&temp)
        .arg("new")
        .arg("user/feature-branch")
        .assert()
        .success();

    // Verify the new worktree directory exists with the full path
    temp.child("user/feature-branch")
        .assert(predicate::path::is_dir());

    // Open the repository and verify git state
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("user/feature-branch"));

    temp.close()?;
    Ok(())
}

#[test]
fn new_orphan_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // First initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Create an orphan worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&temp)
        .arg("new")
        .arg("--orphan")
        .arg("docs")
        .assert()
        .success();

    // Verify the new worktree directory exists
    temp.child("docs").assert(predicate::path::is_dir());

    // Open the repository and verify the main branch exists
    // Note: The orphan branch won't exist in the repo until a commit is made
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("main"));

    temp.close()?;
    Ok(())
}

#[test]
fn new_detached_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;

    // First initialize a repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd.current_dir(&temp).arg("init").assert().success();

    // Create a detached worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&temp)
        .arg("new")
        .arg("--detach")
        .arg("detached")
        .assert()
        .success();

    // Verify the new worktree directory exists
    temp.child("detached").assert(predicate::path::is_dir());

    // Open the repository and verify the main branch exists
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("main"));

    temp.close()?;
    Ok(())
}
