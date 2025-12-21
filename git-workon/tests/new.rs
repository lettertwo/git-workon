use assert_cmd::Command;
use git_workon_fixture::prelude::*;

#[test]
fn new_creates_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a new worktree for a new branch
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature-branch")
        .assert()
        .success();

    // Verify the new worktree directory exists
    fixture
        .root()?
        .child("feature-branch")
        .assert(predicate::path::is_dir());

    fixture.assert(predicate::repo::is_bare());
    fixture.assert(predicate::repo::has_branch("main"));
    fixture.assert(predicate::repo::has_branch("feature-branch"));

    Ok(())
}

#[test]
fn new_with_slashes_in_name() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a new worktree with slashes in the branch name
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("user/feature-branch")
        .assert()
        .success();

    // Verify the new worktree directory exists with the full path
    fixture
        .root()?
        .child("user/feature-branch")
        .assert(predicate::path::is_dir());

    // Open the repository and verify git state
    fixture.assert(predicate::repo::is_bare());
    fixture.assert(predicate::repo::has_branch("user/feature-branch"));

    Ok(())
}

#[test]
#[ignore]
fn new_orphan_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Add a commit to main so we can verify the orphan has no parent
    fixture
        .commit("main")
        .file("test.txt", "test")
        .create("Test commit")?;

    // Create an orphan worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--orphan")
        .arg("docs")
        .assert()
        .success();

    // Verify the new worktree directory exists
    fixture
        .root()?
        .child("docs")
        .assert(predicate::path::is_dir());

    // Open the repository and verify the main branch exists
    fixture.assert(predicate::repo::is_bare());
    fixture.assert(predicate::repo::has_branch("main"));

    // Open the orphan worktree and verify it's truly orphaned
    // let orphan_repo = Repository::open(fixture.root()?.join("docs"))?;

    // Verify HEAD points to the docs branch
    let head = fixture.head()?;
    assert_eq!(head.name(), Some("refs/heads/docs"));

    // Verify the branch has exactly one commit (the initial empty commit)
    let head_commit = head.peel_to_commit()?;
    assert_eq!(
        head_commit.parent_count(),
        0,
        "Orphan branch should have no parent commits"
    );

    // Verify the index is empty
    let index = fixture.repo()?.index()?;
    assert_eq!(index.len(), 0, "Orphan worktree index should be empty");

    // Verify the working directory doesn't contain files from main
    assert!(
        !fixture.root()?.path().join("docs/test.txt").exists(),
        "Orphan worktree should not have files from parent branch"
    );

    Ok(())
}

#[test]
#[ignore]
fn new_detached_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a detached worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--detach")
        .arg("detached")
        .assert()
        .success();

    // Verify the new worktree directory exists
    fixture
        .root()?
        .child("detached")
        .assert(predicate::path::is_dir());

    // Open the repository and verify the main branch exists
    fixture.assert(predicate::repo::is_bare());
    fixture.assert(predicate::repo::has_branch("main"));

    // TODO: Verify the detached worktree HEAD is in detached state

    Ok(())
}
