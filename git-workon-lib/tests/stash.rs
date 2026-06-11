//! Tests for labeled autostash create/find/apply/list.

use std::path::Path;

use git_workon_fixture::prelude::*;
use workon::{
    apply_labeled_stash, create_labeled_stash, find_labeled_stash, list_labeled_for_worktree,
    StashRestore,
};

/// Commit `content` to `name` in the worktree at `wt_path` (stage + commit on HEAD).
fn commit_file(
    wt_path: &Path,
    name: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = git2::Repository::open(wt_path)?;
    std::fs::write(wt_path.join(name), content)?;
    let mut index = repo.index()?;
    index.add_path(Path::new(name))?;
    index.write()?;
    let tree = repo.find_tree(index.write_tree()?)?;
    let sig = git2::Signature::now("test", "test@test.com")?;
    let parent = repo.head()?.peel_to_commit()?;
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &format!("edit {}", name),
        &tree,
        &[&parent],
    )?;
    Ok(())
}

#[test]
fn find_labeled_stash_matches_exact_pair_only() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("home")
        .build()?;

    let wt_path = fixture.root()?.join("home");
    std::fs::write(wt_path.join("wip.txt"), "wip")?;
    let mut wt_repo = git2::Repository::open(&*wt_path)?;
    create_labeled_stash(&mut wt_repo, "feat", "api-v2")?;

    // A worktree label that is a prefix of the stored one must not match.
    assert_eq!(find_labeled_stash(&mut wt_repo, "feat", "api")?, None);
    // A branch label that is a prefix of the stored one must not match.
    assert_eq!(find_labeled_stash(&mut wt_repo, "fea", "api-v2")?, None);
    // The exact pair matches.
    assert_eq!(find_labeled_stash(&mut wt_repo, "feat", "api-v2")?, Some(0));

    Ok(())
}

#[test]
fn list_labeled_for_worktree_ignores_prefix_collisions() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("home")
        .build()?;

    let wt_path = fixture.root()?.join("home");
    std::fs::write(wt_path.join("wip.txt"), "wip")?;
    let mut wt_repo = git2::Repository::open(&*wt_path)?;
    create_labeled_stash(&mut wt_repo, "x", "feature")?;

    // "feat" is a prefix of "feature" — must not be reported as feat's stash.
    assert!(list_labeled_for_worktree(&mut wt_repo, "feat")?.is_empty());
    assert_eq!(list_labeled_for_worktree(&mut wt_repo, "feature")?.len(), 1);

    Ok(())
}

#[test]
fn apply_labeled_stash_drops_entry_after_clean_apply() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("home")
        .build()?;

    let wt_path = fixture.root()?.join("home");
    std::fs::write(wt_path.join("wip.txt"), "wip")?;
    let mut wt_repo = git2::Repository::open(&*wt_path)?;
    create_labeled_stash(&mut wt_repo, "main", "home")?;
    assert!(!wt_path.join("wip.txt").exists());

    assert_eq!(
        apply_labeled_stash(&mut wt_repo, "main", "home")?,
        StashRestore::Applied
    );

    // The work is back in the tree and the entry is gone — a second restore
    // must not re-apply it.
    assert!(wt_path.join("wip.txt").exists());
    assert_eq!(find_labeled_stash(&mut wt_repo, "main", "home")?, None);
    wt_repo.assert(predicate::repo::has_no_stash());
    assert_eq!(
        apply_labeled_stash(&mut wt_repo, "main", "home")?,
        StashRestore::NotFound
    );

    Ok(())
}

#[test]
fn apply_labeled_stash_reports_merge_conflict_and_keeps_entry(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("home")
        .build()?;

    let wt_path = fixture.root()?.join("home");
    commit_file(&wt_path, "content.txt", "base")?;

    // Shelve an edit, then commit a different edit to the same lines so the
    // restore has to merge — stash_apply returns GIT_EMERGECONFLICT here.
    std::fs::write(wt_path.join("content.txt"), "stash-edit")?;
    {
        let mut wt_repo = git2::Repository::open(&*wt_path)?;
        create_labeled_stash(&mut wt_repo, "main", "home")?;
    }
    commit_file(&wt_path, "content.txt", "committed-edit")?;

    // Fresh handle: the apply must see the index written by the commit above.
    let mut wt_repo = git2::Repository::open(&*wt_path)?;

    assert_eq!(
        apply_labeled_stash(&mut wt_repo, "main", "home")?,
        StashRestore::Conflict
    );

    // The entry survives for manual recovery.
    assert_eq!(find_labeled_stash(&mut wt_repo, "main", "home")?, Some(0));
    wt_repo.assert(predicate::repo::has_stash("workon-autostash: main @ home"));

    Ok(())
}
