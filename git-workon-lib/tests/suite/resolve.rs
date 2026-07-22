use git_workon_fixture::prelude::*;
use serial_test::serial;
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
    // feat-a has metadata (parent = main, the trunk) and a branch ref but no worktree.
    // Rule 2 skips (CWD is not inside any fixture worktree during tests).
    // Rule 3 skips (parent is the trunk — we never host on the trunk worktree).
    // Rule 4 fires: branch exists → Materialize.
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
    // ghost-branch has graphite metadata (was `gt track`-ed) but NO local branch ref —
    // simulating a branch that was merged and deleted while Graphite's record lingered.
    // resolve_action should recognise it as a deleted ◯ node, not a plain typo.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .graphite_config(&["main"])
        .ghost_branch_metadata("ghost-branch", "main") // metadata only, no git ref
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
    // Same ghost metadata (no git ref), but StackModel::None → rules 2-3 skip, falls to
    // rule 4 branch check → NotFound (not DeletedNode). Degradation invariant.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .graphite_config(&["main"])
        .ghost_branch_metadata("ghost-branch", "main") // metadata only, no git ref
        .build()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "ghost-branch", StackModel::None),
        Resolution::NotFound
    );
    Ok(())
}

// ── Rule 2: current worktree shares T's stack → Checkout in current worktree ─

/// Rule 2 requires `current_worktree` to succeed, which reads `std::env::current_dir()`.
/// We `set_current_dir` into the fixture worktree, then restore it. `#[serial]` ensures
/// this process-global mutation doesn't race with other tests.
#[test]
#[serial]
fn rule2_current_worktree_in_same_stack_checks_out_in_place() -> Result<(), Box<dyn Error>> {
    // Stack: main → feat-a. Worktrees: main, feat-a.
    // CWD = feat-a worktree. workon("feat-b") where feat-b is in the same stack.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-a")
        .branch("feat-b")
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .branch_metadata("feat-b", "feat-a")
        .build()?;

    let worktree_path = fixture.root()?.path().join("feat-a");
    let saved_cwd = std::env::current_dir()?;
    std::env::set_current_dir(&worktree_path)?;

    let result = (|| -> Result<Resolution, Box<dyn Error>> {
        let repo = fixture.repo()?;
        Ok(resolve_action(repo, "feat-b", StackModel::Graphite))
    })();

    std::env::set_current_dir(saved_cwd)?;

    assert_eq!(
        result?,
        Resolution::Checkout {
            host: "feat-a".to_string()
        }
    );
    Ok(())
}

#[test]
#[serial]
fn rule2_never_hosts_on_the_trunk_worktree() -> Result<(), Box<dyn Error>> {
    // Stack: main → feat-a. CWD = main worktree.
    // ADR-024: the trunk worktree is never a checkout host. Sitting on trunk
    // falls through to rule 4 (materialize) instead of moving main's HEAD.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .branch("feat-a")
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .build()?;

    let worktree_path = fixture.root()?.path().join("main");
    let saved_cwd = std::env::current_dir()?;
    std::env::set_current_dir(&worktree_path)?;

    let result = (|| -> Result<Resolution, Box<dyn Error>> {
        let repo = fixture.repo()?;
        Ok(resolve_action(repo, "feat-a", StackModel::Graphite))
    })();

    std::env::set_current_dir(saved_cwd)?;

    assert_eq!(result?, Resolution::Materialize);
    Ok(())
}

#[test]
#[serial]
fn rule1_does_not_navigate_to_stale_worktree_name() -> Result<(), Box<dyn Error>> {
    // Worktree "feat-a" had its HEAD moved to feat-b by an in-place checkout.
    // `workon feat-a` must not name-match the stale worktree and Navigate —
    // feat-a is no longer checked out anywhere, so it resolves to a checkout
    // in its stack home (rule 2 via the cwd) rather than Navigate.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feat-a")
        .branch("feat-b")
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .branch_metadata("feat-b", "feat-a")
        .build()?;

    let repo = fixture.repo()?;
    let wt = workon::find_worktree(repo, "feat-a")?;
    let wt_repo = git2::Repository::open(wt.path())?;
    assert_eq!(
        workon::checkout_branch_in_worktree(&wt_repo, "feat-b")?,
        workon::CheckoutOutcome::Clean
    );

    let worktree_path = fixture.root()?.path().join("feat-a");
    let saved_cwd = std::env::current_dir()?;
    std::env::set_current_dir(&worktree_path)?;

    let result = (|| -> Result<Resolution, Box<dyn Error>> {
        let repo = fixture.repo()?;
        Ok(resolve_action(repo, "feat-a", StackModel::Graphite))
    })();

    std::env::set_current_dir(saved_cwd)?;

    assert_eq!(
        result?,
        Resolution::Checkout {
            host: "feat-a".to_string()
        }
    );
    Ok(())
}

// ── Rule 3: deepest non-trunk ancestor with a worktree → Checkout there ───────

#[test]
fn rule3_deepest_ancestor_worktree_is_chosen() -> Result<(), Box<dyn Error>> {
    // Stack: main → feat-a → feat-b → feat-c. Worktrees: main, feat-a (not feat-b, feat-c).
    // CWD is not inside any fixture worktree (test runner directory).
    // workon("feat-c") should pick feat-a (deepest non-trunk ancestor with a worktree).
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-a")
        .branch("feat-b")
        .branch("feat-c")
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .branch_metadata("feat-b", "feat-a")
        .branch_metadata("feat-c", "feat-b")
        .build()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "feat-c", StackModel::Graphite),
        Resolution::Checkout {
            host: "feat-a".to_string()
        }
    );
    Ok(())
}

#[test]
fn rule3_nearest_ancestor_wins_when_multiple_have_worktrees() -> Result<(), Box<dyn Error>> {
    // Stack: main → feat-a → feat-b → feat-c. Worktrees: main, feat-a, feat-b.
    // workon("feat-c") → nearest ancestor with worktree is feat-b, not feat-a.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-a")
        .worktree("feat-b")
        .branch("feat-c")
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .branch_metadata("feat-b", "feat-a")
        .branch_metadata("feat-c", "feat-b")
        .build()?;
    let repo = fixture.repo()?;
    assert_eq!(
        resolve_action(repo, "feat-c", StackModel::Graphite),
        Resolution::Checkout {
            host: "feat-b".to_string()
        }
    );
    Ok(())
}

#[test]
fn rule3_does_not_use_trunk_as_host() -> Result<(), Box<dyn Error>> {
    // Stack: main → feat-a. Only worktrees: main (no feat-a worktree).
    // Rule 3 walks parents: feat-a → main (trunk) → breaks without producing a host.
    // Falls through to rule 4: branch exists → Materialize.
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
