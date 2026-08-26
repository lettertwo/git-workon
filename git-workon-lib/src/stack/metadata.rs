//! Provider-free stack graph algorithms over [`StackMetadata`].
//!
//! Each stack provider (Graphite today; gh-stack later) reduces its own on-disk format to a
//! [`StackMetadata`] — a trunk set plus `branch → (parent, parent_revision)` — and every other
//! algorithm here is shared: connected-component enumeration, the current-stack walk, and the
//! changeset ancestor/descendant walk. Providers own parsing; this module owns the graph.
//!
//! **The ghost-pruning difference is enforced by signature, not by comment.** Only [`enumerate`]
//! takes a `&Repository`, so only [`enumerate`] can call [`crate::resolve::branch_exists`] to
//! prune ghost branches (metadata rows with no live ref) before building stacks. [`current`] and
//! [`changeset_walk`] take no `Repository` precisely because they must not prune: routing needs
//! deleted stack nodes to stay visible so it can distinguish a deleted stack node from a plain
//! typo, and changeset assembly needs to walk *through* a ghost to reach its live descendants.

use std::collections::{HashMap, HashSet, VecDeque};

use git2::Repository;

use super::Stack;

/// One branch's stack metadata: its recorded parent branch and (if known) the parent revision
/// snapshotted at track/restack time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchMetadata {
    pub parent: String,
    pub parent_revision: Option<String>,
}

/// Provider-agnostic stack metadata: a trunk set plus `branch → (parent, parent_revision)`,
/// with cosmetic per-branch extras (PR titles, stack numbers) that don't participate in the
/// graph algorithms.
pub(crate) struct StackMetadata {
    /// Trunk branch names, in provider order (first = preferred).
    pub trunks: Vec<String>,
    /// Per-branch parent metadata.
    pub parents: HashMap<String, BranchMetadata>,
    /// PR titles, keyed by branch. Empty for gh-stack (title is `None` for gh-stack nodes).
    pub pr_titles: HashMap<String, String>,
    /// Provider-assigned stack numbers, keyed by **every** member branch of a stack (not just
    /// the root) — `enumerate` prunes ghosts before BFS, so a ghost root would otherwise take
    /// the number with it. Empty for providers with no numbering concept (Graphite, Git).
    pub stack_numbers: HashMap<String, u64>,
}

/// Return all stacks present in `meta`, one per connected component, ghost branches PRUNED.
///
/// A "connected component" is the set of all non-trunk branches reachable from a single direct
/// child of a trunk branch. This is the same grouping key used by `group_by_stack`, so each
/// returned `Stack` maps one-to-one to a potential `StackGroup`.
///
/// Ghost branches — those present in metadata but whose branch ref no longer exists
/// (merged/deleted while the provider's records linger) — are dropped before the BFS so they
/// do not surface as metadata nodes in `list`/`find`. (`current` deliberately does NOT prune —
/// routing needs deleted nodes to stay visible.)
pub(crate) fn enumerate(repo: &Repository, meta: &StackMetadata) -> Vec<Stack> {
    let mut parent_map: HashMap<String, String> = meta
        .parents
        .iter()
        .map(|(branch, m)| (branch.clone(), m.parent.clone()))
        .collect();
    if parent_map.is_empty() {
        return vec![];
    }

    // Drop ghost branches: providers leave metadata rows behind after a branch is merged or
    // deleted. Filter before the BFS so orphaned subtrees are pruned consistently. Trunks are
    // not in the parent map (they are values, not keys), so they are always preserved.
    parent_map.retain(|branch, _| crate::resolve::branch_exists(repo, branch));

    if parent_map.is_empty() {
        return vec![];
    }

    let trunks: HashSet<String> = meta.trunks.iter().cloned().collect();

    let mut reverse_map: HashMap<String, Vec<String>> = HashMap::new();
    for (branch, parent) in &parent_map {
        reverse_map
            .entry(parent.clone())
            .or_default()
            .push(branch.clone());
    }

    // Root branches are direct children of a trunk.
    let mut root_branches: Vec<String> = parent_map
        .iter()
        .filter(|(_, p)| trunks.contains(*p))
        .map(|(b, _)| b.clone())
        .collect();
    root_branches.sort();

    let mut stacks: Vec<Stack> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();

    for root in root_branches {
        if visited.contains(&root) {
            continue;
        }
        let trunk = parent_map.get(&root).cloned().unwrap_or_default();

        let mut diffs: Vec<String> = Vec::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(root);
        while let Some(branch) = queue.pop_front() {
            if !visited.insert(branch.clone()) {
                continue;
            }
            if trunks.contains(&branch) {
                continue;
            }
            diffs.push(branch.clone());
            if let Some(children) = reverse_map.get(&branch) {
                let mut sorted = children.clone();
                sorted.sort();
                for child in sorted {
                    queue.push_back(child);
                }
            }
        }

        if !diffs.is_empty() {
            let current = diffs[0].clone();
            let parents: HashMap<String, String> = diffs
                .iter()
                .filter_map(|b| parent_map.get(b).map(|p| (b.clone(), p.clone())))
                .collect();
            let number = diffs
                .iter()
                .find_map(|b| meta.stack_numbers.get(b).copied());
            stacks.push(Stack {
                trunk,
                diffs,
                current,
                parents,
                number,
            });
        }
    }

    stacks
}

/// Get the stack for the worktree whose HEAD is `head_branch`, ghost branches RETAINED.
///
/// Returns `None` if the branch has no metadata row (not tracked). The returned stack includes
/// all branches reachable from the same stack root, not just the path to HEAD, so branching
/// stacks are fully represented.
pub(crate) fn current(meta: &StackMetadata, head_branch: &str) -> Option<Stack> {
    if !meta.parents.contains_key(head_branch) {
        return None;
    }

    let trunks: HashSet<String> = meta.trunks.iter().cloned().collect();

    // Walk upward from head_branch to find the trunk and collect ancestors.
    let mut walk = head_branch.to_string();
    let mut ancestors: Vec<String> = Vec::new();
    let mut upward_seen: HashSet<String> = HashSet::new();
    upward_seen.insert(walk.clone());

    let trunk = loop {
        if trunks.contains(&walk) {
            break walk.clone();
        }
        match meta.parents.get(&walk) {
            Some(entry) => {
                if !upward_seen.insert(entry.parent.clone()) {
                    // Cycle in metadata: treat current as the implicit root.
                    break walk.clone();
                }
                ancestors.push(walk.clone());
                walk = entry.parent.clone();
            }
            // No metadata — treat this branch as the implicit root.
            None => break walk.clone(),
        }
    };

    // ancestors is [head_branch, ..., bottom]; reverse for bottom → top.
    ancestors.reverse();

    // head_branch is trunk itself when there are no ancestors.
    let stack_root = ancestors.first()?.clone();

    // Build reverse map (parent → children) for BFS downward.
    let mut reverse_map: HashMap<String, Vec<String>> = HashMap::new();
    for (branch, entry) in &meta.parents {
        reverse_map
            .entry(entry.parent.clone())
            .or_default()
            .push(branch.clone());
    }

    // BFS from stack_root to collect all branches in this connected stack.
    let mut stack_branches: Vec<String> = Vec::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();
    queue.push_back(stack_root);

    while let Some(branch) = queue.pop_front() {
        if !visited.insert(branch.clone()) {
            continue;
        }
        if trunks.contains(&branch) {
            continue;
        }
        stack_branches.push(branch.clone());
        if let Some(children) = reverse_map.get(&branch) {
            for child in children {
                queue.push_back(child.clone());
            }
        }
    }

    let parents: HashMap<String, String> = stack_branches
        .iter()
        .filter_map(|b| {
            meta.parents
                .get(b)
                .map(|entry| (b.clone(), entry.parent.clone()))
        })
        .collect();
    let number = stack_branches
        .iter()
        .find_map(|b| meta.stack_numbers.get(b).copied());
    Some(Stack {
        trunk,
        diffs: stack_branches,
        current: head_branch.to_string(),
        parents,
        number,
    })
}

/// Order branch names bottom→below-head, then `head_branch`, then descendants (depth-first,
/// siblings sorted lexically), ghost branches RETAINED — the caller skips them from output but
/// walks through them so live descendants of a ghost still appear.
pub(crate) fn changeset_walk(meta: &StackMetadata, head_branch: &str) -> Vec<String> {
    let trunks: HashSet<String> = meta.trunks.iter().cloned().collect();

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
            let Some(entry) = meta.parents.get(&walk) else {
                break;
            };
            let parent = entry.parent.clone();
            if trunks.contains(&parent) {
                break;
            }
            // An untracked parent (no metadata row) is outside the stack: stop without
            // emitting it. Every walked name therefore has a metadata row.
            if !meta.parents.contains_key(&parent) {
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
    for (branch, entry) in &meta.parents {
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

    ancestors_desc
        .into_iter()
        .chain(std::iter::once(head_branch.to_string()))
        .chain(descendants)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_workon_fixture::prelude::*;

    fn meta(trunk: &str, parents: &[(&str, &str)], numbers: &[(&str, u64)]) -> StackMetadata {
        StackMetadata {
            trunks: vec![trunk.to_string()],
            parents: parents
                .iter()
                .map(|(branch, parent)| {
                    (
                        branch.to_string(),
                        BranchMetadata {
                            parent: parent.to_string(),
                            parent_revision: None,
                        },
                    )
                })
                .collect(),
            pr_titles: HashMap::new(),
            stack_numbers: numbers
                .iter()
                .map(|(branch, n)| (branch.to_string(), *n))
                .collect(),
        }
    }

    /// `Stack::number` is display metadata keyed by any member branch, not just the root —
    /// `enumerate` must find it even when only a non-root diff carries the entry.
    #[test]
    fn enumerate_sets_stack_number_from_any_member_branch() {
        let fixture = FixtureBuilder::new()
            .branch("feat-a")
            .branch("feat-b")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = meta(
            "main",
            &[("feat-a", "main"), ("feat-b", "feat-a")],
            &[("feat-b", 12)],
        );

        let stacks = enumerate(repo, &meta);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].number, Some(12));
    }

    #[test]
    fn enumerate_leaves_number_none_when_unnumbered() {
        let fixture = FixtureBuilder::new().branch("feat-a").build().unwrap();
        let repo = fixture.repo().unwrap();

        let meta = meta("main", &[("feat-a", "main")], &[]);

        let stacks = enumerate(repo, &meta);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].number, None);
    }

    #[test]
    fn current_sets_stack_number_from_any_member_branch() {
        let meta = meta(
            "main",
            &[("feat-a", "main"), ("feat-b", "feat-a")],
            &[("feat-a", 7)],
        );

        let stack = current(&meta, "feat-b").expect("feat-b is tracked");
        assert_eq!(stack.number, Some(7));
    }
}
