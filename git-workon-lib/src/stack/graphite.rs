//! Graphite (`gt`) stack detection.
//!
//! Stack metadata is read without invoking `gt`. Two storage formats are supported:
//!
//! - **gt ≥ 1.8** — SQLite database at `.graphite_metadata.db` in the git common dir.
//!   Table `branch_metadata(branch_name, parent_branch_name, ...)`.
//! - **gt < 1.8** — `refs/branch-metadata/<branch>` git refs: blobs containing JSON
//!   `{ "parentBranchName": "step-1", "parentBranchRevision": "<sha>" }`.
//!
//! The database format is tried first; refs are the fallback for older installations, but only
//! when the database file is absent — a present-but-unreadable database is a hard error, never
//! a silent fallback to (possibly stale) refs metadata. Trunk names come from
//! `.graphite_repo_config` in both cases.
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
/// Uses `repo.commondir()`, which resolves to the shared git dir for both bare repos and
/// linked worktrees (`repo.path()` would instead return the worktree's private gitdir).
pub fn is_graphite_repo(repo: &Repository) -> bool {
    let git_dir = repo.commondir();
    git_dir.join(".graphite_metadata.db").exists() || git_dir.join(".graphite_repo_config").exists()
}

/// Return the first trunk branch name from `.graphite_repo_config`, or `None` if the
/// file is missing, unparseable, or contains no trunk entries.
///
/// Unlike [`read_trunks`], this never falls back to a hardcoded `"main"` — `None`
/// means "unknown", which callers can use to omit `--parent` and let `gt` infer.
pub fn graphite_trunk(repo: &Repository) -> Option<String> {
    let path = repo.commondir().join(".graphite_repo_config");
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
    let path = repo.commondir().join(".graphite_repo_config");
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

/// One branch's Graphite metadata: its recorded parent branch and (if known) the parent
/// revision snapshotted at `gt track`/`gt restack` time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchMetadata {
    pub parent: String,
    pub parent_revision: Option<String>,
}

/// Read per-branch Graphite metadata (parent branch + recorded parent revision).
///
/// Tries the SQLite database (gt ≥ 1.8) first; falls back to git refs (gt < 1.8) only when
/// the database file is **absent**. A present-but-unreadable/corrupt database is a hard
/// error (`StackError::GtParseFailed`) — it never silently falls back to (possibly stale)
/// refs metadata.
pub(crate) fn read_branch_metadata(
    repo: &Repository,
) -> Result<HashMap<String, BranchMetadata>, StackError> {
    let db_path = repo.commondir().join(".graphite_metadata.db");
    if db_path.exists() {
        read_branch_metadata_from_sqlite(&db_path)
    } else {
        read_branch_metadata_from_refs(repo)
    }
}

/// Read branch metadata from the gt ≥ 1.8 SQLite database.
fn read_branch_metadata_from_sqlite(
    db_path: &Path,
) -> Result<HashMap<String, BranchMetadata>, StackError> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| StackError::GtParseFailed {
        message: format!("failed to open .graphite_metadata.db: {e}"),
    })?;

    let mut stmt = conn
        .prepare(
            "SELECT branch_name, parent_branch_name, parent_branch_revision \
             FROM branch_metadata \
             WHERE parent_branch_name IS NOT NULL AND parent_branch_name != ''",
        )
        .map_err(|e| StackError::GtParseFailed {
            message: format!("failed to query .graphite_metadata.db: {e}"),
        })?;

    let mut map = HashMap::new();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|e| StackError::GtParseFailed {
            message: format!("failed to read .graphite_metadata.db rows: {e}"),
        })?;
    for row in rows {
        let (branch, parent, parent_revision) = row.map_err(|e| StackError::GtParseFailed {
            message: format!("failed to read .graphite_metadata.db row: {e}"),
        })?;
        let parent_revision = parent_revision.filter(|r| !r.is_empty());
        map.insert(
            branch,
            BranchMetadata {
                parent,
                parent_revision,
            },
        );
    }
    Ok(map)
}

/// Read branch metadata from gt < 1.8 `refs/branch-metadata/*` git refs.
fn read_branch_metadata_from_refs(
    repo: &Repository,
) -> Result<HashMap<String, BranchMetadata>, StackError> {
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
            let parent_revision = json
                .get("parentBranchRevision")
                .and_then(|r| r.as_str())
                .map(String::from)
                .filter(|r| !r.is_empty());
            map.insert(
                branch.to_string(),
                BranchMetadata {
                    parent: parent.to_string(),
                    parent_revision,
                },
            );
        }
    }
    Ok(map)
}

/// Build a `branch → parent_branch` map (thin wrapper over [`read_branch_metadata`] for
/// callers that only need parent-branch topology, not recorded revisions).
fn build_parent_map(repo: &Repository) -> Result<HashMap<String, String>, StackError> {
    Ok(read_branch_metadata(repo)?
        .into_iter()
        .map(|(branch, metadata)| (branch, metadata.parent))
        .collect())
}

/// Read PR titles from `<git-common-dir>/.graphite_pr_info`, keyed by branch name
/// (`prInfos[].headRefName`).
///
/// Titles are cosmetic: a missing or corrupt file, or entries missing `headRefName`/`title`,
/// yield an empty map or are silently skipped rather than surfacing as an error.
///
/// Not yet called from production code — `assemble_changesets` (m1-changeset-assembly) is its
/// first consumer. Exercised by unit tests below in the meantime.
#[allow(dead_code)]
pub(crate) fn read_pr_titles(repo: &Repository) -> HashMap<String, String> {
    let path = repo.commondir().join(".graphite_pr_info");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let Ok(json) = serde_json::from_str::<Value>(&content) else {
        return HashMap::new();
    };
    let Some(pr_infos) = json.get("prInfos").and_then(|p| p.as_array()) else {
        return HashMap::new();
    };
    pr_infos
        .iter()
        .filter_map(|entry| {
            let branch = entry.get("headRefName").and_then(|v| v.as_str())?;
            let title = entry.get("title").and_then(|v| v.as_str())?;
            Some((branch.to_string(), title.to_string()))
        })
        .collect()
}

/// Return all stacks present in `refs/branch-metadata/*`, one per connected component.
///
/// A "connected component" is the set of all non-trunk branches reachable from a single
/// direct child of a trunk branch. This is the same grouping key used by `group_by_stack`,
/// so each returned `Stack` maps one-to-one to a potential `StackGroup`.
///
/// Ghost branches — those present in Graphite metadata but whose branch ref no longer exists
/// (merged/deleted while Graphite's records linger) — are dropped before the BFS so they
/// do not surface as `◯` metadata nodes in `list`/`find`.
///
/// Used by the `list` command to surface stacks that have no checked-out worktrees yet.
pub fn enumerate_stacks(repo: &Repository) -> Result<Vec<Stack>, StackError> {
    let mut parent_map = build_parent_map(repo)?;
    if parent_map.is_empty() {
        return Ok(vec![]);
    }

    // Drop ghost branches: Graphite leaves metadata rows behind after a branch is merged or
    // deleted. Filter before the BFS so orphaned subtrees are pruned consistently. Trunks are
    // not in the parent map (they are values, not keys), so they are always preserved.
    parent_map.retain(|branch, _| crate::resolve::branch_exists(repo, branch));

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
            let parents: std::collections::HashMap<String, String> = diffs
                .iter()
                .filter_map(|b| parent_map.get(b).map(|p| (b.clone(), p.clone())))
                .collect();
            stacks.push(Stack {
                trunk,
                diffs,
                current,
                parents,
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

    let parents: std::collections::HashMap<String, String> = stack_branches
        .iter()
        .filter_map(|b| parent_map.get(b).map(|p| (b.clone(), p.clone())))
        .collect();
    Ok(Some(Stack {
        trunk,
        diffs: stack_branches,
        current: head_branch.to_string(),
        parents,
    }))
}

// `read_branch_metadata`'s `parent_revision` field is not (yet) reachable through the public
// `Stack`/`current_stack`/`enumerate_stacks` API — that plumbing lands in the m1-changeset-assembly
// changeset. These unit tests exercise it directly via the fixture crate (a dev-dependency of
// this lib), covering both metadata formats.
#[cfg(test)]
mod tests {
    use super::*;
    use git_workon_fixture::prelude::*;

    #[test]
    fn read_branch_metadata_resolves_live_parent_revision_refs() {
        let fixture = FixtureBuilder::new()
            .branch_metadata("feat-a", "main")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let metadata = read_branch_metadata(repo).unwrap();
        let entry = metadata.get("feat-a").expect("feat-a metadata present");
        assert_eq!(entry.parent, "main");

        let main_tip = repo
            .find_branch("main", git2::BranchType::Local)
            .unwrap()
            .get()
            .target()
            .unwrap();
        assert_eq!(
            entry.parent_revision.as_deref(),
            Some(main_tip.to_string().as_str())
        );
    }

    #[test]
    fn read_branch_metadata_resolves_live_parent_revision_sqlite() {
        let fixture = FixtureBuilder::new()
            .metadata_format(MetadataFormat::Sqlite)
            .branch_metadata("feat-a", "main")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let metadata = read_branch_metadata(repo).unwrap();
        let entry = metadata.get("feat-a").expect("feat-a metadata present");
        assert_eq!(entry.parent, "main");

        let main_tip = repo
            .find_branch("main", git2::BranchType::Local)
            .unwrap()
            .get()
            .target()
            .unwrap();
        assert_eq!(
            entry.parent_revision.as_deref(),
            Some(main_tip.to_string().as_str())
        );
    }

    #[test]
    fn read_branch_metadata_preserves_verbatim_stale_parent_revision_refs() {
        let fixture = FixtureBuilder::new()
            .branch_metadata_at(
                "feat-a",
                "main",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "cafebabecafebabecafebabecafebabecafebabe",
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let metadata = read_branch_metadata(repo).unwrap();
        let entry = metadata.get("feat-a").expect("feat-a metadata present");
        assert_eq!(
            entry.parent_revision.as_deref(),
            Some("cafebabecafebabecafebabecafebabecafebabe")
        );
    }

    #[test]
    fn read_branch_metadata_preserves_verbatim_stale_parent_revision_sqlite() {
        let fixture = FixtureBuilder::new()
            .metadata_format(MetadataFormat::Sqlite)
            .branch_metadata_at(
                "feat-a",
                "main",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "cafebabecafebabecafebabecafebabecafebabe",
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let metadata = read_branch_metadata(repo).unwrap();
        let entry = metadata.get("feat-a").expect("feat-a metadata present");
        assert_eq!(
            entry.parent_revision.as_deref(),
            Some("cafebabecafebabecafebabecafebabecafebabe")
        );
    }

    #[test]
    fn read_branch_metadata_treats_empty_parent_revision_as_none_refs() {
        let fixture = FixtureBuilder::new()
            .branch_metadata_at("feat-a", "main", "", "")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let metadata = read_branch_metadata(repo).unwrap();
        let entry = metadata.get("feat-a").expect("feat-a metadata present");
        assert_eq!(entry.parent_revision, None);
    }

    #[test]
    fn read_branch_metadata_treats_empty_parent_revision_as_none_sqlite() {
        let fixture = FixtureBuilder::new()
            .metadata_format(MetadataFormat::Sqlite)
            .branch_metadata_at("feat-a", "main", "", "")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let metadata = read_branch_metadata(repo).unwrap();
        let entry = metadata.get("feat-a").expect("feat-a metadata present");
        assert_eq!(entry.parent_revision, None);
    }

    #[test]
    fn read_pr_titles_maps_branch_to_title() {
        let fixture = FixtureBuilder::new()
            .graphite_pr_info("feat-a", 42, "Add feature A")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let titles = read_pr_titles(repo);
        assert_eq!(
            titles.get("feat-a").map(String::as_str),
            Some("Add feature A")
        );
    }

    #[test]
    fn read_pr_titles_empty_when_file_missing() {
        let fixture = FixtureBuilder::new().build().unwrap();
        let repo = fixture.repo().unwrap();

        assert!(read_pr_titles(repo).is_empty());
    }
}
