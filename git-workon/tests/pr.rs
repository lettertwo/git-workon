use git_workon_fixture::prelude::*;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn pr_reference_parsing() -> Result {
    // Test that PR references are correctly identified
    assert!(workon::is_pr_reference("#123"));
    assert!(workon::is_pr_reference("pr#456"));
    assert!(workon::is_pr_reference("pr-789"));
    assert!(workon::is_pr_reference(
        "https://github.com/owner/repo/pull/999"
    ));

    // Regular branch names should not be identified as PR references
    assert!(!workon::is_pr_reference("my-feature"));
    assert!(!workon::is_pr_reference("main"));

    Ok(())
}

#[test]
fn pr_format_substitution() -> Result {
    // Create a bare repo to test config
    let fixture = FixtureBuilder::new().bare(true).build()?;

    let repo = fixture.repo()?;
    let config = workon::WorkonConfig::new(repo)?;

    // Test default format
    let format = config.pr_format(None)?;
    assert!(format.contains("{number}"));

    // Test formatting
    let name = workon::format_pr_name(&format, 123);
    assert_eq!(name, "pr-123");

    Ok(())
}

#[test]
fn pr_format_custom_config() -> Result {
    // Create a bare repo with custom PR format config
    let fixture = FixtureBuilder::new()
        .bare(true)
        .config("workon.prFormat", "review-{number}")
        .build()?;

    let repo = fixture.repo()?;
    let config = workon::WorkonConfig::new(repo)?;

    // Test custom format
    let format = config.pr_format(None)?;
    assert_eq!(format, "review-{number}");

    // Test formatting with custom format
    let name = workon::format_pr_name(&format, 456);
    assert_eq!(name, "review-456");

    Ok(())
}

#[test]
fn remote_auto_detection_origin() -> Result {
    // Create a bare repo with origin remote
    let origin = FixtureBuilder::new().bare(true).build()?;

    let local = FixtureBuilder::new()
        .bare(true)
        .remote("origin", &origin)
        .build()?;

    let repo = local.repo()?;

    // Should detect origin
    let remote = workon::detect_pr_remote(repo)?;
    assert_eq!(remote, "origin");

    Ok(())
}

#[test]
fn remote_auto_detection_upstream_priority() -> Result {
    // Create bare repos
    let origin = FixtureBuilder::new().bare(true).build()?;
    let upstream = FixtureBuilder::new().bare(true).build()?;

    let local = FixtureBuilder::new()
        .bare(true)
        .remote("origin", &origin)
        .build()?;

    // Add upstream remote
    local.add_remote("upstream", upstream.cwd()?.to_str().unwrap())?;

    let repo = local.repo()?;

    // Should detect upstream (higher priority than origin)
    let remote = workon::detect_pr_remote(repo)?;
    assert_eq!(remote, "upstream");

    Ok(())
}

#[test]
fn remote_auto_detection_no_remote_error() -> Result {
    // Create a bare repo without any remotes
    let fixture = FixtureBuilder::new().bare(true).build()?;

    let repo = fixture.repo()?;

    // Should error when no remotes configured
    let result = workon::detect_pr_remote(repo);
    assert!(result.is_err());

    Ok(())
}

// Note: Testing actual PR fetching and worktree creation would require
// setting up a real GitHub repository or mocking the git fetch command.
// These tests verify the core logic without requiring network access.

#[test]
fn pr_reference_with_base_flag_creates_literal_branch() -> Result {
    use assert_cmd::Command;

    // Create a bare repo with a worktree
    let fixture = FixtureBuilder::new().bare(true).worktree("main").build()?;

    // Run command from the main worktree directory
    let worktree_path = fixture.cwd()?;

    // Try to create worktree with PR-like name but --base flag
    // Should create literal branch named "#123" based on main
    Command::cargo_bin("git-workon")?
        .current_dir(&worktree_path)
        .arg("new")
        .arg("#123")
        .arg("--base")
        .arg("main")
        .assert()
        .success();

    // Should have created worktree with literal name
    let bare_repo = git2::Repository::open(fixture.root()?.join(".bare"))?;
    bare_repo.assert(predicate::repo::has_worktree("#123"));
    bare_repo.assert(predicate::repo::has_branch("#123"));

    Ok(())
}

#[test]
fn pr_reference_with_orphan_flag_creates_literal_branch() -> Result {
    use assert_cmd::Command;

    // Create a bare repo with a worktree
    let fixture = FixtureBuilder::new().bare(true).worktree("main").build()?;

    // Run command from the main worktree directory
    let worktree_path = fixture.cwd()?;

    // Try to create worktree with PR-like name but --orphan flag
    // Should create orphan branch named "pr#456"
    Command::cargo_bin("git-workon")?
        .current_dir(&worktree_path)
        .arg("new")
        .arg("pr#456")
        .arg("--orphan")
        .assert()
        .success();

    // Should have created worktree with literal name
    let bare_repo = git2::Repository::open(fixture.root()?.join(".bare"))?;
    bare_repo.assert(predicate::repo::has_worktree("pr#456"));
    bare_repo.assert(predicate::repo::has_branch("pr#456"));

    Ok(())
}
