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

    // Add a commit to main so we can verify the orphan has no parent
    std::fs::write(temp.path().join("main/test.txt"), "test")?;
    std::process::Command::new("git")
        .current_dir(temp.path().join("main"))
        .args(["add", "test.txt"])
        .output()?;
    std::process::Command::new("git")
        .current_dir(temp.path().join("main"))
        .args(["commit", "-m", "Test commit"])
        .output()?;

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
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("main"));

    // Open the orphan worktree and verify it's truly orphaned
    let orphan_repo = Repository::open(temp.path().join("docs"))?;

    // Verify HEAD points to the docs branch
    let head = orphan_repo.head()?;
    assert_eq!(head.name(), Some("refs/heads/docs"));

    // Verify the branch has exactly one commit (the initial empty commit)
    let head_commit = head.peel_to_commit()?;
    assert_eq!(head_commit.parent_count(), 0, "Orphan branch should have no parent commits");

    // Verify the index is empty
    let index = orphan_repo.index()?;
    assert_eq!(index.len(), 0, "Orphan worktree index should be empty");

    // Verify the working directory doesn't contain files from main
    assert!(!temp.path().join("docs/test.txt").exists(),
            "Orphan worktree should not have files from parent branch");

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
