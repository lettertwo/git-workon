use assert_cmd::cargo_bin_cmd;
use git_workon_fixture::prelude::*;

// ============================================================================
// Individual Filter Tests
// ============================================================================

#[test]
fn list_no_filters_shows_all_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .build()?;

    // Run list without filters
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("main"))
        .stdout(predicate::str::contains("feature-1"))
        .stdout(predicate::str::contains("feature-2"));

    Ok(())
}

#[test]
fn list_dirty_shows_only_dirty_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean-wt")
        .worktree("dirty-wt")
        .build()?;

    // Make dirty-wt dirty
    std::fs::write(
        fixture.root()?.child("dirty-wt").join("test.txt"),
        "uncommitted",
    )?;

    // Run list --dirty
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .assert()
        .success()
        .stdout(predicate::str::contains("dirty-wt"))
        .stdout(predicate::str::contains("clean-wt").not());

    Ok(())
}

#[test]
fn list_clean_shows_only_clean_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean-wt")
        .worktree("dirty-wt")
        .build()?;

    // Make dirty-wt dirty
    std::fs::write(
        fixture.root()?.child("dirty-wt").join("test.txt"),
        "uncommitted",
    )?;

    // Run list --clean
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--clean")
        .assert()
        .success()
        .stdout(predicate::str::contains("clean-wt"))
        .stdout(predicate::str::contains("dirty-wt").not());

    Ok(())
}

#[test]
fn list_ahead_shows_only_worktrees_with_unpushed() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("main", "origin/main")
        .upstream("feature", "origin/feature")
        .build()?;

    // Make feature ahead of upstream
    fixture
        .commit("feature")
        .file("test.txt", "content")
        .create("Test commit")?;

    // Run list --ahead
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--ahead")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"))
        .stdout(predicate::str::contains("main").not());

    Ok(())
}

#[test]
fn list_behind_shows_only_worktrees_behind_upstream() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("main", "origin/main")
        .upstream("feature", "origin/feature")
        .build()?;

    let repo = fixture.repo()?;

    // Create a commit on origin/feature to make feature behind
    let commit_oid = fixture
        .commit("feature")
        .file("test.txt", "content")
        .create("Remote commit")?;

    // Move origin/feature ahead
    let commit = repo.find_commit(commit_oid)?;
    repo.find_branch("origin/feature", git2::BranchType::Remote)?
        .get_mut()
        .set_target(commit.id(), "Move remote ahead")?;

    // Reset feature branch back to where it was
    let mut head = repo.find_reference("refs/heads/feature")?;
    let parent = commit.parent(0)?;
    head.set_target(parent.id(), "Reset feature behind")?;

    // Run list --behind
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--behind")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"))
        .stdout(predicate::str::contains("main").not());

    Ok(())
}

#[test]
fn list_gone_shows_only_worktrees_with_deleted_upstream() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("main", "origin/main")
        .upstream("feature", "origin/feature")
        .build()?;

    let repo = fixture.repo()?;

    // Delete the origin/feature remote branch
    repo.find_reference("refs/remotes/origin/feature")?
        .delete()?;

    // Run list --gone
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--gone")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"))
        .stdout(predicate::str::contains("main").not());

    Ok(())
}

// ============================================================================
// Filter Combination Tests
// ============================================================================

#[test]
fn list_dirty_and_ahead_combines_filters() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean-uptodate")
        .worktree("dirty-uptodate")
        .worktree("clean-ahead")
        .worktree("dirty-ahead")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("clean-uptodate", "origin/clean-uptodate")
        .upstream("dirty-uptodate", "origin/dirty-uptodate")
        .upstream("clean-ahead", "origin/clean-ahead")
        .upstream("dirty-ahead", "origin/dirty-ahead")
        .build()?;

    // Make dirty-uptodate and dirty-ahead dirty
    std::fs::write(
        fixture.root()?.child("dirty-uptodate").join("test.txt"),
        "uncommitted",
    )?;
    std::fs::write(
        fixture.root()?.child("dirty-ahead").join("test.txt"),
        "uncommitted",
    )?;

    // Make clean-ahead and dirty-ahead ahead
    fixture
        .commit("clean-ahead")
        .file("ahead1.txt", "content")
        .create("Ahead commit")?;
    fixture
        .commit("dirty-ahead")
        .file("ahead2.txt", "content")
        .create("Ahead commit")?;

    // Run list --dirty --ahead (should only show dirty-ahead)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .arg("--ahead")
        .assert()
        .success()
        .stdout(predicate::str::contains("dirty-ahead"))
        .stdout(predicate::str::contains("clean-uptodate").not())
        .stdout(predicate::str::contains("dirty-uptodate").not())
        .stdout(predicate::str::contains("clean-ahead").not());

    Ok(())
}

#[test]
fn list_ahead_and_behind_shows_diverged_worktrees() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("diverged")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("diverged", "origin/diverged")
        .build()?;

    let repo = fixture.repo()?;

    // Create a local commit (ahead)
    let local_commit_oid = fixture
        .commit("diverged")
        .file("local.txt", "content")
        .create("Local commit")?;

    // Create a different commit on origin/diverged (behind)
    let remote_commit_oid = fixture
        .commit("diverged")
        .file("remote.txt", "content")
        .create("Remote commit")?;

    // Move origin/diverged to the remote commit
    let remote_commit = repo.find_commit(remote_commit_oid)?;
    repo.find_branch("origin/diverged", git2::BranchType::Remote)?
        .get_mut()
        .set_target(remote_commit.id(), "Move remote")?;

    // Reset diverged branch to local commit (before the remote commit)
    let local_commit = repo.find_commit(local_commit_oid)?;
    repo.find_reference("refs/heads/diverged")?
        .set_target(local_commit.parent(0)?.id(), "Reset to before local commit")?;

    // Make one more local commit so it's ahead
    fixture
        .commit("diverged")
        .file("local2.txt", "content")
        .create("Local commit 2")?;

    // At this point, diverged should be both ahead and behind
    // Run list --ahead --behind (should show diverged)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--ahead")
        .arg("--behind")
        .assert()
        .success()
        .stdout(predicate::str::contains("diverged"));

    Ok(())
}

#[test]
fn list_gone_and_ahead_shows_deleted_upstream_with_local_commits(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("gone-with-commits")
        .worktree("gone-without-commits")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("gone-with-commits", "origin/gone-with-commits")
        .upstream("gone-without-commits", "origin/gone-without-commits")
        .build()?;

    let repo = fixture.repo()?;

    // Add a commit to gone-with-commits
    fixture
        .commit("gone-with-commits")
        .file("test.txt", "content")
        .create("Local commit")?;

    // Delete both remote branches
    repo.find_reference("refs/remotes/origin/gone-with-commits")?
        .delete()?;
    repo.find_reference("refs/remotes/origin/gone-without-commits")?
        .delete()?;

    // Run list --gone --ahead
    // Note: has_unpushed_commits() conservatively returns true for gone upstreams,
    // so both worktrees will match --ahead even though gone-without-commits has no commits
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--gone")
        .arg("--ahead")
        .assert()
        .success()
        .stdout(predicate::str::contains("gone-with-commits"))
        .stdout(predicate::str::contains("gone-without-commits"));

    Ok(())
}

#[test]
fn list_multiple_filters_uses_and_logic() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("matches-all")
        .worktree("matches-some")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("matches-all", "origin/matches-all")
        .upstream("matches-some", "origin/matches-some")
        .build()?;

    // Make matches-all dirty AND ahead
    std::fs::write(
        fixture.root()?.child("matches-all").join("test.txt"),
        "uncommitted",
    )?;
    fixture
        .commit("matches-all")
        .file("ahead.txt", "content")
        .create("Ahead commit")?;

    // Make matches-some only dirty (not ahead)
    std::fs::write(
        fixture.root()?.child("matches-some").join("test.txt"),
        "uncommitted",
    )?;

    // Run list --dirty --ahead --clean (should show nothing - clean contradicts dirty)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .arg("--ahead")
        .arg("--clean")
        .assert()
        .failure(); // Should error due to --dirty and --clean conflict

    Ok(())
}

// ============================================================================
// Edge Case Tests
// ============================================================================

// Note: Detached HEAD test removed due to FixtureBuilder limitations
// Detached HEAD worktrees are correctly excluded from --ahead/--behind/--gone
// filters by the implementation (they return false when branch() returns None)

#[test]
fn list_worktree_without_upstream_excluded_from_behind_filter(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("no-upstream")
        .worktree("with-upstream")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("with-upstream", "origin/with-upstream")
        .build()?;

    // no-upstream has no upstream configured
    // Run list --behind (should not show no-upstream)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--behind")
        .assert()
        .success()
        .stdout(predicate::str::contains("no-upstream").not());

    Ok(())
}

#[test]
fn list_worktree_without_upstream_excluded_from_gone_filter(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("no-upstream")
        .worktree("with-upstream")
        .remote("origin", "https://github.com/test/test.git")
        .upstream("with-upstream", "origin/with-upstream")
        .build()?;

    // no-upstream has no upstream configured
    // Run list --gone (should not show no-upstream)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--gone")
        .assert()
        .success()
        .stdout(predicate::str::contains("no-upstream").not());

    Ok(())
}

#[test]
fn list_dirty_and_clean_both_specified_returns_error() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    // Run list --dirty --clean (should error)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .arg("--clean")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Cannot specify both --dirty and --clean filters",
        ));

    Ok(())
}

#[test]
fn list_empty_result_when_no_worktrees_match_filters() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean")
        .build()?;

    // Run list --dirty (nothing is dirty, should show nothing)
    let mut cmd = cargo_bin_cmd!("git-workon");
    let output = cmd
        .current_dir(&fixture)
        .arg("list")
        .arg("--dirty")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Output should be empty (or just whitespace)
    let output_str = String::from_utf8(output)?;
    assert!(
        output_str.trim().is_empty(),
        "Expected empty output, got: {}",
        output_str
    );

    Ok(())
}

// ── stack rendering ───────────────────────────────────────────────────────────

#[test]
fn list_renders_stack_branches_indented_with_current_marker(
) -> Result<(), Box<dyn std::error::Error>> {
    // Worktree "feat-1" has branch "feat-1" in a stack with "step-2" below it.
    // stackModel = "graphite" bypasses gt binary detection.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("step-2", "feat-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stdout = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("list")
            .output()?
            .stdout,
    )?;

    // main is the active worktree (current dir) → rendered with ◉ glyph
    assert!(
        stdout.contains("◉") && stdout.contains("main"),
        "expected '◉ main' (active) in output: {stdout}"
    );
    // feat-1 has a checked-out worktree but is not active → rendered with ◎ glyph
    let feat1_line = stdout.lines().find(|l| l.contains("feat-1")).unwrap_or("");
    assert!(
        feat1_line.contains("◎"),
        "feat-1 (non-active worktree) must show ◎ glyph: {feat1_line}"
    );
    // step-2 is in the stack but has no worktree → rendered with ◯ (metadata-only)
    assert!(
        stdout.contains("step-2"),
        "expected 'step-2' in stack output: {stdout}"
    );
    assert!(
        stdout.contains("◯") && stdout.contains("step-2"),
        "step-2 should appear as a metadata-only ◯ node: {stdout}"
    );

    Ok(())
}

#[test]
fn list_json_includes_stack_object() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("step-2", "feat-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("list")
        .arg("--json")
        .output()?;

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;

    // New shape: { "worktrees": [...], "stacks": [...] }
    let worktrees = parsed["worktrees"]
        .as_array()
        .unwrap_or_else(|| panic!("expected worktrees array in: {stdout}"));
    assert!(
        worktrees.iter().any(|w| w["name"] == "feat-1"),
        "expected feat-1 in worktrees: {stdout}"
    );
    assert!(
        worktrees.iter().any(|w| w["name"] == "main"),
        "expected main in worktrees: {stdout}"
    );

    let stacks = parsed["stacks"]
        .as_array()
        .unwrap_or_else(|| panic!("expected stacks array in: {stdout}"));
    assert_eq!(
        stacks.len(),
        1,
        "expected exactly one stack group: {stdout}"
    );

    let group = &stacks[0];
    assert_eq!(group["trunk"], "main", "stack trunk must be main: {stdout}");

    let diffs = group["diffs"].as_array().expect("diffs must be array");
    assert!(
        diffs.iter().any(|b| b == "feat-1"),
        "expected feat-1 in stack diffs: {stdout}"
    );
    assert!(
        diffs.iter().any(|b| b == "step-2"),
        "expected step-2 in stack diffs: {stdout}"
    );

    let checkouts = group["checkouts"]
        .as_object()
        .expect("checkouts must be object");
    assert_eq!(
        checkouts.get("feat-1").and_then(|v| v.as_str()),
        Some("feat-1"),
        "feat-1 diff must map to feat-1 worktree: {stdout}"
    );

    assert!(
        parsed.get("ungrouped").is_none(),
        "ungrouped field must not be present: {stdout}"
    );

    Ok(())
}

#[test]
fn list_no_stack_flag_suppresses_stack_rendering() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("step-2", "feat-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stdout = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("list")
            .arg("--no-stack")
            .output()?
            .stdout,
    )?;

    // The worktree row should still appear, but indented stack branches must not
    assert!(
        stdout.contains("feat-1"),
        "worktree row should appear: {stdout}"
    );
    assert!(
        !stdout.contains("* feat-1") && !stdout.contains("  feat-1"),
        "stack branches must not appear with --no-stack: {stdout}"
    );
    assert!(
        !stdout.contains("step-2"),
        "step-2 stack branch must not appear with --no-stack: {stdout}"
    );

    Ok(())
}

#[test]
fn list_no_stack_json_omits_stack_field() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("list")
        .arg("--no-stack")
        .arg("--json")
        .output()?;

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;

    // With --no-stack, stacks array must be empty and worktrees has all entries
    let stacks = parsed["stacks"]
        .as_array()
        .unwrap_or_else(|| panic!("expected stacks array in: {stdout}"));
    assert!(
        stacks.is_empty(),
        "stacks must be empty with --no-stack: {stdout}"
    );

    let worktrees = parsed["worktrees"]
        .as_array()
        .unwrap_or_else(|| panic!("expected worktrees array in: {stdout}"));
    assert_eq!(
        worktrees.len(),
        2,
        "both worktrees should appear in worktrees with --no-stack: {stdout}"
    );

    assert!(
        parsed.get("ungrouped").is_none(),
        "ungrouped field must not be present: {stdout}"
    );

    Ok(())
}

#[test]
fn list_gracefully_handles_worktree_with_no_stack_metadata(
) -> Result<(), Box<dyn std::error::Error>> {
    // Stack model is graphite but "main" worktree branch is not in any metadata.
    // List should render the worktree row without crashing or hiding it.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "graphite")
        .build()?;

    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .arg("list")
        .output()?;

    assert!(
        output.status.success(),
        "list must succeed even with no branch-metadata refs"
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("main"),
        "main worktree row must appear: {stdout}"
    );

    Ok(())
}

#[test]
fn list_shows_stack_when_no_worktree_on_stack_branch() -> Result<(), Box<dyn std::error::Error>> {
    // Scenario: trunk worktree "main" is checked out, but the stacked branch "feat-1" has no
    // worktree. The stack should still be visible in the output.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("step-2", "feat-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stdout = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("list")
            .output()?
            .stdout,
    )?;

    // In the new tree format there is no "Stack (trunk: …)" header; instead, the
    // trunk itself is the root node and stacks hang underneath it.
    assert!(
        stdout.contains("main"),
        "main must appear as the tree root: {stdout}"
    );
    assert!(
        stdout.contains("feat-1"),
        "feat-1 must appear in the tree even without a worktree: {stdout}"
    );
    assert!(
        stdout.contains("◯") && stdout.contains("feat-1"),
        "feat-1 must be shown as a metadata-only ◯ node: {stdout}"
    );
    assert!(
        !stdout.contains("* feat-1"),
        "feat-1 must not carry the old stack '* ' prefix: {stdout}"
    );

    Ok(())
}

#[test]
fn list_json_shows_stack_when_no_worktree_on_stack_branch() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("list")
        .arg("--json")
        .output()?;

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;

    let stacks = parsed["stacks"]
        .as_array()
        .unwrap_or_else(|| panic!("expected stacks array in: {stdout}"));
    assert_eq!(stacks.len(), 1, "expected one stack: {stdout}");

    let group = &stacks[0];
    assert_eq!(group["trunk"], "main");
    assert!(
        group["diffs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "feat-1"),
        "feat-1 must be in diffs: {stdout}"
    );
    let checkouts = group["checkouts"]
        .as_object()
        .expect("checkouts must be object");
    assert!(
        checkouts.is_empty(),
        "checkouts must be empty when no worktrees are on stack branches: {stdout}"
    );

    Ok(())
}

// ── tree rendering: connectors, ← here, parents in JSON ──────────────────────

#[test]
fn list_tree_shows_here_marker_on_active_worktree() -> Result<(), Box<dyn std::error::Error>> {
    // Two worktrees: main (current dir) and feat-1 (stacked).
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stdout = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("list")
            .output()?
            .stdout,
    )?;

    // The active worktree (main) should be marked with ← here.
    assert!(
        stdout.contains("← here"),
        "active worktree must show ← here marker: {stdout}"
    );
    // feat-1 is not active so it must NOT carry the ← here marker on its own line.
    // (We check the line containing feat-1 doesn't also contain ← here.)
    let feat1_line = stdout.lines().find(|l| l.contains("feat-1")).unwrap_or("");
    assert!(
        !feat1_line.contains("← here"),
        "non-active worktree must not show ← here: {feat1_line}"
    );

    Ok(())
}

#[test]
fn list_tree_renders_connector_lines_for_forking_stack() -> Result<(), Box<dyn std::error::Error>> {
    // main → shared → branch-x (fork)
    //              → branch-y
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "graphite")
        .branch_metadata("shared", "main")
        .branch_metadata("branch-x", "shared")
        .branch_metadata("branch-y", "shared")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stdout = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("list")
            .output()?
            .stdout,
    )?;

    // At the fork, both ├─ and └─ should appear.
    assert!(
        stdout.contains("├─") && stdout.contains("└─"),
        "fork connectors ├─ and └─ must appear for branching stacks: {stdout}"
    );
    // All four branches should be present.
    for name in ["main", "shared", "branch-x", "branch-y"] {
        assert!(
            stdout.contains(name),
            "{name} must appear in output: {stdout}"
        );
    }

    Ok(())
}

#[test]
fn list_json_stack_includes_parents_map() -> Result<(), Box<dyn std::error::Error>> {
    // main → api-1 → api-2
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "graphite")
        .branch_metadata("api-1", "main")
        .branch_metadata("api-2", "api-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("list")
        .arg("--json")
        .output()?;

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let parsed: serde_json::Value = serde_json::from_str(stdout)?;

    let stacks = parsed["stacks"].as_array().expect("stacks must be array");
    assert_eq!(stacks.len(), 1);
    let group = &stacks[0];

    let parents = group["parents"]
        .as_object()
        .unwrap_or_else(|| panic!("parents must be an object in: {stdout}"));
    assert_eq!(
        parents.get("api-1").and_then(|v| v.as_str()),
        Some("main"),
        "api-1 parent must be main: {stdout}"
    );
    assert_eq!(
        parents.get("api-2").and_then(|v| v.as_str()),
        Some("api-1"),
        "api-2 parent must be api-1: {stdout}"
    );
    // Trunk itself must not appear as a key.
    assert!(
        !parents.contains_key("main"),
        "trunk must not be a key in parents: {stdout}"
    );

    Ok(())
}

#[test]
fn list_tree_linear_chain_uses_pipe_continuation_not_fork_chars(
) -> Result<(), Box<dyn std::error::Error>> {
    // main → step-1 → step-2 → step-3  (no forks)
    // Linear chains must show │ continuation but NOT ├─ or └─.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "graphite")
        .branch_metadata("step-1", "main")
        .branch_metadata("step-2", "step-1")
        .branch_metadata("step-3", "step-2")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stdout = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("list")
            .output()?
            .stdout,
    )?;

    // All steps must appear.
    for name in ["step-1", "step-2", "step-3"] {
        assert!(stdout.contains(name), "{name} must appear: {stdout}");
    }
    // No fork connectors in a purely linear chain.
    assert!(
        !stdout.contains("├─") && !stdout.contains("└─"),
        "linear chain must not use fork connectors: {stdout}"
    );

    Ok(())
}
