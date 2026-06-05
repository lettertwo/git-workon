use git_workon_fixture::prelude::*;
use std::error::Error;
use workon::{resolve_action, Resolution, StackModel};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Bare repo with a `main` worktree and a `feat-a` worktree.
fn bare_two_worktrees() -> Result<Fixture, Box<dyn Error>> {
    FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-a")
        .build()
}

/// Bare repo with a `main` worktree and a local branch `feat-a` (no worktree).
fn bare_branch_no_worktree() -> Result<Fixture, Box<dyn Error>> {
    FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .branch("feat-a")
        .build()
}

// ── rule 1: T has its own worktree → Navigate ─────────────────────────────────

#[test]
fn rule1_worktree_name_navigates() -> Result<(), Box<dyn Error>> {
    let fixture = bare_two_worktrees()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "feat-a", StackModel::None),
        Resolution::Navigate
    );
    Ok(())
}

#[test]
fn rule1_trunk_worktree_navigates() -> Result<(), Box<dyn Error>> {
    let fixture = bare_two_worktrees()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "main", StackModel::None),
        Resolution::Navigate
    );
    Ok(())
}

#[test]
fn rule1_branch_name_navigates_when_worktree_named_same() -> Result<(), Box<dyn Error>> {
    // find_worktree matches by branch name too, so workon("feat-a") finds the
    // "feat-a" worktree regardless of whether we pass the worktree name or branch name.
    let fixture = bare_two_worktrees()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "feat-a", StackModel::Graphite),
        Resolution::Navigate
    );
    Ok(())
}

// ── rule 4 (no-stack): branch exists → Materialize ───────────────────────────

#[test]
fn rule4_existing_local_branch_materializes() -> Result<(), Box<dyn Error>> {
    let fixture = bare_branch_no_worktree()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "feat-a", StackModel::None),
        Resolution::Materialize
    );
    Ok(())
}

#[test]
fn rule4_unknown_name_is_not_found() -> Result<(), Box<dyn Error>> {
    let fixture = bare_branch_no_worktree()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "no-such-branch", StackModel::None),
        Resolution::NotFound
    );
    Ok(())
}

// ── StackModel::None degradation ─────────────────────────────────────────────
// Even with graphite metadata, StackModel::None collapses to navigate / materialize /
// not-found — rules 2 and 3 never fire.

#[test]
fn no_stack_model_sees_only_navigate_materialize_notfound() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-a")
        .branch("feat-b")
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .branch_metadata("feat-b", "main")
        .build()?;
    let repo = fixture.repo()?;

    // Rule 1 still fires.
    assert_eq!(
        resolve_action(repo, "feat-a", StackModel::None),
        Resolution::Navigate
    );
    // Branch without worktree → Materialize (not Checkout, even though metadata exists).
    assert_eq!(
        resolve_action(repo, "feat-b", StackModel::None),
        Resolution::Materialize
    );
    // Completely unknown → NotFound.
    assert_eq!(
        resolve_action(repo, "ghost", StackModel::None),
        Resolution::NotFound
    );
    Ok(())
}

// ── rule 4 (stack-active): branch exists → Materialize ───────────────────────

#[test]
fn stack_active_existing_branch_no_worktree_materializes() -> Result<(), Box<dyn Error>> {
    // Graphite metadata exists for feat-a, but it has a live branch ref and no worktree.
    // Rules 2/3 are not yet wired: result is Materialize (rule 4).
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .branch("feat-a")
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .build()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "feat-a", StackModel::Graphite),
        Resolution::Materialize
    );
    Ok(())
}

// ── DeletedNode: metadata exists but branch ref was deleted ──────────────────

#[test]
fn deleted_branch_node_returns_deleted_node() -> Result<(), Box<dyn Error>> {
    // ghost-branch has graphite metadata (was `gt track`-ed) but NO local branch ref.
    // resolve_action should recognise it as a deleted ◯ node, not a plain typo.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .graphite_config(&["main"])
        .branch_metadata("ghost-branch", "main")
        // Deliberately NOT calling .branch("ghost-branch")
        .build()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "ghost-branch", StackModel::Graphite),
        Resolution::DeletedNode {
            branch: "ghost-branch".to_string()
        }
    );
    Ok(())
}

#[test]
fn deleted_branch_node_not_triggered_under_no_stack() -> Result<(), Box<dyn Error>> {
    // Same metadata, but StackModel::None → rules 2-3 skip, falls to rule 4 branch check →
    // NotFound (not DeletedNode). Degradation invariant.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .graphite_config(&["main"])
        .branch_metadata("ghost-branch", "main")
        .build()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "ghost-branch", StackModel::None),
        Resolution::NotFound
    );
    Ok(())
}
