use git_workon_fixture::prelude::*;
use std::error::Error;
use workon::{current_stack, graphite_trunk, StackModel};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Build a fixture with Graphite metadata for a linear chain:
/// main → step-1 → step-2 → step-3
fn linear_chain() -> Result<Fixture, Box<dyn Error>> {
    Ok(FixtureBuilder::new()
        .graphite_config(&["main"])
        .branch_metadata("step-1", "main")
        .branch_metadata("step-2", "step-1")
        .branch_metadata("step-3", "step-2")
        .build()?)
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
    assert_eq!(stack.branches, vec!["step-1", "step-2", "step-3"]);
    Ok(())
}

#[test]
fn current_stack_returns_full_stack_from_any_member() -> Result<(), Box<dyn Error>> {
    let fixture = linear_chain()?;
    let repo = fixture.repo()?;

    // From step-1, all three branches are still visible.
    let stack = current_stack(repo, "step-1", StackModel::Graphite)?.unwrap();
    assert_eq!(stack.branches, vec!["step-1", "step-2", "step-3"]);
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
    assert!(stack_from_b.branches.contains(&"feat-a".to_string()));
    assert!(stack_from_b.branches.contains(&"feat-b".to_string()));
    assert!(stack_from_b.branches.contains(&"feat-c".to_string()));
    assert_eq!(stack_from_b.branches.len(), 3);
    // feat-a must come before its children.
    let idx_a = stack_from_b
        .branches
        .iter()
        .position(|b| b == "feat-a")
        .unwrap();
    let idx_b = stack_from_b
        .branches
        .iter()
        .position(|b| b == "feat-b")
        .unwrap();
    let idx_c = stack_from_b
        .branches
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
    assert_eq!(stack.branches, vec!["feat-a"]);
    assert!(!stack.branches.contains(&"feat-b".to_string()));
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
