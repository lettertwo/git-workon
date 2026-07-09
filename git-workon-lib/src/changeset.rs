//! Changeset assembly: turning a stack model + repository state into an ordered list of
//! reviewable [`Changeset`]s for the worktree whose `HEAD` is a given branch.
//!
//! This is the substrate the review TUI (M2+) consumes. It stays **diff-free**: every
//! [`Changeset`] carries resolved `git2::Oid` rev pairs (or the [`ChangesetSpan::Uncommitted`]
//! marker), never a parsed diff. Detecting uncommitted changes uses `repo.statuses`, never
//! `repo.diff_*`.
//!
//! ## Assembly per [`StackModel`]
//!
//! - [`StackModel::None`] → always `Ok(vec![])`.
//! - [`StackModel::Graphite`] → walks recorded stack metadata: ancestors of `head_branch`
//!   (bottom → just-below-head), then `head_branch` itself, then descendants (depth-first,
//!   siblings sorted lexically). Ghost nodes (a metadata row with no resolvable branch ref)
//!   are skipped from the output but still walked through, so live descendants of a ghost
//!   still appear. Falls back to the `Git` arm when `head_branch` is a trunk branch or has no
//!   metadata row at all (mirrors the nvim prototype's factory behavior).
//! - [`StackModel::Git`] → no metadata; walks `upstream..head_branch` commit-by-commit
//!   (oldest first), one [`Changeset`] per commit.
//!
//! In both metadata-bearing arms, a non-empty `repo.statuses` result inserts a
//! [`ChangesetSpan::Uncommitted`] entry immediately after the current node, taking over
//! `current` — but only when the caller passes [`UncommittedLayer::Include`]. The layer
//! belongs only when the thing under review is where the working tree actually is; a caller
//! resolving a source that isn't real `HEAD` (a range, a commit, a PR, a tracked branch you're
//! not standing on) passes [`UncommittedLayer::Omit`] instead. See [`UncommittedLayer`]'s own
//! doc for the full rationale.

use std::collections::{HashMap, HashSet};

use git2::{BranchType, Oid, Repository, StatusOptions};

use crate::error::{ChangesetError, Result};
use crate::stack::{graphite, StackModel};

/// What a [`Changeset`] spans: a resolved commit range, or the working tree + index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesetSpan {
    /// A committed range `base..head` — resolved OIDs only; the lib never diffs them itself.
    Committed { base: Oid, head: Oid },
    /// A committed range whose base is the empty tree: a root commit (no parent) reviewed on
    /// its own, so every file in `head` renders as added. Only the review crate's `<ref>`
    /// bare-commit-ish dispatch (ADR-030) constructs this — `assemble_graphite`/`assemble_git`
    /// never do, since a stack node's base is always a real (or merge-base-derived) commit.
    CommittedRoot { head: Oid },
    /// Uncommitted working-tree + index changes relative to the current branch's head.
    Uncommitted,
}

/// One reviewable unit in an assembled changeset stack, ordered base → head by
/// [`assemble_changesets`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changeset {
    /// Branch name for stack nodes; 8-hex abbreviated commit id for git-inference per-commit
    /// changesets; the current branch name for [`ChangesetSpan::Uncommitted`].
    pub name: String,
    /// The commit range (or uncommitted marker) this changeset covers.
    pub span: ChangesetSpan,
    /// PR title (from `.graphite_pr_info`) for Graphite nodes; commit summary for
    /// git-inference nodes; `None` for [`ChangesetSpan::Uncommitted`].
    pub title: Option<String>,
    /// Exactly one entry in the returned `Vec` is current: the Uncommitted entry when
    /// present, otherwise the current branch's node (Graphite) or tip commit (Git).
    pub current: bool,
    /// Graphite nodes only: the recorded parent revision is non-empty AND differs from the
    /// parent branch's *live* tip. Always `false` for git-inference and Uncommitted entries.
    pub needs_restack: bool,
}

/// Whether [`assemble_changesets`] should insert the synthetic [`ChangesetSpan::Uncommitted`]
/// layer when the worktree has a dirty tree (see [`insert_uncommitted_layer`]).
///
/// ADR-030: the layer only belongs when the thing under review is where the working tree
/// actually is (`stack`, or a `<ref>` that is the current `HEAD` branch) — every other source
/// (a range, a commit, a PR, an untracked branch, a tracked branch you're not standing on) is
/// committed-only, since uncommitted changes diff against `HEAD` and would otherwise attach to
/// a branch they don't belong to. An explicit parameter, not a post-filter: a post-filter would
/// also have to repair whichever node's `current` flag the inserted layer took over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncommittedLayer {
    /// Insert the layer when the tree is dirty (today's behavior).
    Include,
    /// Never insert the layer, regardless of tree state.
    Omit,
}

/// Assemble the ordered (base → head) changesets for the worktree whose `HEAD` is
/// `head_branch`, under the given [`StackModel`].
///
/// See the module docs for the per-model walk semantics. Errors distinguish a genuinely
/// broken reference or stack-metadata snapshot (bad ref, unresolvable recorded revision, no
/// upstream) from a valid empty result (`Ok(vec![])`, e.g. a trunk-only worktree under `Git`
/// with a clean tree). `uncommitted` controls whether a dirty tree gets the synthetic
/// [`ChangesetSpan::Uncommitted`] layer at all — see [`UncommittedLayer`].
pub fn assemble_changesets(
    repo: &Repository,
    head_branch: &str,
    model: StackModel,
    uncommitted: UncommittedLayer,
) -> Result<Vec<Changeset>> {
    match model {
        StackModel::None => Ok(vec![]),
        StackModel::Git => assemble_git(repo, head_branch, uncommitted),
        StackModel::Graphite => assemble_graphite(repo, head_branch, uncommitted),
    }
}

/// Graphite-metadata-driven assembly (see module docs for the walk).
fn assemble_graphite(
    repo: &Repository,
    head_branch: &str,
    uncommitted: UncommittedLayer,
) -> Result<Vec<Changeset>> {
    let metadata = graphite::read_branch_metadata(repo)?;
    let trunks: HashSet<String> = graphite::read_trunks(repo).into_iter().collect();

    // Trunk or untracked head_branch: no stack metadata to walk, fall back to git-inference.
    if trunks.contains(head_branch) || !metadata.contains_key(head_branch) {
        return assemble_git(repo, head_branch, uncommitted);
    }

    // head_branch is tracked but its own branch ref is gone: a genuinely broken state, distinct
    // from an empty result.
    if repo.find_branch(head_branch, BranchType::Local).is_err() {
        return Err(ChangesetError::UnresolvableBranch {
            branch: head_branch.to_string(),
        }
        .into());
    }

    // Ancestors bottom → just-below-head, following recorded parent links. Stops at a trunk
    // parent or a branch absent from the metadata map; cycle-guarded (an `a` <-> `b` cycle in
    // metadata must terminate, not hang).
    let mut ancestors_desc: Vec<String> = Vec::new();
    {
        let mut walk = head_branch.to_string();
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(walk.clone());
        loop {
            if trunks.contains(&walk) {
                break;
            }
            let Some(entry) = metadata.get(&walk) else {
                break;
            };
            let parent = entry.parent.clone();
            if trunks.contains(&parent) {
                break;
            }
            // An untracked parent (no metadata row) is outside the stack: stop without
            // emitting it, matching the prototype walk. Every walked name therefore has a
            // metadata row.
            if !metadata.contains_key(&parent) {
                break;
            }
            if !seen.insert(parent.clone()) {
                break; // cycle guard
            }
            ancestors_desc.push(parent.clone());
            walk = parent;
        }
    }
    ancestors_desc.reverse();

    // Descendants: depth-first from head_branch, siblings sorted lexically, cycle-guarded.
    let mut reverse_map: HashMap<String, Vec<String>> = HashMap::new();
    for (branch, entry) in &metadata {
        reverse_map
            .entry(entry.parent.clone())
            .or_default()
            .push(branch.clone());
    }
    for children in reverse_map.values_mut() {
        children.sort();
    }

    fn visit_descendants(
        branch: &str,
        reverse_map: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        out: &mut Vec<String>,
    ) {
        if let Some(children) = reverse_map.get(branch) {
            for child in children {
                if visited.insert(child.clone()) {
                    out.push(child.clone());
                    visit_descendants(child, reverse_map, visited, out);
                }
            }
        }
    }

    let mut descendants: Vec<String> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(head_branch.to_string());
    visit_descendants(head_branch, &reverse_map, &mut visited, &mut descendants);

    let titles = graphite::read_pr_titles(repo);

    let ordered_names: Vec<String> = ancestors_desc
        .into_iter()
        .chain(std::iter::once(head_branch.to_string()))
        .chain(descendants)
        .collect();

    let mut changesets: Vec<Changeset> = Vec::new();
    let mut current_index: Option<usize> = None;
    for name in ordered_names {
        // Ghost node: metadata row lingers, no branch exists anywhere. Skip from output —
        // its children were already reached by visit_descendants regardless.
        if !crate::resolve::branch_exists(repo, &name) {
            continue;
        }
        // branch_exists also matches remote-only branches; a stack member with no LOCAL
        // ref has no live head to span a changeset to, and that is an error, not a ghost.
        let branch_ref = repo.find_branch(&name, BranchType::Local).map_err(|_| {
            ChangesetError::UnresolvableBranch {
                branch: name.clone(),
            }
        })?;
        let head_oid =
            branch_ref
                .get()
                .target()
                .ok_or_else(|| ChangesetError::UnresolvableBranch {
                    branch: name.clone(),
                })?;

        // Every walked name has a metadata row: the ancestors walk stops at untracked
        // parents, head_branch was checked on entry, and descendants come from the map.
        let entry = &metadata[&name];
        let (base_oid, needs_restack) = resolve_graphite_base(
            repo,
            &metadata,
            &trunks,
            &entry.parent,
            entry.parent_revision.as_deref(),
            head_oid,
            &name,
        )?;

        let is_current = name == head_branch;
        if is_current {
            current_index = Some(changesets.len());
        }
        changesets.push(Changeset {
            title: titles.get(&name).cloned(),
            span: ChangesetSpan::Committed {
                base: base_oid,
                head: head_oid,
            },
            current: is_current,
            needs_restack,
            name,
        });
    }

    if uncommitted == UncommittedLayer::Include {
        insert_uncommitted_layer(repo, head_branch, current_index, &mut changesets)?;
    }

    Ok(changesets)
}

/// Resolve `(base, needs_restack)` for one Graphite stack node.
///
/// `parent_revision`, when present, is the recorded snapshot — verified against the odb and
/// used as `base` directly (no need to resolve the parent's live ref: this keeps a stale
/// recorded revision spanning a ghost parent computable). `needs_restack` compares it against
/// the parent's *live* tip when that tip is resolvable; an unresolvable parent (e.g. a ghost)
/// with a recorded revision present is not a restack question — `needs_restack` is `false`.
///
/// When `parent_revision` is missing, `base` falls back to `merge_base(ancestor_tip, head)`,
/// where `ancestor_tip` is the nearest *live* branch (or trunk) reached by walking through
/// `metadata` past any ghost parents — a live node hanging off a ghost whose own recorded
/// parent revision never resolved (because the ghost's branch ref never existed to resolve a
/// tip from) still needs a computable base. Only when that walk finds nothing resolvable at
/// all is it a genuine error.
fn resolve_graphite_base(
    repo: &Repository,
    metadata: &HashMap<String, graphite::BranchMetadata>,
    trunks: &HashSet<String>,
    parent: &str,
    parent_revision: Option<&str>,
    head: Oid,
    branch: &str,
) -> Result<(Oid, bool)> {
    match parent_revision {
        Some(rev) => {
            let oid = Oid::from_str(rev)
                .ok()
                .filter(|oid| repo.find_commit(*oid).is_ok())
                .ok_or_else(|| ChangesetError::InvalidParentRevision {
                    branch: branch.to_string(),
                    revision: rev.to_string(),
                })?;
            let parent_live_tip = repo
                .find_branch(parent, BranchType::Local)
                .ok()
                .and_then(|b| b.get().target());
            let needs_restack = match parent_live_tip {
                Some(tip) => oid != tip,
                None => false,
            };
            Ok((oid, needs_restack))
        }
        None => {
            let tip =
                resolve_live_ancestor_tip(repo, metadata, trunks, parent).ok_or_else(|| {
                    ChangesetError::UnresolvableBranch {
                        branch: parent.to_string(),
                    }
                })?;
            let base = repo.merge_base(tip, head)?;
            Ok((base, false))
        }
    }
}

/// Walk from `start` through `metadata`'s parent links (cycle-guarded) until a branch that
/// resolves to a live ref is found, returning its tip. `start` itself is checked first, so a
/// live `start` resolves immediately; a ghost `start` walks to its recorded parent, and so on.
fn resolve_live_ancestor_tip(
    repo: &Repository,
    metadata: &HashMap<String, graphite::BranchMetadata>,
    trunks: &HashSet<String>,
    start: &str,
) -> Option<Oid> {
    let mut walk = start.to_string();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(walk.clone());
    loop {
        if let Some(tip) = repo
            .find_branch(&walk, BranchType::Local)
            .ok()
            .and_then(|b| b.get().target())
        {
            return Some(tip);
        }
        if trunks.contains(&walk) {
            return None;
        }
        match metadata.get(&walk) {
            Some(entry) if seen.insert(entry.parent.clone()) => walk = entry.parent.clone(),
            _ => return None, // no metadata entry, or cycle
        }
    }
}

/// Git-inference assembly: one [`Changeset`] per commit in `upstream(head_branch)..head_branch`,
/// oldest first.
fn assemble_git(
    repo: &Repository,
    head_branch: &str,
    uncommitted: UncommittedLayer,
) -> Result<Vec<Changeset>> {
    let branch = repo.find_branch(head_branch, BranchType::Local)?;
    let upstream = branch.upstream().map_err(|_| ChangesetError::NoUpstream {
        branch: head_branch.to_string(),
    })?;
    let head_oid = branch
        .get()
        .target()
        .ok_or_else(|| ChangesetError::NoUpstream {
            branch: head_branch.to_string(),
        })?;
    let upstream_oid = upstream
        .get()
        .target()
        .ok_or_else(|| ChangesetError::NoUpstream {
            branch: head_branch.to_string(),
        })?;

    let mut revwalk = repo.revwalk()?;
    revwalk.push(head_oid)?;
    revwalk.hide(upstream_oid)?;
    revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
    // Follow only the branch's own first-parent line: without this, a merge into the
    // branch enumerates every merged-in commit as its own changeset AND the merge commit
    // (whose base is its first parent) spans the same content again — double-counted.
    revwalk.simplify_first_parent()?;

    let mut changesets: Vec<Changeset> = Vec::new();
    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        // Merges use the first parent; a parentless (root) commit has no base to diff
        // against, so it emits base == head — the whole commit is its own changeset.
        let base = commit.parent_id(0).unwrap_or(oid);
        changesets.push(Changeset {
            name: short_id(oid),
            span: ChangesetSpan::Committed { base, head: oid },
            title: commit.summary()?.map(str::to_string),
            current: false,
            needs_restack: false,
        });
    }

    let current_index = if changesets.is_empty() {
        None
    } else {
        let last = changesets.len() - 1;
        changesets[last].current = true;
        Some(last)
    };

    if uncommitted == UncommittedLayer::Include {
        insert_uncommitted_layer(repo, head_branch, current_index, &mut changesets)?;
    }

    Ok(changesets)
}

/// 8-hex abbreviated commit id, per the [`Changeset::name`] doc for git-inference nodes.
fn short_id(oid: Oid) -> String {
    oid.to_string()[..8].to_string()
}

/// Insert a [`ChangesetSpan::Uncommitted`] entry immediately after `current_index` (or at
/// the end, if there is no committed current node) when `repo.statuses` reports any working
/// tree or index changes. Demotes the previous current node's `current` flag. No-op on a
/// clean tree.
fn insert_uncommitted_layer(
    repo: &Repository,
    current_branch: &str,
    current_index: Option<usize>,
    changesets: &mut Vec<Changeset>,
) -> Result<()> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true);
    opts.include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    if statuses.is_empty() {
        return Ok(());
    }

    if let Some(idx) = current_index {
        changesets[idx].current = false;
    }
    let insert_at = current_index.map_or(changesets.len(), |i| i + 1);
    changesets.insert(
        insert_at,
        Changeset {
            name: current_branch.to_string(),
            span: ChangesetSpan::Uncommitted,
            title: None,
            current: true,
            needs_restack: false,
        },
    );
    Ok(())
}
