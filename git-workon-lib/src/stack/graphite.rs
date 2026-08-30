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

use std::collections::HashMap;
use std::path::Path;

use git2::Repository;
use rusqlite::OpenFlags;
use serde_json::Value;

use super::metadata::{self, BranchMetadata, StackMetadata};
use super::Stack;
use crate::error::StackError;

/// Returns `true` if the `gt` binary is on PATH.
///
/// A filesystem scan of `PATH`, deliberately NOT `gt --version`: `gt` is a Node CLI whose
/// interpreter startup costs ~300-400ms, and this runs on every stack-model detection (CLI
/// routing and review startup). Presence-on-PATH is the documented contract; whether the
/// binary actually executes is the invoking call site's problem (`gt track` reports its own
/// failure).
pub fn detect_gt() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable_file(&dir.join("gt")))
}

#[cfg(unix)]
fn is_executable_file(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(candidate)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(candidate: &Path) -> bool {
    candidate.is_file()
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
pub(crate) fn read_trunks(repo: &Repository) -> Vec<String> {
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

/// Read PR titles from `<git-common-dir>/.graphite_pr_info`, keyed by branch name
/// (`prInfos[].headRefName`).
///
/// Titles are cosmetic: a missing or corrupt file, or entries missing `headRefName`/`title`,
/// yield an empty map or are silently skipped rather than surfacing as an error.
///
/// Used by [`crate::assemble_changesets`] to label Graphite stack nodes with their PR title.
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

/// Read Graphite's stack metadata into provider-agnostic [`StackMetadata`].
///
/// Trunks come from [`read_trunks`]; per-branch parent metadata from [`read_branch_metadata`];
/// PR titles from [`read_pr_titles`]. Graphite has no stack-numbering concept, so
/// `stack_numbers` is always empty.
pub(crate) fn read_metadata(repo: &Repository) -> Result<StackMetadata, StackError> {
    let parents = read_branch_metadata(repo)?;
    Ok(StackMetadata {
        trunks: read_trunks(repo),
        parents,
        pr_titles: read_pr_titles(repo),
        stack_numbers: HashMap::new(),
    })
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
    Ok(metadata::enumerate(repo, &read_metadata(repo)?))
}

/// Get the stack for the worktree whose HEAD is `head_branch`.
///
/// Returns `None` if the branch has no `refs/branch-metadata/` entry (not Graphite-tracked).
/// The returned stack includes all branches reachable from the same stack root, not just the
/// path to HEAD, so branching stacks are fully represented. Ghost branches are retained (not
/// pruned) — see [`metadata::current`].
pub fn current_stack(repo: &Repository, head_branch: &str) -> Result<Option<Stack>, StackError> {
    Ok(metadata::current(&read_metadata(repo)?, head_branch))
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
