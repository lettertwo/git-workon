use git_workon_fixture::prelude::*;
use std::error::Error;
use workon::{
    assemble_changesets, ChangesetError, ChangesetSpan, StackError, StackModel, WorkonError,
};

// ── both-format parameterization (see tests/stack.rs) ────────────────────────

macro_rules! both_formats {
    ($($name:ident),+ $(,)?) => {$(
        mod $name {
            use super::*;
            #[test] fn refs()   { super::$name(MetadataFormat::Refs).unwrap() }
            #[test] fn sqlite() { super::$name(MetadataFormat::Sqlite).unwrap() }
        }
    )+};
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn linear_chain(format: MetadataFormat) -> Result<Fixture, Box<dyn Error>> {
    FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .branch_metadata("c", "b")
        .build()
}

fn branch_tip(fixture: &Fixture, branch: &str) -> Result<git2::Oid, Box<dyn Error>> {
    Ok(fixture
        .repo()?
        .find_branch(branch, git2::BranchType::Local)?
        .get()
        .target()
        .unwrap())
}

// ── Graphite assembly ─────────────────────────────────────────────────────────

fn graphite_linear_order_current_and_titles(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .branch_metadata("c", "b")
        .graphite_pr_info("a", 1, "Add a")
        .build()?;
    let a_tip = branch_tip(&fixture, "a")?;
    let b_tip = branch_tip(&fixture, "b")?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "b", StackModel::Graphite)?;
    let names: Vec<&str> = changesets.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);

    let current: Vec<&str> = changesets
        .iter()
        .filter(|c| c.current)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(current, vec!["b"]);

    let b_cs = changesets.iter().find(|c| c.name == "b").unwrap();
    match b_cs.span {
        ChangesetSpan::Committed { base, head } => {
            assert_eq!(base, a_tip, "b's base must be a's recorded parent tip");
            assert_eq!(head, b_tip, "b's head must be its live tip");
        }
        _ => panic!("expected Committed for 'b'"),
    }

    assert_eq!(
        changesets
            .iter()
            .find(|c| c.name == "a")
            .unwrap()
            .title
            .as_deref(),
        Some("Add a")
    );
    assert_eq!(
        changesets.iter().find(|c| c.name == "b").unwrap().title,
        None,
        "no pr_info entry for 'b'"
    );
    Ok(())
}
both_formats!(graphite_linear_order_current_and_titles);

fn graphite_fork_siblings_sorted_lexically(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("zeta", "a")
        .branch_metadata("alpha", "a")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "a", StackModel::Graphite)?;
    let names: Vec<&str> = changesets.iter().map(|c| c.name.as_str()).collect();
    // Descendant DFS sorts siblings lexically, not by creation order (zeta was added first).
    assert_eq!(names, vec!["a", "alpha", "zeta"]);
    Ok(())
}
both_formats!(graphite_fork_siblings_sorted_lexically);

fn graphite_all_at_one_commit_base_equals_head(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "a", StackModel::Graphite)?;
    assert_eq!(changesets.len(), 1);
    match changesets[0].span {
        ChangesetSpan::Committed { base, head } => {
            assert_eq!(base, head, "no divergence yet: base must equal head")
        }
        _ => panic!("expected Committed"),
    }
    Ok(())
}
both_formats!(graphite_all_at_one_commit_base_equals_head);

fn graphite_ghost_mid_stack_skipped_children_present(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .ghost_branch_metadata("ghost", "main")
        .branch_metadata("child", "ghost")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "child", StackModel::Graphite)?;
    let names: Vec<&str> = changesets.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["child"], "ghost must not appear in output");
    assert!(changesets[0].current);
    Ok(())
}
both_formats!(graphite_ghost_mid_stack_skipped_children_present);

fn graphite_untracked_parent_excluded_from_walk(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    // "outside" is a live branch with NO metadata row; "feat" records it as parent
    // (with a resolvable revision, so base resolution doesn't depend on the walk).
    // The ancestors walk must stop at the untracked parent without emitting it.
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch("outside")
        .branch_metadata("feat", "outside")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "feat", StackModel::Graphite)?;
    let names: Vec<&str> = changesets.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["feat"], "untracked parent must not be emitted");
    Ok(())
}
both_formats!(graphite_untracked_parent_excluded_from_walk);

fn graphite_current_branch_missing_ref_errors(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .ghost_branch_metadata("c", "main")
        .build()?;
    let repo = fixture.repo()?;

    let err = assemble_changesets(repo, "c", StackModel::Graphite).unwrap_err();
    match err {
        WorkonError::Changeset(ChangesetError::UnresolvableBranch { branch }) => {
            assert_eq!(branch, "c")
        }
        other => panic!("expected UnresolvableBranch, got {other:?}"),
    }
    Ok(())
}
both_formats!(graphite_current_branch_missing_ref_errors);

// ── Trap 7 ─────────────────────────────────────────────────────────────────────

fn trap7_spans_stale_branch_revision_to_live_head(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .worktree("feat-a")
        .branch_metadata("feat-a", "main")
        .build()?;
    let main_tip = branch_tip(&fixture, "main")?;

    fixture
        .commit("feat-a")
        .file("f1.txt", "1")
        .create("commit1")?;
    let live_tip = fixture
        .commit("feat-a")
        .file("f2.txt", "2")
        .create("commit2")?;

    let repo = fixture.repo()?;
    let changesets = assemble_changesets(repo, "feat-a", StackModel::Graphite)?;
    assert_eq!(changesets.len(), 1);
    match changesets[0].span {
        ChangesetSpan::Committed { base, head } => {
            assert_eq!(
                base, main_tip,
                "base must be the recorded parentBranchRevision"
            );
            assert_eq!(
                head, live_tip,
                "head must be the live tip, not the stale branch_revision"
            );
        }
        _ => panic!("expected Committed"),
    }
    Ok(())
}
both_formats!(trap7_spans_stale_branch_revision_to_live_head);

fn trap7_bogus_parent_revision_errors(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let bogus = "deadbeef".repeat(5);
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata_at("feat-a", "main", &bogus, &bogus)
        .build()?;
    let repo = fixture.repo()?;

    let err = assemble_changesets(repo, "feat-a", StackModel::Graphite).unwrap_err();
    match err {
        WorkonError::Changeset(ChangesetError::InvalidParentRevision { branch, revision }) => {
            assert_eq!(branch, "feat-a");
            assert_eq!(revision, bogus);
        }
        other => panic!("expected InvalidParentRevision (not an empty Ok), got {other:?}"),
    }
    Ok(())
}
both_formats!(trap7_bogus_parent_revision_errors);

#[test]
fn trap7_corrupt_sqlite_db_errors() -> Result<(), Box<dyn Error>> {
    // Refs-format fixture with valid metadata, then garbage bytes at the sqlite db path —
    // proves the error isn't masked by a silent fallback to (valid!) refs metadata.
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main"])
        .branch_metadata("feat-a", "main")
        .build()?;
    let repo = fixture.repo()?;
    let db_path = repo.commondir().join(".graphite_metadata.db");
    std::fs::write(&db_path, b"not a sqlite database")?;

    let err = assemble_changesets(repo, "feat-a", StackModel::Graphite).unwrap_err();
    assert!(
        matches!(err, WorkonError::Stack(StackError::GtParseFailed { .. })),
        "expected GtParseFailed, got {err:?}"
    );
    Ok(())
}

// ── needs-restack ──────────────────────────────────────────────────────────────

fn needs_restack_true_when_parent_advances_post_build(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .worktree("parent-branch")
        .branch_metadata("parent-branch", "main")
        .branch_metadata("child", "parent-branch")
        .build()?;

    fixture
        .commit("parent-branch")
        .file("z.txt", "1")
        .create("advance parent")?;

    let repo = fixture.repo()?;
    let changesets = assemble_changesets(repo, "child", StackModel::Graphite)?;

    let child_cs = changesets.iter().find(|c| c.name == "child").unwrap();
    assert!(
        child_cs.needs_restack,
        "child's recorded parent revision is now stale"
    );
    let parent_cs = changesets
        .iter()
        .find(|c| c.name == "parent-branch")
        .unwrap();
    assert!(
        !parent_cs.needs_restack,
        "parent-branch's own recorded parent (main) hasn't moved"
    );
    Ok(())
}
both_formats!(needs_restack_true_when_parent_advances_post_build);

fn needs_restack_false_for_untouched_stack(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = linear_chain(format)?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "c", StackModel::Graphite)?;
    assert!(
        changesets.iter().all(|c| !c.needs_restack),
        "no branch advanced past what metadata recorded"
    );
    Ok(())
}
both_formats!(needs_restack_false_for_untouched_stack);

fn needs_restack_false_with_empty_parent_revision_and_merge_base_fallback(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata_at("a", "main", "", "")
        .build()?;
    let main_tip = branch_tip(&fixture, "main")?;
    let a_tip = branch_tip(&fixture, "a")?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "a", StackModel::Graphite)?;
    assert_eq!(changesets.len(), 1);
    assert!(!changesets[0].needs_restack);
    match changesets[0].span {
        ChangesetSpan::Committed { base, head } => {
            assert_eq!(head, a_tip);
            assert_eq!(base, main_tip, "merge-base fallback resolves to main's tip");
        }
        _ => panic!("expected Committed"),
    }
    Ok(())
}
both_formats!(needs_restack_false_with_empty_parent_revision_and_merge_base_fallback);

fn needs_restack_computed_for_ancestors_of_current(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .branch_metadata("c", "b")
        .build()?;

    // Advance main (trunk) past what 'a' recorded as its parent revision — 'a' is an
    // ancestor of the current branch 'b', not the current node itself.
    fixture
        .commit("main")
        .file("m.txt", "1")
        .create("advance main")?;

    let repo = fixture.repo()?;
    let changesets = assemble_changesets(repo, "b", StackModel::Graphite)?;

    let a_cs = changesets.iter().find(|c| c.name == "a").unwrap();
    assert!(
        a_cs.needs_restack,
        "ancestor 'a' of current 'b' must be flagged, not just descendants of current"
    );
    let b_cs = changesets.iter().find(|c| c.name == "b").unwrap();
    assert!(
        !b_cs.needs_restack,
        "b's recorded parent revision (a's tip) hasn't changed"
    );
    Ok(())
}
both_formats!(needs_restack_computed_for_ancestors_of_current);

// ── Uncommitted layer (Graphite) ────────────────────────────────────────────────

fn uncommitted_layer_staged(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .worktree("a")
        .branch_metadata("a", "main")
        .staged_file("new.txt", "content")
        .build()?;
    assert_uncommitted_inserted_after_current(&fixture, "a")
}
both_formats!(uncommitted_layer_staged);

fn uncommitted_layer_unstaged(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .worktree("a")
        .branch_metadata("a", "main")
        .unstaged_file("tracked.txt", "committed", "modified")
        .build()?;
    assert_uncommitted_inserted_after_current(&fixture, "a")
}
both_formats!(uncommitted_layer_unstaged);

fn uncommitted_layer_untracked(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .worktree("a")
        .branch_metadata("a", "main")
        .untracked_file("scratch.txt", "hi")
        .build()?;
    assert_uncommitted_inserted_after_current(&fixture, "a")
}
both_formats!(uncommitted_layer_untracked);

fn uncommitted_layer_combined(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .worktree("a")
        .branch_metadata("a", "main")
        .staged_file("staged.txt", "1")
        .unstaged_file("tracked.txt", "committed", "modified")
        .untracked_file("scratch.txt", "hi")
        .build()?;
    assert_uncommitted_inserted_after_current(&fixture, "a")
}
both_formats!(uncommitted_layer_combined);

fn uncommitted_layer_absent_on_clean_tree(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .worktree("a")
        .branch_metadata("a", "main")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "a", StackModel::Graphite)?;
    assert_eq!(changesets.len(), 1);
    assert!(changesets[0].current);
    assert_ne!(changesets[0].span, ChangesetSpan::Uncommitted);
    Ok(())
}
both_formats!(uncommitted_layer_absent_on_clean_tree);

fn assert_uncommitted_inserted_after_current(
    fixture: &Fixture,
    current_branch: &str,
) -> Result<(), Box<dyn Error>> {
    let repo = fixture.repo()?;
    let changesets = assemble_changesets(repo, current_branch, StackModel::Graphite)?;
    assert_eq!(changesets.len(), 2);
    assert_eq!(changesets[0].name, current_branch);
    assert!(!changesets[0].current, "branch node must drop current");
    assert_eq!(changesets[1].span, ChangesetSpan::Uncommitted);
    assert_eq!(changesets[1].name, current_branch);
    assert!(changesets[1].current, "Uncommitted takes current");
    Ok(())
}

// ── Graphite → Git fallback ────────────────────────────────────────────────────

fn graphite_falls_back_to_git_on_trunk(format: MetadataFormat) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .build()?;
    fixture
        .commit("main")
        .file("x.txt", "1")
        .create("only commit")?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "main", StackModel::Graphite)?;
    assert_eq!(changesets.len(), 1);
    assert_eq!(changesets[0].title.as_deref(), Some("only commit"));
    Ok(())
}
both_formats!(graphite_falls_back_to_git_on_trunk);

fn graphite_falls_back_to_git_on_untracked_branch(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .worktree("feat-a")
        .remote("origin", "https://example.com/origin.git")
        .upstream("feat-a", "origin/feat-a")
        .build()?;
    fixture
        .commit("feat-a")
        .file("y.txt", "1")
        .create("untracked commit")?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "feat-a", StackModel::Graphite)?;
    assert_eq!(changesets.len(), 1);
    assert_eq!(changesets[0].title.as_deref(), Some("untracked commit"));
    Ok(())
}
both_formats!(graphite_falls_back_to_git_on_untracked_branch);

// ── Git inference ────────────────────────────────────────────────────────────────

#[test]
fn git_inference_two_commits_oldest_first() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .build()?;
    let first = fixture.commit("main").file("a.txt", "1").create("first")?;

    fixture.commit("main").file("b.txt", "2").create("second")?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "main", StackModel::Git)?;
    assert_eq!(changesets.len(), 2);
    assert_eq!(changesets[0].title.as_deref(), Some("first"));
    assert_eq!(changesets[1].title.as_deref(), Some("second"));
    assert!(!changesets[0].current);
    assert!(changesets[1].current);
    assert!(!changesets[0].needs_restack && !changesets[1].needs_restack);
    assert_eq!(
        changesets[0].name.len(),
        8,
        "name is an 8-hex abbreviated id"
    );

    match (&changesets[0].span, &changesets[1].span) {
        (ChangesetSpan::Committed { head: h0, .. }, ChangesetSpan::Committed { base: b1, .. }) => {
            assert_eq!(*h0, first, "first commit's head is its own oid");
            assert_eq!(*b1, *h0, "second's base is first's head");
        }
        _ => panic!("expected Committed sources"),
    }
    Ok(())
}

#[test]
fn git_inference_dirty_tree_appends_uncommitted_as_current() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .untracked_file("scratch.txt", "hi")
        .build()?;
    fixture.commit("main").file("a.txt", "1").create("first")?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "main", StackModel::Git)?;
    assert_eq!(changesets.len(), 2);
    assert!(!changesets[0].current);
    assert_eq!(changesets[1].span, ChangesetSpan::Uncommitted);
    assert!(changesets[1].current);
    Ok(())
}

#[test]
fn git_inference_caught_up_and_clean_returns_empty() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "main", StackModel::Git)?;
    assert_eq!(changesets, vec![]);
    Ok(())
}

#[test]
fn git_inference_no_upstream_errors() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    let err = assemble_changesets(repo, "main", StackModel::Git).unwrap_err();
    match err {
        WorkonError::Changeset(ChangesetError::NoUpstream { branch }) => {
            assert_eq!(branch, "main")
        }
        other => panic!("expected NoUpstream, got {other:?}"),
    }
    Ok(())
}

// ── StackModel::None ─────────────────────────────────────────────────────────────

#[test]
fn none_model_always_returns_empty() -> Result<(), Box<dyn Error>> {
    let fixture = linear_chain(MetadataFormat::Refs)?;
    let repo = fixture.repo()?;

    assert_eq!(assemble_changesets(repo, "c", StackModel::None)?, vec![]);
    Ok(())
}

// ── StackModel::GhStack ──────────────────────────────────────────────────────────
// Thin wiring check: GhStack shares assemble_from_metadata with Graphite (see stack/gh_stack.rs
// for the provider-specific parsing tests), so this only proves changeset.rs's match arm calls
// gh_stack::read_metadata and gets a properly ordered walk out of it.

#[test]
fn gh_stack_linear_order_and_current() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .gh_stack(None, 1, "main", &["a", "b", "c"])
        .build()?;
    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "b", StackModel::GhStack)?;
    let names: Vec<&str> = changesets.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);

    let current: Vec<&str> = changesets
        .iter()
        .filter(|c| c.current)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(current, vec!["b"]);
    Ok(())
}

#[test]
fn gh_stack_trap7_bogus_parent_revision_errors() -> Result<(), Box<dyn Error>> {
    let bogus = "deadbeef".repeat(5);
    let fixture = FixtureBuilder::new()
        .gh_stack_at(None, 1, "main", &[("feat-a", &bogus)])
        .build()?;
    let repo = fixture.repo()?;

    let err = assemble_changesets(repo, "feat-a", StackModel::GhStack).unwrap_err();
    match err {
        WorkonError::Changeset(ChangesetError::InvalidParentRevision { branch, revision }) => {
            assert_eq!(branch, "feat-a");
            assert_eq!(revision, bogus);
        }
        other => panic!("expected InvalidParentRevision (not an empty Ok), got {other:?}"),
    }
    Ok(())
}

#[test]
fn gh_stack_needs_restack_true_when_base_differs_from_parent_live_tip() -> Result<(), Box<dyn Error>>
{
    let fixture = FixtureBuilder::new()
        .worktree("feat-a")
        .gh_stack(None, 1, "main", &["feat-a", "feat-b"])
        .build()?;

    fixture
        .commit("feat-a")
        .file("z.txt", "1")
        .create("advance feat-a")?;

    let repo = fixture.repo()?;

    let changesets = assemble_changesets(repo, "feat-b", StackModel::GhStack)?;
    let feat_a = changesets.iter().find(|c| c.name == "feat-a").unwrap();
    assert!(
        !feat_a.needs_restack,
        "feat-a's own recorded parent (main) hasn't moved"
    );
    let feat_b = changesets.iter().find(|c| c.name == "feat-b").unwrap();
    assert!(
        feat_b.needs_restack,
        "feat-b's recorded base no longer matches feat-a's live tip"
    );
    Ok(())
}
