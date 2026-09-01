//! Fixture tests for the source-selector work's `Source` classifier + resolver (ADR-036): the
//! `stack`/`uncommitted` keywords, and `<ref>` shape-aware dispatch + `Range` resolution. Output
//! assertions pin `NO_COLOR=1` per the FORCE_COLOR trap this dev environment sets.

use assert_cmd::cargo_bin_cmd;
use git2::ObjectType;
use git_workon_fixture::prelude::*;
use std::error::Error;
use workon::ChangesetSpan;
use workon_review::acquire::{diff_changeset, resolve_changesets, ChangesetDiff};
use workon_review::error::SourceError;
use workon_review::source::{resolve_source, Source};

macro_rules! both_formats {
    ($($name:ident),+ $(,)?) => {$(
        mod $name {
            use super::*;
            #[test] fn refs()   { super::$name(MetadataFormat::Refs).unwrap() }
            #[test] fn sqlite() { super::$name(MetadataFormat::Sqlite).unwrap() }
        }
    )+};
}

both_formats!(
    stack_keyword_in_graphite_repo_returns_full_stack,
    uncommitted_keyword_in_graphite_repo_returns_single_uncommitted_changeset,
);

fn stack_keyword_in_graphite_repo_returns_full_stack(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .branch_metadata("c", "b")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = resolve_source(repo, "b", Source::classify("stack"))?;
    let names: Vec<&str> = changesets.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);

    let current: Vec<&str> = changesets
        .iter()
        .filter(|c| c.current)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(current, vec!["b"], "exactly the focused branch is current");
    Ok(())
}

fn uncommitted_keyword_in_graphite_repo_returns_single_uncommitted_changeset(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = resolve_source(repo, "b", Source::classify("uncommitted"))?;
    assert_eq!(changesets.len(), 1, "always exactly one changeset");
    assert_eq!(changesets[0].span, ChangesetSpan::Uncommitted);
    assert!(changesets[0].current);
    assert_eq!(
        changesets[0].name, "b",
        "the uncommitted changeset is named after the focused branch, not the stack"
    );
    Ok(())
}

#[test]
fn stack_keyword_in_plain_git_repo_with_upstream_returns_per_commit_changesets(
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .build()?;
    fixture.commit("main").file("a.txt", "1").create("first")?;
    fixture.commit("main").file("b.txt", "2").create("second")?;
    let repo = fixture.repo()?;

    let changesets = resolve_source(repo, "main", Source::classify("stack"))?;
    assert_eq!(changesets.len(), 2, "one changeset per commit");
    assert_eq!(changesets[0].title.as_deref(), Some("first"));
    assert_eq!(changesets[1].title.as_deref(), Some("second"));
    assert!(changesets[1].current);
    Ok(())
}

#[test]
fn stack_keyword_with_no_upstream_errors() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    let err = resolve_source(repo, "main", Source::classify("stack")).unwrap_err();
    match err {
        SourceError::NoUpstream { branch } => assert_eq!(branch, "main"),
        other => panic!("expected NoUpstream, got {other:?}"),
    }
    Ok(())
}

/// `stack` on a branch that's caught up with its upstream and has a clean tree resolves to
/// zero changesets (`assemble_git`'s empty-vec arm, `git_inference_caught_up_and_clean_returns_empty`
/// in `git-workon-lib/tests/changeset.rs`) — end-to-end through the binary this must print
/// "nothing to review" and exit 0, exactly like the no-argument auto-detect path, not panic.
#[test]
fn stack_keyword_caught_up_and_clean_prints_nothing_to_review() {
    let fixture = FixtureBuilder::new()
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .build()
        .unwrap();
    let repo = fixture.repo().unwrap();
    let workdir = repo.workdir().unwrap();

    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.current_dir(workdir)
        .env("NO_COLOR", "1")
        .arg("stack")
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing to review"));
}

/// The classifier/resolver seam the stack/uncommitted-source-keywords work introduces resolves
/// `Ref` to a named, hinted pre-TUI failure (real ref resolution is `<ref>` and range
/// resolution) — end-to-end through the binary, so this doubles as
/// the stack/uncommitted-source-keywords manual smoke check ("a source shape renders or errors
/// honestly"). Color is pinned
/// off: `FORCE_COLOR=3` is set in this dev environment and would otherwise leak ANSI codes
/// into the assertion.
#[test]
fn unresolvable_ref_source_prints_named_error_and_exits_nonzero() {
    let fixture = FixtureBuilder::new().build().unwrap();
    let repo = fixture.repo().unwrap();
    let workdir = repo.workdir().unwrap();

    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.current_dir(workdir)
        .env("NO_COLOR", "1")
        .arg("no-such-thing")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot resolve 'no-such-thing' as a review source",
        ));
}

// ── `<ref>` and range resolution: shape-aware dispatch + `Range` resolution ─────────────────

both_formats!(ref_on_graphite_tracked_branch_that_is_head_matches_auto_detect,);

/// A `<ref>` naming the Graphite-tracked branch that IS real `HEAD` must resolve identically to
/// auto-detect (ADR-036: the uncommitted layer rides along). A dirty tree (an untracked file)
/// makes the layer's presence in both outputs an actual assertion, not a vacuous one.
fn ref_on_graphite_tracked_branch_that_is_head_matches_auto_detect(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .untracked_file("dirty.txt", "wip")
        .build()?;
    let repo = fixture.repo()?;

    let auto = resolve_changesets(repo, "b")?;
    let via_ref = resolve_source(repo, "b", Source::classify("b"))?;
    assert_eq!(auto, via_ref);
    assert!(
        auto.iter().any(|cs| cs.span == ChangesetSpan::Uncommitted),
        "a dirty tree on real HEAD must carry the uncommitted layer"
    );
    Ok(())
}

/// A `<ref>` naming a Graphite-tracked branch that is NOT real `HEAD` never gets the
/// uncommitted layer, even on a dirty tree — the dirty tree belongs to whatever branch is
/// actually checked out, not the reviewed one (ADR-036).
#[test]
fn ref_on_graphite_tracked_branch_that_is_not_head_omits_uncommitted_layer_on_dirty_tree(
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .untracked_file("dirty.txt", "wip")
        .build()?;
    let repo = fixture.repo()?;

    // head_branch is deliberately NOT "b": the real HEAD is some other branch entirely.
    let changesets = resolve_source(repo, "some-other-branch", Source::classify("b"))?;
    assert!(
        !changesets
            .iter()
            .any(|cs| cs.span == ChangesetSpan::Uncommitted),
        "reviewing a non-HEAD branch must never surface the uncommitted layer"
    );
    let current: Vec<&str> = changesets
        .iter()
        .filter(|c| c.current)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(current, vec!["b"]);
    Ok(())
}

/// An untracked local branch with an upstream resolves to one committed changeset spanning
/// `merge-base(upstream, branch)..branch` — "what this branch adds".
#[test]
fn untracked_branch_with_upstream_bases_on_merge_base_with_upstream() -> Result<(), Box<dyn Error>>
{
    let fixture = FixtureBuilder::new()
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .build()?;
    // `.upstream()` pins `origin/main` to the branch's tip AT BUILD TIME (the root commit) —
    // both commits below land after that, so the upstream-anchored merge-base is the root.
    let base_oid = fixture.head()?.peel_to_commit()?.id();
    fixture.commit("main").file("a.txt", "1").create("first")?;
    let head_oid = fixture.commit("main").file("b.txt", "2").create("second")?;
    let repo = fixture.repo()?;

    let changesets = resolve_source(repo, "main", Source::classify("main"))?;
    assert_eq!(changesets.len(), 1);
    match changesets[0].span {
        ChangesetSpan::Committed { base, head } => {
            assert_eq!(base, base_oid, "base is the upstream-anchored merge-base");
            assert_eq!(head, head_oid);
        }
        other => panic!("expected Committed, got {other:?}"),
    }
    assert_eq!(changesets[0].name, "main");
    Ok(())
}

/// An untracked local branch with NO upstream falls back to `merge-base(trunk, branch)`, where
/// trunk is the repo's default branch (no Graphite trunk configured here).
#[test]
fn untracked_branch_without_upstream_bases_on_merge_base_with_trunk() -> Result<(), Box<dyn Error>>
{
    let fixture = FixtureBuilder::new()
        .default_branch("main")
        .worktree("feature")
        .build()?;
    fixture
        .commit("main")
        .file("a.txt", "1")
        .create("on main")?;
    let feature_head = fixture
        .commit("feature")
        .file("b.txt", "1")
        .create("on feature")?;
    let repo = fixture.repo()?;

    let changesets = resolve_source(repo, "feature", Source::classify("feature"))?;
    assert_eq!(changesets.len(), 1);
    match changesets[0].span {
        ChangesetSpan::Committed { base, head } => {
            let main_tip = repo
                .find_branch("main", git2::BranchType::Local)?
                .get()
                .target()
                .unwrap();
            let expected_base = repo.merge_base(main_tip, feature_head)?;
            assert_eq!(base, expected_base, "base is the trunk-anchored merge-base");
            assert_eq!(head, feature_head);
        }
        other => panic!("expected Committed, got {other:?}"),
    }
    Ok(())
}

/// An untracked branch with neither an upstream nor a resolvable trunk is a named error, not a
/// silent fallback.
#[test]
fn untracked_branch_with_no_upstream_and_no_trunk_errors() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().default_branch("solo").build()?;
    let repo = fixture.repo()?;

    let err = resolve_source(repo, "solo", Source::classify("solo")).unwrap_err();
    match err {
        SourceError::NoBaseForBranch { branch } => assert_eq!(branch, "solo"),
        other => panic!("expected NoBaseForBranch, got {other:?}"),
    }
    Ok(())
}

/// A bare commit sha resolves to one changeset spanning `parent..sha`.
#[test]
fn commit_sha_resolves_to_parent_and_sha() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let parent_oid = fixture.head()?.peel_to_commit()?.id();
    let head_oid = fixture.commit("main").file("a.txt", "1").create("first")?;
    let repo = fixture.repo()?;

    let sha = head_oid.to_string();
    let changesets = resolve_source(repo, "main", Source::classify(&sha))?;
    assert_eq!(changesets.len(), 1);
    match changesets[0].span {
        ChangesetSpan::Committed { base, head } => {
            assert_eq!(base, parent_oid);
            assert_eq!(head, head_oid);
        }
        other => panic!("expected Committed, got {other:?}"),
    }
    assert_eq!(changesets[0].name, sha);
    assert!(changesets[0].current);
    Ok(())
}

/// A tag resolves to the commit it points at, same as a bare sha.
#[test]
fn tag_resolves_to_tagged_commit() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let parent_oid = fixture.head()?.peel_to_commit()?.id();
    let head_oid = fixture.commit("main").file("a.txt", "1").create("first")?;
    let repo = fixture.repo()?;
    let tagged = repo.find_object(head_oid, Some(ObjectType::Commit))?;
    repo.tag_lightweight("v1", &tagged, false)?;

    let changesets = resolve_source(repo, "main", Source::classify("v1"))?;
    assert_eq!(changesets.len(), 1);
    match changesets[0].span {
        ChangesetSpan::Committed { base, head } => {
            assert_eq!(base, parent_oid);
            assert_eq!(head, head_oid);
        }
        other => panic!("expected Committed, got {other:?}"),
    }
    Ok(())
}

/// A root commit (no parent) reviewed on its own must still render — its base is the empty
/// tree, so every file in it shows as added. The commit is a genuine orphan (parents: &[]) so
/// it has no ancestry to fall back on, addressed only by its own sha.
#[test]
fn root_commit_renders_against_the_empty_tree() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    let sig = git2::Signature::now("Test User", "test@example.com")?;
    let blob_oid = repo.blob(b"hello")?;
    let mut builder = repo.treebuilder(None)?;
    builder.insert("a.txt", blob_oid, 0o100_644)?;
    let tree_oid = builder.write()?;
    let tree = repo.find_tree(tree_oid)?;
    let root_oid = repo.commit(None, &sig, &sig, "orphan root", &tree, &[])?;

    let sha = root_oid.to_string();
    let changesets = resolve_source(repo, "main", Source::classify(&sha))?;
    assert_eq!(changesets.len(), 1);
    assert_eq!(
        changesets[0].span,
        ChangesetSpan::CommittedRoot { head: root_oid }
    );

    match diff_changeset(repo, &changesets[0])? {
        ChangesetDiff::Committed(model) => {
            assert!(!model.files.is_empty(), "root commit must render its file")
        }
        other => panic!("expected a Committed diff, got {other:?}"),
    }
    Ok(())
}

/// `a..b` and `a...b` diverge after the branches actually diverge: two-dot bases on `a` itself,
/// three-dot bases on their merge-base.
#[test]
fn two_dot_and_three_dot_ranges_differ_after_divergence() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .default_branch("main")
        .worktree("feature")
        .build()?;
    fixture
        .commit("main")
        .file("a.txt", "1")
        .create("on main")?;
    fixture
        .commit("feature")
        .file("b.txt", "1")
        .create("on feature")?;
    let repo = fixture.repo()?;

    let main_tip = repo
        .find_branch("main", git2::BranchType::Local)?
        .get()
        .target()
        .unwrap();
    let feature_tip = repo
        .find_branch("feature", git2::BranchType::Local)?
        .get()
        .target()
        .unwrap();
    let expected_merge_base = repo.merge_base(main_tip, feature_tip)?;

    let two_dot = resolve_source(repo, "main", Source::classify("main..feature"))?;
    let three_dot = resolve_source(repo, "main", Source::classify("main...feature"))?;

    match (&two_dot[0].span, &three_dot[0].span) {
        (
            ChangesetSpan::Committed { base: b2, head: h2 },
            ChangesetSpan::Committed { base: b3, head: h3 },
        ) => {
            assert_eq!(*h2, feature_tip);
            assert_eq!(*h3, feature_tip);
            assert_eq!(*b2, main_tip, "two-dot bases directly on the left endpoint");
            assert_eq!(
                *b3, expected_merge_base,
                "three-dot bases on the merge-base"
            );
            assert_ne!(b2, b3, "two-dot and three-dot bases diverge");
        }
        other => panic!("expected two Committed spans, got {other:?}"),
    }
    Ok(())
}

/// An empty side of a range defaults to `HEAD` at resolution time.
#[test]
fn range_empty_side_defaults_to_head() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().branch("old").build()?;
    fixture
        .commit("main")
        .file("a.txt", "1")
        .create("advance main")?;
    let repo = fixture.repo()?;
    let head_oid = repo.head()?.peel_to_commit()?.id();

    let explicit = resolve_source(repo, "main", Source::classify("old..main"))?;
    let defaulted = resolve_source(repo, "main", Source::classify("old.."))?;
    // Names differ (source text as typed); spans must be identical — the empty side resolved
    // to the exact same commit as writing `main` out explicitly.
    assert_eq!(explicit[0].span, defaulted[0].span);
    match defaulted[0].span {
        ChangesetSpan::Committed { head, .. } => assert_eq!(head, head_oid),
        other => panic!("expected Committed, got {other:?}"),
    }
    Ok(())
}

/// `review <tag>..<same tag>` is a valid-but-empty range: exit 0, "nothing to review" naming
/// the source text (ADR-036's empty-but-valid UX, extended by `<ref>` and range resolution to
/// name the source).
#[test]
fn empty_range_between_same_tag_prints_named_nothing_to_review_and_exits_zero() {
    let fixture = FixtureBuilder::new().build().unwrap();
    let repo = fixture.repo().unwrap();
    let head_oid = repo.head().unwrap().peel_to_commit().unwrap().id();
    let tagged = repo
        .find_object(head_oid, Some(ObjectType::Commit))
        .unwrap();
    repo.tag_lightweight("v1", &tagged, false).unwrap();
    let workdir = repo.workdir().unwrap();

    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.current_dir(workdir)
        .env("NO_COLOR", "1")
        .arg("v1..v1")
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing to review in v1..v1"));
}
