use assert_cmd::Command;
use git_workon_fixture::prelude::*;

#[test]
fn doctor_healthy_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    // Doctor should succeed and report no worktree-level issues
    let output = Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("doctor")
        .output()?;

    assert!(output.status.success());
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(
        !stderr.contains("missing directory"),
        "unexpected worktree issue: {stderr}"
    );
    assert!(
        !stderr.contains("broken git link"),
        "unexpected worktree issue: {stderr}"
    );

    Ok(())
}

#[test]
fn doctor_detects_missing_directory() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    // Delete the feature worktree directory (fixture cwd is now "feature")
    let feature_path = fixture.cwd()?.to_path_buf();
    std::fs::remove_dir_all(&feature_path)?;

    // Run doctor from the main worktree
    let main_path = fixture.root()?.join("main");
    Command::cargo_bin("git-workon")?
        .current_dir(&main_path)
        .arg("doctor")
        .assert()
        .success()
        .stderr(predicate::str::contains("missing directory"));

    Ok(())
}

#[test]
fn doctor_fix_missing_directory() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    // Delete the feature worktree directory
    let feature_path = fixture.cwd()?.to_path_buf();
    std::fs::remove_dir_all(&feature_path)?;

    // Run doctor --fix from the main worktree
    let main_path = fixture.root()?.join("main");
    Command::cargo_bin("git-workon")?
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--fix")
        .assert()
        .success()
        .stderr(predicate::str::contains("Pruned: feature"));

    // Verify the worktree entry is removed from git
    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    assert!(
        bare_repo.find_worktree("feature").is_err(),
        "Expected worktree 'feature' to be pruned from git registry"
    );

    Ok(())
}

#[test]
fn doctor_dry_run_does_not_fix() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    // Delete the feature worktree directory
    let feature_path = fixture.cwd()?.to_path_buf();
    std::fs::remove_dir_all(&feature_path)?;

    // Run doctor --dry-run from the main worktree
    let main_path = fixture.root()?.join("main");
    Command::cargo_bin("git-workon")?
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicate::str::contains("Would fix 1 issue(s)"));

    // Verify the worktree entry is still registered (not pruned)
    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    assert!(
        bare_repo.find_worktree("feature").is_ok(),
        "Expected worktree 'feature' to still be in git registry after dry-run"
    );

    Ok(())
}

#[test]
fn doctor_json_output() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    // Delete the feature worktree directory to produce a worktree issue
    let feature_path = fixture.cwd()?.to_path_buf();
    std::fs::remove_dir_all(&feature_path)?;

    let main_path = fixture.root()?.join("main");
    let output = Command::cargo_bin("git-workon")?
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--json")
        .output()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        std::str::from_utf8(&output.stderr).unwrap_or("(invalid utf8)")
    );

    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;

    let issues = parsed["issues"]
        .as_array()
        .expect("issues should be an array");
    let has_missing = issues
        .iter()
        .any(|i| i["kind"] == "missing_directory" && i["name"] == "feature");
    assert!(
        has_missing,
        "Expected missing_directory issue for 'feature' in: {stdout}"
    );

    Ok(())
}
