//! Acquiring a [`DiffModel`] for a resolved rev pair or the live worktree, and routing a
//! [`workon::Changeset`] to the right one.
//!
//! Stays deliberately thin: [`workon::assemble_changesets`] already resolved *what* to diff
//! (a committed rev pair, or "uncommitted"); this module only knows *how* to turn that into
//! git2 diffs and then a [`DiffModel`].

use git2::{DiffFindOptions, DiffOptions, Oid, Repository};
use workon::{assemble_changesets, Changeset, ChangesetSpan, StackModel, UncommittedLayer};

use crate::error::DiffError;
use crate::model::DiffModel;

/// The working-tree diffs a review session needs: the index against `HEAD` (staged), the
/// working tree against the index (unstaged, including untracked content), and the fused
/// `HEAD` ↔ worktree view (whole) the renderer reviews by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDiffs {
    pub staged: DiffModel,
    pub unstaged: DiffModel,
    /// `HEAD`'s tree diffed straight against the working tree (index consulted only for
    /// untracked/ignore filtering), fusing staged and unstaged hunks on the same file into one
    /// diff — the whole-diff view the renderer reviews by default.
    pub whole: DiffModel,
}

/// Diff `HEAD`'s tree against the index (staged), the index against the working tree
/// (unstaged), and `HEAD`'s tree against the working tree directly (whole), for a
/// [`ChangesetSpan::Uncommitted`] changeset.
///
/// The unstaged and whole sides both set `include_untracked`/`recurse_untracked_dirs`/
/// `show_untracked_content` so untracked files carry real content in the model (git2 gives
/// `Delta::Untracked` natively here — no `/dev/null` header synthesis needed). `find_similar`
/// runs on all three diffs before materialization so worktree renames (e.g. an untracked file
/// that replaces a tracked one under a new name) surface as [`crate::model::FileStatus::Renamed`]
/// rather than a delete+add pair — the read side already handles that status (corpus-proven).
///
/// The two untracked-including diffs (unstaged, whole) pass explicit
/// [`DiffFindOptions::for_untracked`] — plain `find_similar(None)`'s default flags (just
/// `GIT_DIFF_FIND_RENAMES`) do NOT pair an untracked file with a workdir deletion; libgit2
/// requires `for_untracked` opted in separately for that side of the match. The staged diff
/// never sees untracked deltas, so `None` (matching [`diff_committed`]'s convention) is enough
/// there.
pub fn diff_uncommitted(repo: &Repository) -> Result<WorktreeDiffs, DiffError> {
    let head_tree = repo.head()?.peel_to_tree()?;

    let mut staged_opts = DiffOptions::new();
    staged_opts.context_lines(3);
    let mut staged_diff =
        repo.diff_tree_to_index(Some(&head_tree), None, Some(&mut staged_opts))?;
    staged_diff.find_similar(None)?;
    let staged = DiffModel::from_git2(&staged_diff)?;

    let mut worktree_opts = DiffOptions::new();
    worktree_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .context_lines(3);
    let mut untracked_find = DiffFindOptions::new();
    untracked_find.renames(true).for_untracked(true);

    let mut unstaged_diff = repo.diff_index_to_workdir(None, Some(&mut worktree_opts))?;
    unstaged_diff.find_similar(Some(&mut untracked_find))?;
    let unstaged = DiffModel::from_git2(&unstaged_diff)?;

    let mut whole_diff =
        repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut worktree_opts))?;
    whole_diff.find_similar(Some(&mut untracked_find))?;
    let whole = DiffModel::from_git2(&whole_diff)?;

    Ok(WorktreeDiffs {
        staged,
        unstaged,
        whole,
    })
}

/// Diff `base`'s tree against `head`'s tree, for a [`ChangesetSpan::Committed`] changeset —
/// rename/copy detection runs via [`git2::Diff::find_similar`] so renamed files come back as
/// [`crate::model::FileStatus::Renamed`] instead of a delete+add pair.
pub fn diff_committed(repo: &Repository, base: Oid, head: Oid) -> Result<DiffModel, DiffError> {
    let base_tree = repo.find_commit(base)?.tree()?;
    let head_tree = repo.find_commit(head)?.tree()?;

    diff_trees(repo, Some(&base_tree), &head_tree)
}

/// Diff the empty tree against `head`'s tree, for a [`ChangesetSpan::CommittedRoot`] changeset
/// (a root commit reviewed on its own — every file in `head` renders as added). `git2` treats
/// `None` as the empty tree on either side of [`Repository::diff_tree_to_tree`], so no tree
/// object needs to be synthesized.
pub fn diff_committed_root(repo: &Repository, head: Oid) -> Result<DiffModel, DiffError> {
    let head_tree = repo.find_commit(head)?.tree()?;

    diff_trees(repo, None, &head_tree)
}

/// Shared tail of [`diff_committed`] and [`diff_committed_root`]: diff `base` (the empty tree
/// when `None`) against `head`, with rename/copy detection via [`git2::Diff::find_similar`] so
/// renamed files come back as [`crate::model::FileStatus::Renamed`] instead of a delete+add
/// pair.
fn diff_trees(
    repo: &Repository,
    base: Option<&git2::Tree>,
    head: &git2::Tree,
) -> Result<DiffModel, DiffError> {
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let mut diff = repo.diff_tree_to_tree(base, Some(head), Some(&mut opts))?;
    diff.find_similar(None)?;

    DiffModel::from_git2(&diff)
}

/// The diff for one [`Changeset`], shaped by its [`ChangesetSpan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesetDiff {
    Committed(DiffModel),
    Uncommitted(WorktreeDiffs),
}

/// Diff `cs`, routing on its [`ChangesetSpan`].
///
/// A changeset carrying a resolved-but-unreadable rev pair (a bad or garbage `Oid` — e.g.
/// stale Graphite metadata pointing at a pruned commit) is a genuine failure, never an empty
/// [`DiffModel`]: any underlying git2 error is reported as
/// [`DiffError::ChangesetDiffFailed`].
pub fn diff_changeset(repo: &Repository, cs: &Changeset) -> Result<ChangesetDiff, DiffError> {
    match cs.span {
        ChangesetSpan::Committed { base, head } => diff_committed(repo, base, head)
            .map(ChangesetDiff::Committed)
            .map_err(|err| changeset_diff_failed(&cs.name, err)),
        ChangesetSpan::CommittedRoot { head } => diff_committed_root(repo, head)
            .map(ChangesetDiff::Committed)
            .map_err(|err| changeset_diff_failed(&cs.name, err)),
        ChangesetSpan::Uncommitted => diff_uncommitted(repo)
            .map(ChangesetDiff::Uncommitted)
            .map_err(|err| changeset_diff_failed(&cs.name, err)),
    }
}

/// Diff every changeset in `changesets`, returning the diffs in input order.
///
/// The changesets are independent, and a review of a deep stack runs one [`diff_changeset`]
/// per node (a few ms of tree diff + rename detection each) — run sequentially their sum
/// gates the first frame at startup and every whole-stack refresh. So the work is striped
/// across `available_parallelism` threads. git2's `Repository` is `Send` but not `Sync`, so
/// each worker opens its own handle on the same on-disk repo instead of sharing `repo`.
///
/// On any failure the first-failing changeset's error (by input order) is returned, matching
/// what the sequential loop this replaces would have surfaced.
pub fn diff_changesets(
    repo: &Repository,
    changesets: &[Changeset],
) -> Result<Vec<ChangesetDiff>, DiffError> {
    let workers = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(changesets.len());
    if workers <= 1 {
        return changesets
            .iter()
            .map(|cs| diff_changeset(repo, cs))
            .collect();
    }

    // Workers re-open at the workdir so the Uncommitted span's index/worktree diffs resolve
    // against the same working tree as `repo`; the gitdir is the fallback for a bare repo
    // (where only committed spans can occur).
    let open_at = repo.workdir().unwrap_or_else(|| repo.path()).to_path_buf();

    let chunk = changesets.len().div_ceil(workers);
    let mut results: Vec<Option<Result<ChangesetDiff, DiffError>>> = Vec::new();
    results.resize_with(changesets.len(), || None);

    std::thread::scope(|scope| {
        for (cs_chunk, out_chunk) in changesets.chunks(chunk).zip(results.chunks_mut(chunk)) {
            let open_at = &open_at;
            scope.spawn(move || {
                let repo = match Repository::open(open_at) {
                    Ok(repo) => repo,
                    Err(err) => {
                        // Every changeset in this chunk is undiffable without a handle; the
                        // first slot's error is the one input-order selection below reports.
                        out_chunk[0] = Some(Err(err.into()));
                        return;
                    }
                };
                for (cs, out) in cs_chunk.iter().zip(out_chunk.iter_mut()) {
                    *out = Some(diff_changeset(&repo, cs));
                }
            });
        }
    });

    results
        .into_iter()
        .map(|slot| slot.expect("every chunk fills its slots or errors its first slot"))
        .collect()
}

/// Resolve the changeset stack the review App opens on for the worktree whose `HEAD` is
/// `head_branch` (locked design decision: auto-detect Graphite, else a single uncommitted
/// changeset): the full Graphite stack
/// when one is active, or a single synthetic [`Changeset`] spanning the uncommitted worktree
/// otherwise.
///
/// This does NOT simply forward to [`workon::assemble_changesets`] with the detected
/// [`StackModel`]: that function returns `Ok(vec![])` for [`StackModel::None`] unconditionally
/// (see its module docs) rather than a reviewable uncommitted layer, and mapping `None` to
/// [`StackModel::Git`] instead would make every branch without upstream tracking (the common
/// case for a scratch/local branch) fail with `NoUpstream` before it ever got to review a dirty
/// tree — a regression from the original "just diff the worktree" default. So the
/// non-Graphite case is built directly here, matching exactly what `assemble_changesets`'s own
/// `insert_uncommitted_layer` would produce for a lone dirty tree: one `current` entry, no
/// title, not needing a restack.
pub fn resolve_changesets(
    repo: &Repository,
    head_branch: &str,
) -> Result<Vec<Changeset>, DiffError> {
    match StackModel::detect(repo) {
        model @ (StackModel::Graphite | StackModel::GhStack) => Ok(assemble_changesets(
            repo,
            head_branch,
            model,
            UncommittedLayer::Include,
        )?),
        StackModel::None | StackModel::Git => Ok(vec![uncommitted_changeset(head_branch)]),
    }
}

/// The single synthetic [`ChangesetSpan::Uncommitted`] changeset for `head_branch` — always
/// `current`, no title, no restack question. Shared by [`resolve_changesets`]'s non-Graphite
/// fallback arm and the review binary's `uncommitted` keyword (`crate::source`), both of which
/// mean the same thing: "just diff the worktree."
pub fn uncommitted_changeset(head_branch: &str) -> Changeset {
    Changeset {
        name: head_branch.to_string(),
        span: ChangesetSpan::Uncommitted,
        title: None,
        current: true,
        needs_restack: false,
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
        // `diff_committed`/`diff_uncommitted` only ever raise `Git`, so this arm is
        // unreachable in practice — kept exhaustive rather than a wildcard so a future
        // `DiffError` variant forces a decision here instead of silently falling through.
        already_wrapped @ DiffError::StackAssembly(_) => already_wrapped,
    }
}
