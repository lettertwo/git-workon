use assert_cmd::cargo_bin_cmd;
use git_workon_fixture::prelude::*;

fn set_local_config_bool(
    bare_path: &std::path::Path,
    key: &str,
    value: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = git2::Repository::open_bare(bare_path)?;
    let mut config = repo.config()?;
    config.set_bool(key, value)?;
    Ok(())
}

#[test]
fn doctor_healthy_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    // Doctor should succeed and report no worktree-level issues
    let output = cargo_bin_cmd!("git-workon")
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
    cargo_bin_cmd!("git-workon")
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
    cargo_bin_cmd!("git-workon")
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
    cargo_bin_cmd!("git-workon")
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
    let output = cargo_bin_cmd!("git-workon")
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

#[test]
fn doctor_warns_renamed_auto_copy_untracked() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    let bare_path = fixture.root()?.join(".bare");
    set_local_config_bool(&bare_path, "workon.autoCopyUntracked", true)?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("doctor")
        .output()?;

    assert!(output.status.success());
    let stderr = std::str::from_utf8(&output.stderr)?;

    assert!(
        stderr.contains("workon.autoCopyUntracked"),
        "expected old key name in stderr: {stderr}"
    );
    assert!(
        stderr.contains("workon.autoCopy"),
        "expected new key name in stderr: {stderr}"
    );
    assert!(
        stderr.contains("renamed") || stderr.contains("no longer"),
        "expected rename messaging (not 'deprecated') in stderr: {stderr}"
    );
    assert!(
        !stderr.contains("deprecated"),
        "should not say 'deprecated' — the key was renamed, not deprecated: {stderr}"
    );

    Ok(())
}

#[test]
fn doctor_json_emits_renamed_config_issue() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    let bare_path = fixture.root()?.join(".bare");
    set_local_config_bool(&bare_path, "workon.autoCopyUntracked", true)?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
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
    let issue = issues
        .iter()
        .find(|i| i["kind"] == "renamed_config_key")
        .unwrap_or_else(|| panic!("expected renamed_config_key issue in: {stdout}"));

    assert_eq!(issue["old_key"], "workon.autoCopyUntracked");
    assert_eq!(issue["new_key"], "workon.autoCopy");
    assert_eq!(issue["value"], "true");
    assert_eq!(issue["new_already_set"], false);
    assert_eq!(issue["fixable"], true);

    Ok(())
}

#[test]
fn doctor_fix_migrates_renamed_auto_copy_untracked() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    let bare_path = fixture.root()?.join(".bare");
    set_local_config_bool(&bare_path, "workon.autoCopyUntracked", true)?;

    let main_path = fixture.root()?.join("main");
    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--fix")
        .assert()
        .success()
        .stderr(predicate::str::contains("Migrated:"))
        .stderr(predicate::str::contains("workon.autoCopyUntracked"))
        .stderr(predicate::str::contains("workon.autoCopy"));

    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    bare_repo.assert(predicate::repo::has_config("workon.autoCopy", Some("true")));

    let config = bare_repo.config()?;
    assert!(
        config.get_bool("workon.autoCopyUntracked").is_err(),
        "old key should have been removed"
    );

    Ok(())
}

#[test]
fn doctor_fix_removes_old_key_when_new_already_set() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    let bare_path = fixture.root()?.join(".bare");
    set_local_config_bool(&bare_path, "workon.autoCopyUntracked", false)?;
    set_local_config_bool(&bare_path, "workon.autoCopy", true)?;

    let main_path = fixture.root()?.join("main");
    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--fix")
        .assert()
        .success()
        .stderr(predicate::str::contains("Migrated:"));

    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    // New key should remain true (not overwritten by the old false value)
    bare_repo.assert(predicate::repo::has_config("workon.autoCopy", Some("true")));

    let config = bare_repo.config()?;
    assert!(
        config.get_bool("workon.autoCopyUntracked").is_err(),
        "old key should have been removed"
    );

    Ok(())
}
