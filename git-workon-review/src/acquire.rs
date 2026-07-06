//! Acquiring a [`DiffModel`] for a resolved rev pair or the live worktree, and routing a
//! [`workon::Changeset`] to the right one.
//!
//! Stays deliberately thin: [`workon::assemble_changesets`] already resolved *what* to diff
//! (a committed rev pair, or "uncommitted"); this module only knows *how* to turn that into
//! git2 diffs and then a [`DiffModel`].

use git2::{DiffOptions, Oid, Repository};
use workon::{Changeset, ChangesetSource};

use crate::error::DiffError;
use crate::model::DiffModel;

/// The two working-tree diffs a review session needs: the index against `HEAD` (staged), and
/// the working tree against the index (unstaged, including untracked content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDiffs {
    pub staged: DiffModel,
    pub unstaged: DiffModel,
}

/// Diff `HEAD`'s tree against the index (staged) and the index against the working tree
/// (unstaged), for a [`ChangesetSource::Uncommitted`] changeset.
///
/// The unstaged side sets `include_untracked`/`recurse_untracked_dirs`/
/// `show_untracked_content` so untracked files carry real content in the model (git2 gives
/// `Delta::Untracked` natively here — no `/dev/null` header synthesis needed).
pub fn diff_uncommitted(repo: &Repository) -> Result<WorktreeDiffs, DiffError> {
    let head_tree = repo.head()?.peel_to_tree()?;

    let mut staged_opts = DiffOptions::new();
    staged_opts.context_lines(3);
    let staged_diff = repo.diff_tree_to_index(Some(&head_tree), None, Some(&mut staged_opts))?;
    let staged = DiffModel::from_git2(&staged_diff)?;

    let mut unstaged_opts = DiffOptions::new();
    unstaged_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .context_lines(3);
    let unstaged_diff = repo.diff_index_to_workdir(None, Some(&mut unstaged_opts))?;
    let unstaged = DiffModel::from_git2(&unstaged_diff)?;

    Ok(WorktreeDiffs { staged, unstaged })
}

/// Diff `base`'s tree against `head`'s tree, for a [`ChangesetSource::Committed`] changeset —
/// rename/copy detection runs via [`git2::Diff::find_similar`] so renamed files come back as
/// [`crate::model::FileStatus::Renamed`] instead of a delete+add pair.
pub fn diff_committed(repo: &Repository, base: Oid, head: Oid) -> Result<DiffModel, DiffError> {
    let base_tree = repo.find_commit(base)?.tree()?;
    let head_tree = repo.find_commit(head)?.tree()?;

    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let mut diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut opts))?;
    diff.find_similar(None)?;

    DiffModel::from_git2(&diff)
}

/// The diff for one [`Changeset`], shaped by its [`ChangesetSource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesetDiff {
    Committed(DiffModel),
    Uncommitted(WorktreeDiffs),
}

/// Diff `cs`, routing on its [`ChangesetSource`].
///
/// A changeset carrying a resolved-but-unreadable rev pair (a bad or garbage `Oid` — e.g.
/// stale Graphite metadata pointing at a pruned commit) is a genuine failure, never an empty
/// [`DiffModel`]: any underlying git2 error is reported as
/// [`DiffError::ChangesetDiffFailed`].
pub fn diff_changeset(repo: &Repository, cs: &Changeset) -> Result<ChangesetDiff, DiffError> {
    match cs.source {
        ChangesetSource::Committed { base, head } => diff_committed(repo, base, head)
            .map(ChangesetDiff::Committed)
            .map_err(|err| changeset_diff_failed(&cs.name, err)),
        ChangesetSource::Uncommitted => diff_uncommitted(repo)
            .map(ChangesetDiff::Uncommitted)
            .map_err(|err| changeset_diff_failed(&cs.name, err)),
    }
}

/// Fold a [`DiffError`] into [`DiffError::ChangesetDiffFailed`], attaching the changeset name.
fn changeset_diff_failed(name: &str, err: DiffError) -> DiffError {
    match err {
        DiffError::Git(source) => DiffError::ChangesetDiffFailed {
            name: name.to_string(),
            source,
        },
        already_wrapped @ DiffError::ChangesetDiffFailed { .. } => already_wrapped,
    }
}
