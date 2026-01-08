use assert_cmd::Command;
use git_workon_fixture::prelude::*;

#[test]
fn find_exact_match() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("feature")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"));

    Ok(())
}

#[test]
fn find_no_match_errors() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("nonexistent")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No matching worktree"));

    Ok(())
}

#[test]
fn find_multiple_fuzzy_matches_errors_with_no_interactive() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("feature")
        .arg("--no-interactive")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Multiple worktrees match"));

    Ok(())
}

#[test]
fn find_no_name_errors_with_no_interactive() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("--no-interactive")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No worktree name provided"));

    Ok(())
}

#[test]
fn find_with_dirty_filter() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean")
        .worktree("dirty")
        .build()?;

    // Make dirty worktree dirty
    std::fs::write(
        fixture.root()?.child("dirty").join("test.txt"),
        "uncommitted",
    )?;

    // Should find dirty when searching with --dirty
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("dirty")
        .arg("--dirty")
        .assert()
        .success();

    // Should NOT find clean with --dirty filter
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("clean")
        .arg("--dirty")
        .assert()
        .failure();

    Ok(())
}

#[test]
fn find_all_filtered_out_errors() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("--dirty")
        .arg("--no-interactive")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No worktrees match"));

    Ok(())
}

#[test]
fn find_single_fuzzy_match_returns_directly() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-branch")
        .worktree("other")
        .build()?;

    // "feature" matches only "feature-branch" → return without interaction
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("feature")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature-branch"));

    Ok(())
}

#[test]
fn find_case_insensitive_fuzzy_match() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("Feature-Branch")
        .build()?;

    // "feature" should match "Feature-Branch" (case-insensitive)
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("feature")
        .assert()
        .success();

    Ok(())
}
