//! Tests for the repo-level branch signal helpers (`branch_has_gone_upstream`,
//! `branch_is_merged_into`, `branch_tip_at_or_behind`), the branch-only-row
//! counterparts to the `WorktreeDescriptor` methods they mirror.

use git_workon_fixture::prelude::*;
use workon::{branch_has_gone_upstream, branch_is_merged_into, branch_tip_at_or_behind};

#[test]
fn branch_has_gone_upstream_false_without_upstream() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .branch("feature")
        .build()?;

    let repo = fixture.repo()?;

    assert!(!branch_has_gone_upstream(repo, "feature")?);

    Ok(())
}

#[test]
fn branch_has_gone_upstream_false_when_upstream_exists() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .branch("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    let repo = fixture.repo()?;

    assert!(!branch_has_gone_upstream(repo, "feature")?);

    Ok(())
}

#[test]
fn branch_has_gone_upstream_true_when_upstream_ref_deleted(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .remote("origin", "/dev/null")
        .branch("feature")
        .upstream("feature", "origin/feature")
        .build()?;

    let repo = fixture.repo()?;
    repo.find_reference("refs/remotes/origin/feature")?
        .delete()?;

    assert!(branch_has_gone_upstream(repo, "feature")?);

    Ok(())
}

#[test]
fn branch_has_gone_upstream_false_for_missing_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let repo = fixture.repo()?;

    assert!(!branch_has_gone_upstream(repo, "nonexistent")?);

    Ok(())
}

#[test]
fn branch_is_merged_into_true_at_same_commit() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .branch("feature")
        .build()?;

    let repo = fixture.repo()?;

    assert!(branch_is_merged_into(repo, "feature", "main")?);

    Ok(())
}

#[test]
fn branch_is_merged_into_false_with_additional_commits() -> Result<(), Box<dyn std::error::Error>> {
    // A worktree is needed to write the extra commit; the helper itself doesn't care
    // whether "feature" has a worktree.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    fixture
        .commit("feature")
        .file("test.txt", "test")
        .create("Feature commit")?;

    let repo = fixture.repo()?;

    assert!(!branch_is_merged_into(repo, "feature", "main")?);

    Ok(())
}

#[test]
fn branch_is_merged_into_true_after_fast_forward() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    let feature_commit_oid = fixture
        .commit("feature")
        .file("feature.txt", "feature")
        .create("Feature commit")?;

    fixture.update_branch("main", feature_commit_oid)?;

    let repo = fixture.repo()?;

    assert!(branch_is_merged_into(repo, "feature", "main")?);

    Ok(())
}

#[test]
fn branch_is_merged_into_false_when_target_missing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .branch("feature")
        .build()?;

    let repo = fixture.repo()?;

    assert!(!branch_is_merged_into(repo, "feature", "nonexistent")?);

    Ok(())
}

#[test]
fn branch_is_merged_into_false_for_same_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let repo = fixture.repo()?;

    assert!(!branch_is_merged_into(repo, "main", "main")?);

    Ok(())
}

#[test]
fn branch_tip_at_or_behind_true_when_equal() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .branch("feature")
        .build()?;

    let repo = fixture.repo()?;
    let tip = repo
        .find_branch("feature", git2::BranchType::Local)?
        .get()
        .target()
        .unwrap()
        .to_string();

    assert!(branch_tip_at_or_behind(repo, "feature", &tip)?);

    Ok(())
}

#[test]
fn branch_tip_at_or_behind_true_when_oid_is_descendant() -> Result<(), Box<dyn std::error::Error>> {
    // "old" is a local branch with no worktree, sitting at the initial commit;
    // "feature" is a worktree branched from the same commit and then advanced. The
    // feature worktree's new commit is a descendant of "old"'s tip, so "old" reads as
    // at-or-behind it.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .branch("old")
        .worktree("feature")
        .build()?;

    let ahead_oid = fixture
        .commit("feature")
        .file("later.txt", "later")
        .create("Later commit")?;

    let repo = fixture.repo()?;

    assert!(branch_tip_at_or_behind(
        repo,
        "old",
        &ahead_oid.to_string()
    )?);

    Ok(())
}

#[test]
fn branch_tip_at_or_behind_false_when_oid_unknown() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .branch("feature")
        .build()?;

    let repo = fixture.repo()?;

    assert!(!branch_tip_at_or_behind(
        repo,
        "feature",
        "0000000000000000000000000000000000000000"
    )?);

    Ok(())
}

#[test]
fn branch_tip_at_or_behind_false_for_missing_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let repo = fixture.repo()?;

    assert!(!branch_tip_at_or_behind(
        repo,
        "nonexistent",
        "0000000000000000000000000000000000000000"
    )?);

    Ok(())
}
