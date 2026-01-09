use assert_cmd::Command;
use git2::BranchType;
use git_workon_fixture::prelude::*;

#[test]
fn move_basic_rename() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let repo = fixture.repo()?;

    // Verify initial state
    repo.assert(predicate::repo::has_branch("feature"));
    repo.assert(predicate::repo::has_worktree("feature"));
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    // Execute move
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("feature")
        .arg("bugfix")
        .assert()
        .success();

    // Verify final state
    repo.assert(predicate::repo::has_branch("bugfix"));
    repo.assert(predicate::repo::has_worktree("bugfix"));
    fixture
        .root()?
        .child("bugfix")
        .assert(predicate::path::is_dir());

    // Verify old is gone
    let has_old_branch = repo.find_branch("feature", BranchType::Local).is_ok();
    assert!(!has_old_branch);
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn move_namespace_change() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let repo = fixture.repo()?;

    // Move into namespace
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("feature")
        .arg("user/feature")
        .assert()
        .success();

    // Verify namespace directory created and worktree moved
    repo.assert(predicate::repo::has_branch("user/feature"));
    fixture
        .root()?
        .child("user")
        .child("feature")
        .assert(predicate::path::is_dir());
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn move_fails_if_target_exists() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .worktree("bugfix")
        .build()?;

    // Try to move feature to bugfix (which already exists)
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("feature")
        .arg("bugfix")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    Ok(())
}

#[test]
fn move_fails_if_source_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("nonexistent")
        .arg("new-name")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not find worktree"));

    Ok(())
}

#[test]
fn move_fails_on_detached_head() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    // Get the current HEAD commit and detach to it
    let main_path = fixture.cwd()?;
    let worktree_repo = git2::Repository::open(&main_path)?;
    let head_commit = worktree_repo.head()?.peel_to_commit()?;
    worktree_repo.set_head_detached(head_commit.id())?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("main")
        .arg("new-name")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Cannot move detached HEAD"));

    Ok(())
}

#[test]
fn move_fails_on_dirty_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Create uncommitted changes
    std::fs::write(fixture.cwd()?.join("uncommitted.txt"), "test")?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("feature")
        .arg("bugfix")
        .assert()
        .failure()
        .stderr(predicate::str::contains("dirty"));

    Ok(())
}

#[test]
fn move_fails_on_unpushed_commits() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .remote("origin", "https://github.com/example/repo.git")
        .upstream("feature", "origin/feature")
        .build()?;

    // Create a commit (will be unpushed since remote doesn't exist)
    let feature_path = fixture.cwd()?;
    std::fs::write(feature_path.join("test.txt"), "test")?;

    let worktree_repo = git2::Repository::open(&feature_path)?;
    let mut index = worktree_repo.index()?;
    index.add_path(std::path::Path::new("test.txt"))?;
    index.write()?;

    let tree_id = index.write_tree()?;
    let tree = worktree_repo.find_tree(tree_id)?;
    let parent = worktree_repo.head()?.peel_to_commit()?;
    let sig = git2::Signature::now("Test", "test@example.com")?;

    worktree_repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "Unpushed commit",
        &tree,
        &[&parent],
    )?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("feature")
        .arg("bugfix")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unpushed"));

    Ok(())
}

#[test]
fn move_fails_on_protected_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .config("workon.pruneProtectedBranches", "develop")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("develop")
        .arg("development")
        .assert()
        .failure()
        .stderr(predicate::str::contains("protected"));

    Ok(())
}

#[test]
fn move_force_overrides_all_checks() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .config("workon.pruneProtectedBranches", "develop")
        .build()?;

    // Create uncommitted changes (dirty)
    std::fs::write(fixture.cwd()?.join("uncommitted.txt"), "test")?;

    // With --force, should succeed despite being protected and dirty
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("--force")
        .arg("develop")
        .arg("development")
        .assert()
        .success();

    let repo = fixture.repo()?;
    repo.assert(predicate::repo::has_branch("development"));

    Ok(())
}

#[test]
fn move_preserves_upstream_config() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .remote("origin", "https://github.com/example/repo.git")
        .upstream("feature", "origin/feature")
        .build()?;

    let repo = fixture.repo()?;

    // Verify upstream is set before move
    repo.assert(predicate::repo::has_upstream("feature", None));

    // Move the worktree
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("feature")
        .arg("bugfix")
        .assert()
        .success();

    // Verify upstream is still set after move (with new branch name)
    repo.assert(predicate::repo::has_upstream("bugfix", None));

    Ok(())
}

#[test]
fn move_dry_run_preview_only() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let repo = fixture.repo()?;

    // Execute with --dry-run
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("--dry-run")
        .arg("feature")
        .arg("bugfix")
        .assert()
        .success()
        .stdout(predicate::str::contains("Would move"));

    // Verify nothing changed
    repo.assert(predicate::repo::has_branch("feature"));
    repo.assert(predicate::repo::has_worktree("feature"));
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    let has_new_branch = repo.find_branch("bugfix", BranchType::Local).is_ok();
    assert!(!has_new_branch);
    fixture
        .root()?
        .child("bugfix")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn move_identical_names_fails() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("move")
        .arg("feature")
        .arg("feature")
        .assert()
        .failure()
        .stderr(predicate::str::contains("identical"));

    Ok(())
}

#[test]
fn move_current_worktree_single_arg() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let repo = fixture.repo()?;

    // The fixture is opened in the feature worktree (last worktree specified)
    // Execute move with single arg (rename current worktree)
    Command::cargo_bin("git-workon")?
        .current_dir(fixture.cwd()?)
        .arg("move")
        .arg("bugfix")
        .assert()
        .success();

    // Verify final state - branch renamed and directory moved
    repo.assert(predicate::repo::has_branch("bugfix"));
    fixture
        .root()?
        .child("bugfix")
        .assert(predicate::path::is_dir());

    // Verify old is gone
    let has_old_branch = repo.find_branch("feature", BranchType::Local).is_ok();
    assert!(!has_old_branch);
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::missing());

    Ok(())
}

#[test]
fn move_current_worktree_fails_outside_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    // Try to move with single arg from the bare repo (not in a worktree)
    let bare_dir = fixture.root()?.join(".bare");
    Command::cargo_bin("git-workon")?
        .current_dir(&bare_dir)
        .arg("move")
        .arg("bugfix")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Not in a worktree directory"));

    Ok(())
}

#[test]
fn move_current_worktree_dry_run() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let repo = fixture.repo()?;

    // Execute with --dry-run and single arg (from within the worktree)
    Command::cargo_bin("git-workon")?
        .current_dir(fixture.cwd()?)
        .arg("move")
        .arg("--dry-run")
        .arg("bugfix")
        .assert()
        .success()
        .stdout(predicate::str::contains("Would move"));

    // Verify nothing changed - branch and directory still there
    repo.assert(predicate::repo::has_branch("feature"));
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    let has_new_branch = repo.find_branch("bugfix", BranchType::Local).is_ok();
    assert!(!has_new_branch);
    fixture
        .root()?
        .child("bugfix")
        .assert(predicate::path::missing());

    Ok(())
}
