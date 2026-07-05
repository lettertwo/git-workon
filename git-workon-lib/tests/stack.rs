use git_workon_fixture::prelude::*;
use std::error::Error;
use workon::{current_stack, enumerate_stacks, graphite_trunk, StackModel};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Build a fixture with Graphite metadata for a linear chain:
/// main → step-1 → step-2 → step-3
fn linear_chain() -> Result<Fixture, Box<dyn Error>> {
    FixtureBuilder::new()
        .graphite_config(&["main"])
        .branch_metadata("step-1", "main")
        .branch_metadata("step-2", "step-1")
        .branch_metadata("step-3", "step-2")
        .build()
}

// ── graphite_trunk ────────────────────────────────────────────────────────────

#[test]
fn graphite_trunk_returns_none_when_config_missing() -> Result<(), Box<dyn Error>> {
    // No .graphite_repo_config → no hardcoded "main" fallback, just None.
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;
    assert_eq!(graphite_trunk(repo), None);
    Ok(())
}

#[test]
fn graphite_trunk_returns_trunk_from_config() -> Result<(), Box<dyn Error>> {
    // The core bug: repo with trunk=develop, not main.
    let fixture = FixtureBuilder::new()
        .graphite_config(&["develop"])
        .build()?;
    let repo = fixture.repo()?;
    assert_eq!(graphite_trunk(repo), Some("develop".to_string()));
    Ok(())
}

#[test]
fn graphite_trunk_returns_first_trunk_when_multiple() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main", "release"])
        .build()?;
    let repo = fixture.repo()?;
    assert_eq!(graphite_trunk(repo), Some("main".to_string()));
    Ok(())
}

// ── read_trunks (tested indirectly via current_stack) ────────────────────────

#[test]
fn read_trunks_defaults_to_main_when_file_missing() -> Result<(), Box<dyn Error>> {
    // No .graphite_repo_config; the trunk fallback is "main".
    let fixture = FixtureBuilder::new()
        .branch_metadata("feat-a", "main")
        .build()?;
    let repo = fixture.repo()?;

    let stack = current_stack(repo, "feat-a", StackModel::Graphite)?.unwrap();
    assert_eq!(stack.trunk, "main");
    Ok(())
}

#[test]
fn read_trunks_parses_explicit_trunks() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .graphite_config(&["develop"])
        .branch_metadata("feat-a", "develop")
        .build()?;
    let repo = fixture.repo()?;

    let stack = current_stack(repo, "feat-a", StackModel::Graphite)?.unwrap();
    assert_eq!(stack.trunk, "develop");
    Ok(())
}

#[test]
fn read_trunks_parses_multiple_trunks() -> Result<(), Box<dyn Error>> {
    // "release" is also a trunk; feat-a's parent chain leads to it.
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main", "release"])
        .branch_metadata("hotfix-1", "release")
        .build()?;
    let repo = fixture.repo()?;

    let stack = current_stack(repo, "hotfix-1", StackModel::Graphite)?.unwrap();
    assert_eq!(stack.trunk, "release");
    Ok(())
}

// ── current_stack — model dispatch ───────────────────────────────────────────

#[test]
fn current_stack_returns_none_when_model_is_none() -> Result<(), Box<dyn Error>> {
    let fixture = linear_chain()?;
    let repo = fixture.repo()?;

    // StackModel::None always short-circuits to None regardless of metadata.
    assert!(current_stack(repo, "step-3", StackModel::None)?.is_none());
    Ok(())
}

// ── current_stack — Graphite path ────────────────────────────────────────────

#[test]
fn current_stack_linear_chain_returns_bfs_branches_and_trunk() -> Result<(), Box<dyn Error>> {
    let fixture = linear_chain()?;
    let repo = fixture.repo()?;

    let stack = current_stack(repo, "step-3", StackModel::Graphite)?.unwrap();
    assert_eq!(stack.trunk, "main");
    assert_eq!(stack.current, "step-3");
    // BFS from bottom: step-1, step-2, step-3
    assert_eq!(stack.diffs, vec!["step-1", "step-2", "step-3"]);
    Ok(())
}

#[test]
fn current_stack_returns_full_stack_from_any_member() -> Result<(), Box<dyn Error>> {
    let fixture = linear_chain()?;
    let repo = fixture.repo()?;

    // From step-1, all three branches are still visible.
    let stack = current_stack(repo, "step-1", StackModel::Graphite)?.unwrap();
    assert_eq!(stack.diffs, vec!["step-1", "step-2", "step-3"]);
    assert_eq!(stack.current, "step-1");
    Ok(())
}

#[test]
fn current_stack_unknown_branch_returns_none() -> Result<(), Box<dyn Error>> {
    let fixture = linear_chain()?;
    let repo = fixture.repo()?;

    // "untracked" has no refs/branch-metadata entry → None.
    assert!(current_stack(repo, "untracked", StackModel::Graphite)?.is_none());
    Ok(())
}

#[test]
fn current_stack_missing_metadata_refs_returns_none() -> Result<(), Box<dyn Error>> {
    // No branch-metadata refs at all.
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    assert!(current_stack(repo, "main", StackModel::Graphite)?.is_none());
    Ok(())
}

#[test]
fn current_stack_head_is_trunk_returns_none() -> Result<(), Box<dyn Error>> {
    let fixture = linear_chain()?;
    let repo = fixture.repo()?;

    // "main" is trunk; the code returns None because head_branch is trunk itself.
    assert!(current_stack(repo, "main", StackModel::Graphite)?.is_none());
    Ok(())
}

#[test]
fn current_stack_diamond_bfs_includes_all_branches() -> Result<(), Box<dyn Error>> {
    // main → A; A → B; A → C (fork)
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .branch_metadata("feat-b", "feat-a")
        .branch_metadata("feat-c", "feat-a")
        .build()?;
    let repo = fixture.repo()?;

    let stack_from_b = current_stack(repo, "feat-b", StackModel::Graphite)?.unwrap();
    assert_eq!(stack_from_b.trunk, "main");
    // feat-a, then both children (order within the same BFS level is not specified).
    assert!(stack_from_b.diffs.contains(&"feat-a".to_string()));
    assert!(stack_from_b.diffs.contains(&"feat-b".to_string()));
    assert!(stack_from_b.diffs.contains(&"feat-c".to_string()));
    assert_eq!(stack_from_b.diffs.len(), 3);
    // feat-a must come before its children.
    let idx_a = stack_from_b
        .diffs
        .iter()
        .position(|b| b == "feat-a")
        .unwrap();
    let idx_b = stack_from_b
        .diffs
        .iter()
        .position(|b| b == "feat-b")
        .unwrap();
    let idx_c = stack_from_b
        .diffs
        .iter()
        .position(|b| b == "feat-c")
        .unwrap();
    assert!(idx_a < idx_b && idx_a < idx_c);
    Ok(())
}

#[test]
fn current_stack_malformed_blob_branch_is_excluded() -> Result<(), Box<dyn Error>> {
    // feat-a has valid metadata; feat-b has an invalid JSON blob.
    // When querying from feat-a: feat-b's metadata is silently skipped,
    // so feat-b does not appear in the stack.
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .raw_branch_metadata("feat-b", b"not json at all".to_vec())
        .build()?;
    let repo = fixture.repo()?;

    let stack = current_stack(repo, "feat-a", StackModel::Graphite)?.unwrap();
    assert_eq!(stack.diffs, vec!["feat-a"]);
    assert!(!stack.diffs.contains(&"feat-b".to_string()));
    Ok(())
}

#[test]
fn current_stack_malformed_blob_for_head_returns_none() -> Result<(), Box<dyn Error>> {
    // The queried branch itself has a malformed blob → not in parent map → None.
    let fixture = FixtureBuilder::new()
        .raw_branch_metadata("feat-a", b"not json".to_vec())
        .build()?;
    let repo = fixture.repo()?;

    assert!(current_stack(repo, "feat-a", StackModel::Graphite)?.is_none());
    Ok(())
}

// ── Stack.parents — tree structure ────────────────────────────────────────────

#[test]
fn current_stack_linear_chain_parents_map_is_populated() -> Result<(), Box<dyn Error>> {
    // main → step-1 → step-2 → step-3
    let fixture = linear_chain()?;
    let repo = fixture.repo()?;

    let stack = current_stack(repo, "step-3", StackModel::Graphite)?.unwrap();
    // Each diff maps to its immediate parent.
    assert_eq!(
        stack.parents.get("step-1").map(String::as_str),
        Some("main"),
        "step-1's parent must be main"
    );
    assert_eq!(
        stack.parents.get("step-2").map(String::as_str),
        Some("step-1"),
        "step-2's parent must be step-1"
    );
    assert_eq!(
        stack.parents.get("step-3").map(String::as_str),
        Some("step-2"),
        "step-3's parent must be step-2"
    );
    // Trunk itself is not a key in parents.
    assert!(
        !stack.parents.contains_key("main"),
        "trunk must not be a key in parents"
    );
    Ok(())
}

#[test]
fn current_stack_branching_stack_parents_map_covers_all_diffs() -> Result<(), Box<dyn Error>> {
    // main → feat-a → feat-b (linear), and also main → feat-a → feat-c (fork at feat-a)
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .branch_metadata("feat-b", "feat-a")
        .branch_metadata("feat-c", "feat-a")
        .build()?;
    let repo = fixture.repo()?;

    let stack = current_stack(repo, "feat-b", StackModel::Graphite)?.unwrap();

    assert_eq!(
        stack.parents.get("feat-a").map(String::as_str),
        Some("main")
    );
    assert_eq!(
        stack.parents.get("feat-b").map(String::as_str),
        Some("feat-a")
    );
    assert_eq!(
        stack.parents.get("feat-c").map(String::as_str),
        Some("feat-a")
    );
    // All three diffs should have entries.
    assert_eq!(stack.parents.len(), 3);
    Ok(())
}

#[test]
fn enumerate_stacks_parents_map_is_populated() -> Result<(), Box<dyn Error>> {
    // Verify that the parents map is also populated via enumerate_stacks (metadata-only path).
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main"])
        .branch_metadata("api-1", "main")
        .branch_metadata("api-2", "api-1")
        .build()?;
    let repo = fixture.repo()?;

    let stacks = enumerate_stacks(repo, StackModel::Graphite)?;
    assert_eq!(stacks.len(), 1);
    let stack = &stacks[0];
    assert_eq!(stack.parents.get("api-1").map(String::as_str), Some("main"));
    assert_eq!(
        stack.parents.get("api-2").map(String::as_str),
        Some("api-1")
    );
    Ok(())
}

#[test]
fn current_stack_cycle_in_metadata_terminates() -> Result<(), Box<dyn Error>> {
    // A → B → A: the upward walk from B should not loop.
    // B's parent is A, A's parent is B — there's no trunk in the chain,
    // so the walk terminates when it reaches a branch with no parent entry
    // or detects it has walked past every known branch. The result may be
    // None or a partial stack, but must not hang.
    let fixture = FixtureBuilder::new()
        .branch_metadata("feat-a", "feat-b")
        .branch_metadata("feat-b", "feat-a")
        .build()?;
    let repo = fixture.repo()?;

    // Must complete without hanging (the visited set in BFS prevents infinite looping).
    // The upward walk terminates when it hits a branch not in the parent map
    // (feat-b's parent is feat-a, feat-a's parent is feat-b, ... eventually
    // it wraps — the walk exits when the branch re-appears or the map is exhausted).
    // We just verify it returns without panicking.
    let _ = current_stack(repo, "feat-b", StackModel::Graphite)?;
    Ok(())
}

// ── worktree-opened repo (commondir vs gitdir regression) ────────────────────

#[test]
fn current_stack_resolves_metadata_when_repo_is_worktree_opened() -> Result<(), Box<dyn Error>> {
    // Graphite writes `.graphite_repo_config` / branch-metadata to the git *common* dir.
    // When `repo` is opened at a linked worktree, `repo.path()` is the worktree's private
    // gitdir (`<common>/worktrees/<name>/`), not the common dir — only `repo.commondir()`
    // resolves there. This is a regression test for that bug: the fixture opens `step-2`
    // as a linked worktree, so metadata reads must go through `commondir()`.
    //
    // The trunk is deliberately named "develop" (not "main"): `read_trunks`'s hardcoded
    // `"main"` fallback would otherwise mask the bug (a missing/unreadable config file
    // falls back to `vec!["main"]`, which happens to be the right answer when the real
    // trunk actually is "main"). `graphite_trunk` is the assertion that fails pre-fix;
    // `current_stack` is independently robust to this bug (its upward walk terminates on
    // a missing parent-map entry for the trunk branch either way), so its assertions are
    // a behavior-completeness check, not the discriminating ones.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .graphite_config(&["develop"])
        .worktree("step-2")
        .branch_metadata("step-1", "develop")
        .branch_metadata("step-2", "step-1")
        .build()?;
    let repo = fixture.repo()?;

    assert_eq!(graphite_trunk(repo), Some("develop".to_string()));

    let stack = current_stack(repo, "step-2", StackModel::Graphite)?.unwrap();
    assert_eq!(stack.trunk, "develop");
    assert_eq!(stack.diffs, vec!["step-1", "step-2"]);
    Ok(())
}

// ── fixture predicates ────────────────────────────────────────────────────────

#[test]
fn has_branch_metadata_predicate_passes_for_correct_parent() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .branch_metadata("feat-a", "main")
        .build()?;

    fixture.assert(predicate::repo::has_branch_metadata("feat-a", "main"));
    Ok(())
}

#[test]
fn has_graphite_config_predicate_passes_for_matching_trunks() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main", "release"])
        .build()?;

    fixture.assert(predicate::repo::has_graphite_config(&["main", "release"]));
    Ok(())
}
