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
fn doctor_detects_and_fixes_stale_worktree_name() -> Result<(), Box<dyn std::error::Error>> {
    // Simulate what a raw `git worktree move` leaves behind (ADR-027, "Consequences"):
    // the gitdir/.git pointer pair is rewritten to the new location, but the admin
    // directory keeps its old name — desyncing it from `encode_worktree_name` of the
    // worktree's current root-relative path.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    let bare_path = fixture.root()?.join(".bare");
    let old_path = fixture.root()?.join("feature");
    let new_path = fixture.root()?.join("ee").join("feature");
    std::fs::create_dir_all(new_path.parent().unwrap())?;
    std::fs::rename(&old_path, &new_path)?;

    let meta_dir = bare_path.join("worktrees").join("feature");
    std::fs::write(
        meta_dir.join("gitdir"),
        format!("{}\n", new_path.join(".git").display()),
    )?;
    std::fs::write(
        new_path.join(".git"),
        format!("gitdir: {}\n", meta_dir.display()),
    )?;

    let main_path = fixture.root()?.join("main");
    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("doctor")
        .assert()
        .success()
        .stderr(predicate::str::contains("admin directory name is stale"));

    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--fix")
        .assert()
        .success()
        .stderr(predicate::str::contains("Renamed admin directory"));

    // `git worktree list` (via git2) resolves the worktree under the repaired name.
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    assert!(bare_repo.find_worktree("ee~feature").is_ok());
    assert!(bare_repo.find_worktree("feature").is_err());

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

// ── stack / gt checks ─────────────────────────────────────────────────────────

/// Builds a PATH that excludes any directory containing a `gt` binary.
fn path_without_gt() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|dir| !std::path::Path::new(dir).join("gt").exists())
        .collect::<Vec<_>>()
        .join(":")
}

#[test]
fn doctor_warns_when_gt_not_found_but_does_not_fail() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .env("PATH", path_without_gt())
        .arg("doctor")
        .output()?;

    // gt missing must not cause a non-zero exit
    assert!(
        output.status.success(),
        "doctor should exit 0 even without gt; stderr: {}",
        std::str::from_utf8(&output.stderr).unwrap_or("(invalid utf8)")
    );
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(
        stderr.contains("gt"),
        "expected gt mention in stderr: {stderr}"
    );
    // Must use ⚠ (check_warn), not ✗ (check_fail)
    assert!(
        !stderr.contains("✗ gt"),
        "gt should warn not fail: {stderr}"
    );

    Ok(())
}

#[test]
fn doctor_json_gt_not_found_emits_gt_not_found_kind() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .env("PATH", path_without_gt())
        .arg("doctor")
        .arg("--json")
        .output()?;

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;
    let issues = parsed["issues"].as_array().expect("issues must be array");
    assert!(
        issues.iter().any(|i| i["kind"] == "gt_not_found"),
        "expected gt_not_found issue in: {stdout}"
    );

    Ok(())
}

#[test]
fn doctor_flags_invalid_stack_model_with_check_fail() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "branchless")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stderr = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("doctor")
            .output()?
            .stderr,
    )?;

    assert!(
        stderr.contains("workon.stackModel"),
        "expected key name in stderr: {stderr}"
    );
    assert!(
        stderr.contains("branchless"),
        "expected invalid value in stderr: {stderr}"
    );

    Ok(())
}

#[test]
fn doctor_json_invalid_stack_model_emits_invalid_stack_config(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "branchless")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--json")
        .output()?;

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;
    let issues = parsed["issues"].as_array().expect("issues must be array");
    let issue = issues
        .iter()
        .find(|i| i["kind"] == "invalid_stack_config")
        .unwrap_or_else(|| panic!("expected invalid_stack_config issue in: {stdout}"));

    assert_eq!(issue["key"], "workon.stackModel");
    assert_eq!(issue["value"], "branchless");
    assert!(issue["reason"].as_str().is_some(), "reason must be present");

    Ok(())
}

#[test]
fn doctor_json_invalid_stack_granularity_emits_invalid_stack_config(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackWorktreeGranularity", "diff")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--json")
        .output()?;

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;
    let issues = parsed["issues"].as_array().expect("issues must be array");
    let issue = issues
        .iter()
        .find(|i| {
            i["kind"] == "invalid_stack_config" && i["key"] == "workon.stackWorktreeGranularity"
        })
        .unwrap_or_else(|| panic!("expected invalid_stack_config for granularity in: {stdout}"));

    assert_eq!(issue["value"], "diff");

    Ok(())
}

#[test]
fn doctor_json_configuration_includes_stack_keys() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "none")
        .config("workon.gtAutoTrack", "false")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("doctor")
        .arg("--json")
        .output()?;

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;
    let config = &parsed["configuration"];

    assert_eq!(
        config["workon.stackModel"]["value"], "none",
        "configuration must include stackModel: {stdout}"
    );
    assert!(
        config.get("workon.stackWorktreeGranularity").is_some(),
        "configuration must include stackWorktreeGranularity: {stdout}"
    );
    assert_eq!(
        config["workon.gtAutoTrack"]["value"], "false",
        "configuration must include gtAutoTrack with set value: {stdout}"
    );

    Ok(())
}
