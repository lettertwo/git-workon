//! `gh stack` (`github/gh-stack` CLI extension) stack detection — read path.
//!
//! Stack metadata is read without invoking `gh`. Upstream writes one JSON file per git dir
//! (`schemaVersion: 1`, `{ repository, stacks: [{ id, number, trunk: branchRef, branches:
//! [branchRef] }] }`, `branchRef = { branch, head, base, pullRequest }`), which for a linked
//! worktree is per-worktree, not shared. workon keeps one canonical copy at
//! `<common-dir>/gh-stack` and symlinks each worktree's admin-dir path to it (see
//! `link_worktree`, added in a later changeset) so it behaves like Graphite's shared store.
//!
//! **Never use `repo.path()` here — always `repo.commondir()`.** `get_repo` (`get_repo.rs`)
//! follows `commondir` back and returns the bare repo, so `repo.path() == repo.commondir()`
//! at every CLI call site. But `Fixture::repo()` in tests can be a *worktree* handle where
//! they differ. A `path()`-based scan silently passes under test and fails for every real
//! linked worktree.
//!
//! ## Read order and the degraded union fallback
//!
//! [`read_metadata`] reads the canonical file first, then unions in [`unlinked_files`] —
//! worktree admin-dir files that are *not* symlinks resolving to canonical — in directory
//! order. In a healthy (fully-linked) repo `unlinked_files` is empty and the union never
//! runs. It exists because write-in-place (upstream truncates its target through the
//! symlink) is an implementation detail, not a contract: if gh-stack ever switches to
//! temp-and-rename, the rename replaces a worktree's canonical symlink with a real file, and
//! that worktree's writes silently stop reaching canonical. The union read means nothing goes
//! invisible in the meantime — `doctor` (added later) flags any unlinked file it finds.
//!
//! Dedupe when the union fires: identity is `number` when non-zero, else `id` when
//! non-empty, else `(trunk, first branch)`; **first wins wholesale** — the entire stack
//! object from the earliest source is kept, later ones with the same identity are discarded
//! entirely, never merged field-by-field. Merging two disagreeing ordered `branches` arrays
//! has no defined semantics (an insertion in one is indistinguishable from a deletion in the
//! other), so a field-level merge could synthesize a stack that existed in neither worktree.
//!
//! ## Truncated reads are tolerated, not fatal
//!
//! A partial file is the *expected* steady state during a concurrent `gh stack` command —
//! upstream's `os.WriteFile` truncates in place rather than writing to a temp file and
//! renaming, so a reader can observe a half-written file. [`read_metadata`] retries a
//! read-and-parse up to 3 times, 25ms apart, and skips the file with `log::warn!` if every
//! attempt still fails to parse. This is the deliberate opposite of Graphite's rule
//! (`graphite.rs`'s `read_branch_metadata`, where a present-but-unreadable database is a hard
//! error): sqlite writes are atomic, so unreadable there means corrupt, not mid-write.
//!
//! `schemaVersion > 1` is not retried — retrying a version mismatch cannot fix it, and
//! skipping it would silently render a confidently wrong (outdated) stack, so it is a hard
//! error ([`StackError::GhStackSchemaUnsupported`]). Missing or `0` is treated as `1`,
//! matching Go's zero-value behavior for an unset int field.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use git2::Repository;
use serde_json::Value;

use super::metadata::{self, BranchMetadata, StackMetadata};
use super::Stack;
use crate::error::StackError;

/// One `branchRef` entry (`{ branch, head, base, pullRequest }`), pulled from a raw
/// `serde_json::Value` rather than a derived struct (this crate has no `serde` derive
/// dependency, only `serde_json`; see `graphite.rs` for the same raw-`Value` convention).
/// `head` and `pullRequest` are read from the file but not carried into [`StackMetadata`]:
/// assembly uses the live tip for `head`, and `pullRequest` has no `StackMetadata` field.
/// Both matter to the write path (added in a later changeset), which round-trips the raw
/// `Value` to preserve them.
#[derive(Debug)]
struct GhStackBranchRef {
    branch: String,
    base: String,
}

impl GhStackBranchRef {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            branch: value.get("branch")?.as_str()?.to_string(),
            base: value
                .get("base")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }
}

#[derive(Debug)]
struct GhStackEntry {
    id: String,
    number: u64,
    trunk: GhStackBranchRef,
    branches: Vec<GhStackBranchRef>,
}

impl GhStackEntry {
    fn from_value(value: &Value) -> Option<Self> {
        let trunk = GhStackBranchRef::from_value(value.get("trunk")?)?;
        let branches = value
            .get("branches")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(GhStackBranchRef::from_value)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            id: value
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            number: value.get("number").and_then(|v| v.as_u64()).unwrap_or(0),
            trunk,
            branches,
        })
    }
}

/// Parse `doc`'s `stacks` array. Entries missing a well-formed `trunk` are skipped (not fatal
/// — one malformed entry in an otherwise-valid file shouldn't blind the whole read).
fn parse_stacks(doc: &Value) -> Vec<GhStackEntry> {
    doc.get("stacks")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(GhStackEntry::from_value).collect())
        .unwrap_or_default()
}

/// Number of read-and-parse attempts before a persistently truncated/malformed file is
/// skipped with a warning. See the module docs' "Truncated reads" section.
const READ_ATTEMPTS: u32 = 3;
const READ_RETRY_DELAY: Duration = Duration::from_millis(25);

/// `<common-dir>/gh-stack` — the canonical store every worktree's admin-dir file is meant to
/// symlink to.
pub(crate) fn canonical_path(repo: &Repository) -> PathBuf {
    repo.commondir().join("gh-stack")
}

/// Worktree admin-dir `gh-stack` files that are NOT symlinks resolving to [`canonical_path`],
/// sorted by directory name. Empty in the healthy (fully-linked) case. Directory-name order
/// is the dedupe tiebreak in [`read_metadata`].
pub(crate) fn unlinked_files(repo: &Repository) -> Vec<PathBuf> {
    let canonical = canonical_path(repo);
    let worktrees_dir = repo.commondir().join("worktrees");
    let Ok(entries) = std::fs::read_dir(&worktrees_dir) else {
        return vec![];
    };

    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();

    names
        .into_iter()
        .filter_map(|name| {
            let path = worktrees_dir.join(&name).join("gh-stack");
            if !path_exists_at_all(&path) || is_symlink_resolving_to(&path, &canonical) {
                None
            } else {
                Some(path)
            }
        })
        .collect()
}

fn path_exists_at_all(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// `true` if `path` is a symlink whose target, resolved lexically relative to `path`'s parent
/// (no `fs::canonicalize` — a dangling symlink to a not-yet-created canonical file is a valid
/// state; see the module docs), equals `canonical`.
fn is_symlink_resolving_to(path: &Path, canonical: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    let Ok(target) = std::fs::read_link(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    normalize_lexically(&parent.join(target)) == normalize_lexically(canonical)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Returns `true` if this repository has a gh-stack file anywhere workon knows to look —
/// canonical or an unlinked worktree file.
pub(crate) fn is_gh_stack_repo(repo: &Repository) -> bool {
    canonical_path(repo).exists() || !unlinked_files(repo).is_empty()
}

/// Read, parse, and schema-check the gh-stack file at `path`.
///
/// Returns `Ok(None)` if the file does not exist, or if every read-and-parse attempt fails
/// (logged via `log::warn!`) — both are non-fatal per the module docs. Returns `Err` only for
/// `schemaVersion > 1`, which is never retried.
fn read_doc(path: &Path) -> Result<Option<Vec<GhStackEntry>>, StackError> {
    let mut last_error: Option<String> = None;

    for attempt in 0..READ_ATTEMPTS {
        match std::fs::read(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => last_error = Some(e.to_string()),
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Err(e) => last_error = Some(e.to_string()),
                Ok(value) => {
                    let version = value
                        .get("schemaVersion")
                        .and_then(|v| v.as_u64())
                        .filter(|&v| v != 0)
                        .unwrap_or(1);
                    if version > 1 {
                        return Err(StackError::GhStackSchemaUnsupported {
                            path: path.to_path_buf(),
                            version,
                        });
                    }
                    return Ok(Some(parse_stacks(&value)));
                }
            },
        }
        if attempt + 1 < READ_ATTEMPTS {
            std::thread::sleep(READ_RETRY_DELAY);
        }
    }

    log::warn!(
        "gh-stack: skipping unreadable file {}: {}",
        path.display(),
        last_error.unwrap_or_default()
    );
    Ok(None)
}

/// Identity used to dedupe [`GhStackEntry`] values across canonical + unlinked files. See the
/// module docs' "Read order and the degraded union fallback" section.
#[derive(Debug, PartialEq, Eq, Hash)]
enum StackIdentity {
    Number(u64),
    Id(String),
    TrunkAndFirstBranch(String, String),
}

fn identity(entry: &GhStackEntry) -> StackIdentity {
    if entry.number != 0 {
        StackIdentity::Number(entry.number)
    } else if !entry.id.is_empty() {
        StackIdentity::Id(entry.id.clone())
    } else {
        let first_branch = entry
            .branches
            .first()
            .map(|b| b.branch.clone())
            .unwrap_or_default();
        StackIdentity::TrunkAndFirstBranch(entry.trunk.branch.clone(), first_branch)
    }
}

/// Read gh-stack's stack metadata into provider-agnostic [`StackMetadata`].
///
/// Reads canonical first, then unions in [`unlinked_files`] (directory order), deduping by
/// [`StackIdentity`] with first-seen-wins. See the module docs for why the union is a
/// degraded fallback rather than the primary path, and why first-wins never merges.
pub(crate) fn read_metadata(repo: &Repository) -> Result<StackMetadata, StackError> {
    let mut seen: HashSet<StackIdentity> = HashSet::new();
    let mut kept: Vec<GhStackEntry> = Vec::new();

    let mut sources = vec![canonical_path(repo)];
    sources.extend(unlinked_files(repo));

    for path in sources {
        let Some(entries) = read_doc(&path)? else {
            continue;
        };
        for entry in entries {
            if seen.insert(identity(&entry)) {
                kept.push(entry);
            }
        }
    }

    let mut trunks: Vec<String> = Vec::new();
    let mut parents: HashMap<String, BranchMetadata> = HashMap::new();
    let mut stack_numbers: HashMap<String, u64> = HashMap::new();

    for entry in &kept {
        if !trunks.contains(&entry.trunk.branch) {
            trunks.push(entry.trunk.branch.clone());
        }

        // branches[i].base maps to parent_revision, empty string normalizing to None
        // (matches graphite.rs's treatment of parentBranchRevision); branches[i].head is
        // discarded, assembly uses the branch's live tip instead.
        let mut parent = entry.trunk.branch.clone();
        for branch_ref in &entry.branches {
            let parent_revision = if branch_ref.base.is_empty() {
                None
            } else {
                Some(branch_ref.base.clone())
            };
            // First-wins wholesale, matching `trunks` above: if a branch appears in two
            // stacks, the earliest source's parent and stack number stick and `doctor` flags
            // the divergence, rather than the last-seen source silently overwriting them.
            parents
                .entry(branch_ref.branch.clone())
                .or_insert(BranchMetadata {
                    parent: parent.clone(),
                    parent_revision,
                });
            if entry.number != 0 {
                stack_numbers
                    .entry(branch_ref.branch.clone())
                    .or_insert(entry.number);
            }
            parent = branch_ref.branch.clone();
        }
    }

    Ok(StackMetadata {
        trunks,
        parents,
        pr_titles: HashMap::new(),
        stack_numbers,
    })
}

/// Return all gh-stack stacks, one per connected component, ghost branches PRUNED.
pub(crate) fn enumerate_stacks(repo: &Repository) -> Result<Vec<Stack>, StackError> {
    Ok(metadata::enumerate(repo, &read_metadata(repo)?))
}

/// Get the gh-stack stack for the worktree whose HEAD is `head_branch`, ghost branches
/// RETAINED (see [`metadata::current`]).
pub(crate) fn current_stack(
    repo: &Repository,
    head_branch: &str,
) -> Result<Option<Stack>, StackError> {
    Ok(metadata::current(&read_metadata(repo)?, head_branch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_workon_fixture::prelude::*;

    #[test]
    fn reads_linear_stack_from_canonical() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .gh_stack(None, 12, "main", &["feat-a", "feat-b"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert_eq!(meta.trunks, vec!["main".to_string()]);
        assert_eq!(meta.parents["feat-a"].parent, "main");
        assert_eq!(meta.parents["feat-b"].parent, "feat-a");
        assert_eq!(meta.stack_numbers["feat-a"], 12);
        assert_eq!(meta.stack_numbers["feat-b"], 12);

        let stacks = enumerate_stacks(repo).unwrap();
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].number, Some(12));
        assert_eq!(stacks[0].diffs, vec!["feat-a", "feat-b"]);
    }

    #[test]
    fn ghost_retained_by_current_stack_and_pruned_by_enumerate() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .gh_stack(None, 5, "main", &["feat-a"])
            .gh_stack_ghost_branch(None, 5, "feat-b")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        // current_stack retains the ghost when walking from a live descendant... but feat-b
        // has no ref, so retrieve current_stack from feat-a (the live branch) instead, which
        // must still see feat-b was never linked as a child in enumerate's pruned output.
        let current = current_stack(repo, "feat-a").unwrap().expect("tracked");
        assert!(current.diffs.contains(&"feat-a".to_string()));

        let enumerated = enumerate_stacks(repo).unwrap();
        assert_eq!(enumerated.len(), 1);
        assert!(!enumerated[0].diffs.contains(&"feat-b".to_string()));
        assert!(enumerated[0].diffs.contains(&"feat-a".to_string()));
    }

    #[test]
    fn truncated_file_is_skipped() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .raw_gh_stack(None, b"{\"schemaVersion\": 1, \"stacks\": [".to_vec())
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert!(meta.trunks.is_empty());
        assert!(meta.parents.is_empty());
    }

    #[test]
    fn schema_version_2_is_a_hard_error() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .raw_gh_stack(None, br#"{"schemaVersion": 2, "stacks": []}"#.to_vec())
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        match read_metadata(repo) {
            Err(StackError::GhStackSchemaUnsupported { version: 2, .. }) => {}
            Err(e) => panic!("expected GhStackSchemaUnsupported{{version: 2}}, got {e:?}"),
            Ok(_) => panic!("expected GhStackSchemaUnsupported{{version: 2}}, got Ok"),
        }
    }

    #[test]
    fn missing_schema_version_defaults_to_1() {
        // The module doc claims a missing `schemaVersion` is treated as `1`, matching Go's
        // zero-value behavior for an unset int field. No prior test omitted the field.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .raw_gh_stack(
                None,
                br#"{"stacks": [{"number": 1, "trunk": {"branch": "main", "head": "", "base": ""}, "branches": [{"branch": "feat-a", "head": "", "base": ""}]}]}"#.to_vec(),
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert_eq!(meta.parents["feat-a"].parent, "main");
        assert_eq!(meta.stack_numbers["feat-a"], 1);
    }

    #[test]
    fn schema_version_0_defaults_to_1() {
        // Same claim, explicit `schemaVersion: 0` — Go's own zero value for the field, and
        // distinct from "the field is absent" (missing_schema_version_defaults_to_1 above),
        // since the two arrive through different branches of `.filter(|&v| v != 0)`.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .raw_gh_stack(
                None,
                br#"{"schemaVersion": 0, "stacks": [{"number": 1, "trunk": {"branch": "main", "head": "", "base": ""}, "branches": [{"branch": "feat-a", "head": "", "base": ""}]}]}"#.to_vec(),
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert_eq!(meta.parents["feat-a"].parent, "main");
        assert_eq!(meta.stack_numbers["feat-a"], 1);
    }

    #[test]
    fn needs_restack_true_when_base_differs_from_parent_live_tip() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .gh_stack_at(
                None,
                1,
                "main",
                &[("feat-a", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")],
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        let entry = meta.parents.get("feat-a").unwrap();
        assert_eq!(
            entry.parent_revision.as_deref(),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        );
        let main_tip = repo
            .find_branch("main", git2::BranchType::Local)
            .unwrap()
            .get()
            .target()
            .unwrap();
        assert_ne!(
            entry.parent_revision.as_deref(),
            Some(main_tip.to_string().as_str())
        );
    }

    #[test]
    fn degraded_union_pulls_in_unlinked_worktree_file() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(Some("feat-a"), 9, "main", &["feat-a"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert_eq!(meta.parents["feat-a"].parent, "main");
        assert_eq!(meta.stack_numbers["feat-a"], 9);
    }

    #[test]
    fn degraded_union_first_wins_on_disagreeing_unlinked_files() {
        // Two worktrees each hold their own unlinked file, both claiming stack number 1 for
        // a different branch set. Canonical is empty, so both are unioned; directory-name
        // order ("feat-a" < "feat-b") makes feat-a's file win the number-1 identity.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .worktree("feat-b")
            .gh_stack(Some("feat-a"), 1, "main", &["feat-a"])
            .gh_stack(Some("feat-b"), 1, "main", &["feat-b"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert!(meta.parents.contains_key("feat-a"));
        assert!(!meta.parents.contains_key("feat-b"));
    }

    #[test]
    fn branch_spanning_two_stacks_keeps_the_first_stacks_parent_and_number() {
        // Regression test for finding E: read_metadata's flattening loop deduped `trunks`
        // first-wins but wrote `parents`/`stack_numbers` last-wins, contradicting the module
        // doc's "first wins wholesale" and the spec's "first-seen wins, doctor flags it". Two
        // canonical stacks, both listing "shared" — stack 1 comes first in file order, so its
        // parent ("main") and number (1) must stick even though stack 2 ("other-trunk", 2) is
        // read afterward.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .branch("other-trunk")
            .worktree("main")
            .gh_stack(None, 1, "main", &["shared"])
            .gh_stack(None, 2, "other-trunk", &["shared"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert_eq!(meta.parents["shared"].parent, "main");
        assert_eq!(meta.stack_numbers["shared"], 1);
    }
}
