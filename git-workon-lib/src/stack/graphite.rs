//! Graphite (`gt`) stack detection.
//!
//! Stack metadata is read without invoking `gt`. Two storage formats are supported:
//!
//! - **gt ≥ 1.8** — SQLite database at `.graphite_metadata.db` in the git common dir.
//!   Table `branch_metadata(branch_name, parent_branch_name, ...)`.
//! - **gt < 1.8** — `refs/branch-metadata/<branch>` git refs: blobs containing JSON
//!   `{ "parentBranchName": "step-1", "parentBranchRevision": "<sha>" }`.
//!
//! The database format is tried first; refs are the fallback for older installations.
//! Trunk names come from `.graphite_repo_config` in both cases.
//!
//! `gt track` is invoked only when registering a new branch after `workon new` creates a
//! worktree forked off an existing stack-worktree branch.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::{Command, Stdio};

use git2::Repository;
use rusqlite::OpenFlags;
use serde_json::Value;

use super::Stack;
use crate::error::StackError;

/// Returns `true` if the `gt` binary is on PATH.
pub fn detect_gt() -> bool {
    Command::new("gt")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Returns `true` if the repository has been Graphite-initialized.
///
/// Checks for either the SQLite metadata DB (gt ≥ 1.8) or the legacy config file (gt < 1.8).
/// Uses `repo.path()`, which returns the shared git dir for both bare repos and worktrees.
pub fn is_graphite_repo(repo: &Repository) -> bool {
    let git_dir = repo.path();
    git_dir.join(".graphite_metadata.db").exists() || git_dir.join(".graphite_repo_config").exists()
}

/// Return the first trunk branch name from `.graphite_repo_config`, or `None` if the
/// file is missing, unparseable, or contains no trunk entries.
///
/// Unlike [`read_trunks`], this never falls back to a hardcoded `"main"` — `None`
/// means "unknown", which callers can use to omit `--parent` and let `gt` infer.
pub fn graphite_trunk(repo: &Repository) -> Option<String> {
    let path = repo.path().join(".graphite_repo_config");
    let content = std::fs::read_to_string(&path).ok()?;
    let json = serde_json::from_str::<Value>(&content).ok()?;
    if let Some(trunks) = json.get("trunks").and_then(|t| t.as_array()) {
        let first = trunks
            .iter()
            .find_map(|t| t.get("name").and_then(|n| n.as_str()))
            .map(String::from);
        if first.is_some() {
            return first;
        }
    }
    json.get("trunk").and_then(|t| t.as_str()).map(String::from)
}

/// Read trunk branch names from `.graphite_repo_config`.
///
/// Falls back to `["main"]` if the file is missing or unparseable.
fn read_trunks(repo: &Repository) -> Vec<String> {
    let path = repo.path().join(".graphite_repo_config");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return vec!["main".to_string()],
    };
    let Ok(json) = serde_json::from_str::<Value>(&content) else {
        return vec!["main".to_string()];
    };
    if let Some(trunks) = json.get("trunks").and_then(|t| t.as_array()) {
        let names: Vec<String> = trunks
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .map(String::from)
            .collect();
        if !names.is_empty() {
            return names;
        }
    }
    if let Some(trunk) = json.get("trunk").and_then(|t| t.as_str()) {
        return vec![trunk.to_string()];
    }
    vec!["main".to_string()]
}

/// Build a `branch → parent_branch` map.
///
/// Tries the SQLite database (gt ≥ 1.8) first; falls back to git refs (gt < 1.8).
fn build_parent_map(repo: &Repository) -> Result<HashMap<String, String>, StackError> {
    let db_path = repo.path().join(".graphite_metadata.db");
    if db_path.exists() {
        if let Ok(map) = build_parent_map_from_sqlite(&db_path) {
            return Ok(map);
        }
    }
    build_parent_map_from_refs(repo)
}

/// Read `branch → parent_branch` from the gt ≥ 1.8 SQLite database.
fn build_parent_map_from_sqlite(db_path: &Path) -> Result<HashMap<String, String>, StackError> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| StackError::GtParseFailed {
        message: format!("failed to open .graphite_metadata.db: {e}"),
    })?;

    let mut stmt = conn
        .prepare(
            "SELECT branch_name, parent_branch_name \
             FROM branch_metadata \
             WHERE parent_branch_name IS NOT NULL AND parent_branch_name != ''",
        )
        .map_err(|e| StackError::GtParseFailed {
            message: format!("failed to query .graphite_metadata.db: {e}"),
        })?;

    let mut map = HashMap::new();
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| StackError::GtParseFailed {
            message: format!("failed to read .graphite_metadata.db rows: {e}"),
        })?;
    for row in rows.flatten() {
        map.insert(row.0, row.1);
    }
    Ok(map)
}

/// Read `branch → parent_branch` from gt < 1.8 `refs/branch-metadata/*` git refs.
fn build_parent_map_from_refs(repo: &Repository) -> Result<HashMap<String, String>, StackError> {
    let mut map = HashMap::new();
    let references = repo
        .references_glob("refs/branch-metadata/*")
        .map_err(|e| StackError::GtParseFailed {
            message: format!("failed to list branch-metadata refs: {e}"),
        })?;
    for reference in references {
        let reference = reference.map_err(|e| StackError::GtParseFailed {
            message: format!("failed to read branch-metadata ref: {e}"),
        })?;
        let Ok(refname) = reference.name() else {
            continue;
        };
        let Some(branch) = refname.strip_prefix("refs/branch-metadata/") else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        let Ok(object) = reference.peel(git2::ObjectType::Blob) else {
            continue;
        };
        let Ok(blob) = object.into_blob() else {
            continue;
        };
        let Ok(json) = serde_json::from_slice::<Value>(blob.content()) else {
            continue;
        };
        if let Some(parent) = json.get("parentBranchName").and_then(|p| p.as_str()) {
            map.insert(branch.to_string(), parent.to_string());
        }
    }
    Ok(map)
}

/// Return all stacks present in `refs/branch-metadata/*`, one per connected component.
///
/// A "connected component" is the set of all non-trunk branches reachable from a single
/// direct child of a trunk branch. This is the same grouping key used by `group_by_stack`,
/// so each returned `Stack` maps one-to-one to a potential `StackGroup`.
///
/// Used by the `list` command to surface stacks that have no checked-out worktrees yet.
pub fn enumerate_stacks(repo: &Repository) -> Result<Vec<Stack>, StackError> {
    let parent_map = build_parent_map(repo)?;
    if parent_map.is_empty() {
        return Ok(vec![]);
    }

    let trunks: HashSet<String> = read_trunks(repo).into_iter().collect();

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
        .filter_map(|(b, p)| trunks.contains(p).then(|| b.clone()))
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
            stacks.push(Stack {
                trunk,
                diffs,
                current,
            });
        }
    }

    Ok(stacks)
}

/// Get the stack for the worktree whose HEAD is `head_branch`.
///
/// Returns `None` if the branch has no `refs/branch-metadata/` entry (not Graphite-tracked).
/// The returned stack includes all branches reachable from the same stack root, not just the
/// path to HEAD, so branching stacks are fully represented.
pub fn current_stack(repo: &Repository, head_branch: &str) -> Result<Option<Stack>, StackError> {
    let parent_map = build_parent_map(repo)?;

    if !parent_map.contains_key(head_branch) {
        return Ok(None);
    }

    let trunks: HashSet<String> = read_trunks(repo).into_iter().collect();

    // Walk upward from head_branch to find the trunk and collect ancestors.
    let mut walk = head_branch.to_string();
    let mut ancestors: Vec<String> = Vec::new();
    let mut upward_seen: HashSet<String> = HashSet::new();
    upward_seen.insert(walk.clone());

    let trunk = loop {
        if trunks.contains(&walk) {
            break walk.clone();
        }
        match parent_map.get(&walk) {
            Some(parent) => {
                if !upward_seen.insert(parent.clone()) {
                    // Cycle in branch-metadata: treat current as the implicit root.
                    break walk.clone();
                }
                ancestors.push(walk.clone());
                walk = parent.clone();
            }
            // No metadata — treat this branch as the implicit root.
            None => break walk.clone(),
        }
    };

    // ancestors is [head_branch, ..., bottom]; reverse for bottom → top.
    ancestors.reverse();

    let stack_root = match ancestors.first() {
        Some(r) => r.clone(),
        None => return Ok(None), // head_branch is trunk itself
    };

    // Build reverse map (parent → children) for BFS downward.
    let mut reverse_map: HashMap<String, Vec<String>> = HashMap::new();
    for (branch, parent) in &parent_map {
        reverse_map
            .entry(parent.clone())
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

    Ok(Some(Stack {
        trunk,
        diffs: stack_branches,
        current: head_branch.to_string(),
    }))
}
