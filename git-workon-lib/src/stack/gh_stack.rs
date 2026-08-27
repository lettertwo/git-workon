//! `gh stack` (`github/gh-stack` CLI extension) stack detection — read path.
//!
//! Stack metadata is read without invoking `gh`. Upstream writes one JSON file per git dir
//! (`schemaVersion: 1`, `{ repository, stacks: [{ id, number, trunk: branchRef, branches:
//! [branchRef] }] }`, `branchRef = { branch, head, base, pullRequest }`), which for a linked
//! worktree is per-worktree, not shared. workon keeps one canonical copy at
//! `<common-dir>/gh-stack` and symlinks each worktree's admin-dir path to it (see
//! [`link_worktree`]) so it behaves like Graphite's shared store.
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

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

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

// ── Linking worktrees to the canonical file ─────────────────────────────────────────────

/// RAII guard holding `<common-dir>/gh-stack.lock`'s `flock`. Released on drop.
#[cfg(unix)]
struct LockGuard(std::fs::File);

#[cfg(unix)]
impl Drop for LockGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid, open file descriptor for the whole guard lifetime.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(not(unix))]
struct LockGuard;

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Take `<common-dir>/gh-stack.lock` (`flock(LOCK_EX | LOCK_NB)`, retried every 100ms up to
/// 5s), so a concurrent `gh stack` run in any worktree is genuinely excluded — every
/// worktree's lock path symlinks to this same file (see [`link_worktree`]). A no-op guard on
/// non-unix targets, mirroring `graphite.rs`'s `#[cfg(not(unix))]` fallback.
#[cfg(unix)]
fn lock_canonical(repo: &Repository) -> Result<LockGuard, StackError> {
    let lock_path = repo.commondir().join("gh-stack.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false) // lock file's contents (if any) are irrelevant; never clobber them
        .open(&lock_path)
        .map_err(|e| StackError::GhStackWriteFailed {
            path: lock_path.clone(),
            message: e.to_string(),
        })?;

    let deadline = std::time::Instant::now() + LOCK_TIMEOUT;
    loop {
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            return Ok(LockGuard(file));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) || std::time::Instant::now() >= deadline {
            return Err(StackError::GhStackLocked { path: lock_path });
        }
        std::thread::sleep(LOCK_RETRY_DELAY);
    }
}

#[cfg(not(unix))]
fn lock_canonical(_repo: &Repository) -> Result<LockGuard, StackError> {
    Ok(LockGuard)
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Plant `admin_dir/<filename>` as a relative symlink (`../../<filename>`) to
/// `<common-dir>/<filename>`. Idempotent — a no-op if the symlink already points there.
/// Never replaces a regular file: that is [`migrate_worktree`]'s job alone.
fn plant_link(admin_dir: &Path, filename: &str) -> Result<(), StackError> {
    let link_path = admin_dir.join(filename);
    let relative_target = Path::new("..").join("..").join(filename);

    match std::fs::symlink_metadata(&link_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if std::fs::read_link(&link_path).ok().as_deref() == Some(relative_target.as_path()) {
                return Ok(()); // already correctly linked
            }
            std::fs::remove_file(&link_path).map_err(|e| StackError::GhStackLinkFailed {
                path: link_path.clone(),
                message: e.to_string(),
            })?;
            create_symlink(&relative_target, &link_path).map_err(|e| {
                StackError::GhStackLinkFailed {
                    path: link_path,
                    message: e.to_string(),
                }
            })
        }
        Ok(_) => Ok(()), // a real file is here — never replace it, see migrate_worktree
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_symlink(&relative_target, &link_path).map_err(|e| {
                StackError::GhStackLinkFailed {
                    path: link_path,
                    message: e.to_string(),
                }
            })
        }
        Err(e) => Err(StackError::GhStackLinkFailed {
            path: link_path,
            message: e.to_string(),
        }),
    }
}

/// Plant `gh-stack` and `gh-stack.lock` in `<common>/worktrees/<worktree_name>/` as relative
/// symlinks (`../../gh-stack`) to the canonical store. Idempotent. Never replaces a regular
/// file — that is [`migrate_worktree`]'s job.
///
/// Safe to call before any stack exists: `open()` with `O_CREAT` through a dangling symlink
/// creates the target, so the first `gh stack init` in any linked worktree creates canonical.
/// See the module docs.
pub(crate) fn link_worktree(repo: &Repository, worktree_name: &str) -> Result<(), StackError> {
    let admin_dir = repo.commondir().join("worktrees").join(worktree_name);
    plant_link(&admin_dir, "gh-stack")?;
    plant_link(&admin_dir, "gh-stack.lock")?;
    Ok(())
}

/// Identity computed straight from a raw `stacks[]` entry `Value`, mirroring [`identity`] but
/// without parsing into [`GhStackEntry`] first — used by [`migrate_worktree`], which must
/// preserve `id`/`pullRequest`/`head` verbatim rather than round-tripping through the
/// read-path's lossy struct.
fn raw_identity(entry: &Value) -> StackIdentity {
    let number = entry.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
    if number != 0 {
        return StackIdentity::Number(number);
    }
    let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    if !id.is_empty() {
        return StackIdentity::Id(id.to_string());
    }
    let trunk = entry
        .get("trunk")
        .and_then(|t| t.get("branch"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let first_branch = entry
        .get("branches")
        .and_then(|b| b.as_array())
        .and_then(|arr| arr.first())
        .and_then(|b| b.get("branch"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    StackIdentity::TrunkAndFirstBranch(trunk, first_branch)
}

/// Read `path` as a whole raw `Value` (no [`GhStackEntry`] parsing, so every top-level field —
/// `repository`, `id`, `pullRequest`, anything a future gh-stack adds — survives), rejecting
/// `schemaVersion > 1`. `Ok(None)` for a missing file. A single attempt, no retries: called
/// only under [`lock_canonical`] during `doctor --fix` or [`register_branch`], not on the hot
/// read path [`read_doc`] serves.
fn read_raw_doc(path: &Path) -> Result<Option<Value>, StackError> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StackError::GhStackParseFailed {
            path: path.to_path_buf(),
            message: e.to_string(),
        }),
        Ok(bytes) => {
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|e| StackError::GhStackParseFailed {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;
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
            Ok(Some(value))
        }
    }
}

/// Read `path`'s `stacks[]` array as raw `Value`s (no [`GhStackEntry`] parsing, so `id` and
/// `pullRequest` survive). Missing file, or a file with no `stacks` array, is an empty vec.
fn read_raw_stacks(path: &Path) -> Result<Vec<Value>, StackError> {
    Ok(read_raw_doc(path)?
        .and_then(|doc| doc.get("stacks").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default())
}

/// Merge a worktree's real `gh-stack` file into canonical, then replace it with a symlink.
/// Writes `gh-stack.bak` alongside the original before removing it. Takes the canonical lock
/// throughout.
///
/// This is the only place workon replaces a file another tool wrote, so it is reachable only
/// from `doctor --fix` — never automatically, never from `workon new`. If `worktree_name`'s
/// `gh-stack` path is missing or already a symlink, this degrades to [`link_worktree`]: there
/// is no real file to migrate.
///
/// Merge order matches [`read_metadata`]'s dedupe rule: canonical entries are seeded first, so
/// a colliding identity in the worktree file is dropped, never merged field-by-field.
pub(crate) fn migrate_worktree(repo: &Repository, worktree_name: &str) -> Result<(), StackError> {
    let admin_dir = repo.commondir().join("worktrees").join(worktree_name);
    let worktree_file = admin_dir.join("gh-stack");

    let is_regular_file = matches!(
        std::fs::symlink_metadata(&worktree_file),
        Ok(meta) if !meta.file_type().is_symlink()
    );
    if !is_regular_file {
        remove_stale_lock_file(&admin_dir)?;
        return link_worktree(repo, worktree_name);
    }

    let _lock = lock_canonical(repo)?;

    let canonical = canonical_path(repo);
    let canonical_doc = read_raw_doc(&canonical)?;

    // Base the merged document on canonical's whole `Value` when it exists, falling back to
    // the worktree file's, so every top-level field outside `stacks` — `repository` above
    // all — round-trips instead of being discarded. Mirrors `plan_registered_doc`, which
    // round-trips the same way for the same reason.
    let mut doc = match &canonical_doc {
        Some(v) => v.clone(),
        None => read_raw_doc(&worktree_file)?
            .unwrap_or_else(|| serde_json::json!({ "schemaVersion": 1, "stacks": [] })),
    };

    let mut merged: Vec<Value> = canonical_doc
        .as_ref()
        .and_then(|v| v.get("stacks"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut seen: HashSet<StackIdentity> = merged.iter().map(raw_identity).collect();
    for entry in read_raw_stacks(&worktree_file)? {
        if seen.insert(raw_identity(&entry)) {
            merged.push(entry);
        }
    }

    doc["schemaVersion"] = serde_json::json!(1);
    doc["stacks"] = serde_json::Value::Array(merged);

    // Verify the merged result parses before touching anything on disk or unlinking the
    // worktree's original — never destroy the only copy of data that failed to round-trip.
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| StackError::GhStackWriteFailed {
        path: canonical.clone(),
        message: e.to_string(),
    })?;
    serde_json::from_slice::<Value>(&bytes).map_err(|e| StackError::GhStackParseFailed {
        path: canonical.clone(),
        message: e.to_string(),
    })?;

    let tmp_path = canonical.with_extension("tmp");
    std::fs::write(&tmp_path, &bytes).map_err(|e| StackError::GhStackWriteFailed {
        path: tmp_path.clone(),
        message: e.to_string(),
    })?;
    std::fs::rename(&tmp_path, &canonical).map_err(|e| StackError::GhStackWriteFailed {
        path: canonical.clone(),
        message: e.to_string(),
    })?;

    // Re-read the bytes actually on disk (not just the in-memory copy) before unlinking the
    // worktree's original file. `read_raw_stacks`, not `read_doc`: `read_doc` only ever
    // returns `Err` for a schema mismatch that can't happen on a doc we just wrote with
    // `schemaVersion: 1`, so it can never fail here and isn't a real guard. `read_raw_stacks`
    // is single-attempt and genuinely errors on a parse failure.
    read_raw_stacks(&canonical)?;

    let bak_path = next_available_backup_path(&admin_dir);
    std::fs::rename(&worktree_file, &bak_path).map_err(|e| StackError::GhStackWriteFailed {
        path: worktree_file.clone(),
        message: e.to_string(),
    })?;

    remove_stale_lock_file(&admin_dir)?;
    link_worktree(repo, worktree_name)
}

/// Remove `admin_dir/gh-stack.lock` if it is a regular file, so the following
/// [`link_worktree`] call can plant a proper symlink — [`plant_link`] never replaces a regular
/// file. Its contents are irrelevant (upstream never reads them, it is a pure `flock` target;
/// see the module docs' shared-canonical-file section), so simply discarding it is safe.
///
/// A worktree that has already run `gh stack` almost always has a real `gh-stack.lock`
/// alongside its real `gh-stack` file (upstream opens it `O_CREATE` on every lock attempt), so
/// this runs on every path through [`migrate_worktree`], not only the merge path — leaving it
/// behind would silently keep that worktree flocking a private inode instead of the shared
/// canonical lock, defeating cross-worktree mutual exclusion without [`worktree_link_status`]
/// (which now checks both paths) reporting it.
fn remove_stale_lock_file(admin_dir: &Path) -> Result<(), StackError> {
    let lock_path = admin_dir.join("gh-stack.lock");
    let is_regular_file = matches!(
        std::fs::symlink_metadata(&lock_path),
        Ok(meta) if !meta.file_type().is_symlink()
    );
    if is_regular_file {
        std::fs::remove_file(&lock_path).map_err(|e| StackError::GhStackWriteFailed {
            path: lock_path,
            message: e.to_string(),
        })?;
    }
    Ok(())
}

/// The first of `gh-stack.bak`, `gh-stack.bak.1`, `gh-stack.bak.2`, ... that doesn't already
/// exist in `admin_dir`. A worktree can legitimately acquire a real `gh-stack` file again
/// after a prior migration — the write-in-place risk this crate's module docs describe (a
/// gh-stack release switching to temp-and-rename would cause exactly this) — so re-migrating
/// must never clobber the backup a previous migration left behind.
fn next_available_backup_path(admin_dir: &Path) -> PathBuf {
    let base = admin_dir.join("gh-stack.bak");
    if std::fs::symlink_metadata(&base).is_err() {
        return base;
    }
    (1u32..)
        .map(|n| admin_dir.join(format!("gh-stack.bak.{n}")))
        .find(|candidate| std::fs::symlink_metadata(candidate).is_err())
        .expect("u32 backup suffixes are effectively inexhaustible")
}

// ── `doctor` support ─────────────────────────────────────────────────────────────────────

/// Per-worktree link status, for `doctor`'s `GhStackWorktreeNotLinked` check and its `--fix`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    /// Correctly symlinked to canonical.
    Linked,
    /// Not linked. `holds_file` distinguishes the two `--fix` actions: `true` means a real
    /// file is present and must be merged ([`migrate_worktree`]); `false` means the path is
    /// simply missing (or a symlink pointing somewhere else) and can be planted directly
    /// ([`link_worktree`]).
    NotLinked { holds_file: bool },
}

/// `Linked`/`NotLinked { holds_file }` for a single admin-dir path against `expected_target`
/// — the canonical file *that path's own filename* symlinks to (`<common-dir>/gh-stack` for a
/// `gh-stack` path, `<common-dir>/gh-stack.lock` for a `gh-stack.lock` path). Factored out of
/// [`worktree_link_status`] so it can be applied to both filenames — a stale `gh-stack.lock`
/// regular file (upstream opens it `O_CREATE` on every lock attempt, so a worktree that has run
/// `gh stack` almost always has one) is just as unlinked as a stale `gh-stack` file, and must be
/// equally visible to `doctor`.
fn link_status_for_path(path: &Path, expected_target: &Path) -> LinkStatus {
    match std::fs::symlink_metadata(path) {
        Err(_) => LinkStatus::NotLinked { holds_file: false },
        Ok(meta) if meta.file_type().is_symlink() => {
            if is_symlink_resolving_to(path, expected_target) {
                LinkStatus::Linked
            } else {
                LinkStatus::NotLinked { holds_file: false }
            }
        }
        Ok(_) => LinkStatus::NotLinked { holds_file: true },
    }
}

/// Compute [`LinkStatus`] for `worktree_name`'s `gh-stack` and `gh-stack.lock` admin-dir
/// paths, `NotLinked` if either is unlinked. `holds_file` reflects only the `gh-stack` path —
/// whether there is a real stack file needing [`migrate_worktree`]'s merge — never the lock
/// file, whose contents are irrelevant and whose own staleness is fully handled by
/// [`migrate_worktree`] discarding it before relinking; a stale lock alone must route through
/// [`link_worktree`], not `migrate_worktree`.
///
/// Each path is checked against its own target: `plant_link` symlinks `gh-stack` to
/// `../../gh-stack` and `gh-stack.lock` to `../../gh-stack.lock`, so comparing both against
/// `canonical_path` alone would mean the `gh-stack.lock` check can never resolve to it —
/// `is_symlink_resolving_to` compares the *lock's* resolved target against the *stack file's*
/// path, which are never equal.
pub(crate) fn worktree_link_status(repo: &Repository, worktree_name: &str) -> LinkStatus {
    let admin_dir = repo.commondir().join("worktrees").join(worktree_name);
    let canonical = canonical_path(repo);
    let canonical_lock = repo.commondir().join("gh-stack.lock");

    let gh_stack_status = link_status_for_path(&admin_dir.join("gh-stack"), &canonical);
    if matches!(gh_stack_status, LinkStatus::NotLinked { .. }) {
        return gh_stack_status;
    }

    match link_status_for_path(&admin_dir.join("gh-stack.lock"), &canonical_lock) {
        LinkStatus::Linked => LinkStatus::Linked,
        LinkStatus::NotLinked { .. } => LinkStatus::NotLinked { holds_file: false },
    }
}

/// Files (canonical + [`unlinked_files`]) that exist but fail to parse, or whose
/// `schemaVersion` is unsupported — for `doctor`'s `GhStackFileUnreadable` check.
///
/// Unlike [`read_doc`], this is a single-attempt read: `doctor` is a point-in-time health
/// check, not the hot read path a concurrent `gh stack` write races against, so there is no
/// truncated-read tolerance to preserve here — a transient mid-write read just gets reported
/// and re-checked on the next `doctor` run.
pub(crate) fn readability_errors(repo: &Repository) -> Vec<(PathBuf, StackError)> {
    let mut sources = vec![canonical_path(repo)];
    sources.extend(unlinked_files(repo));

    sources
        .into_iter()
        .filter(|path| path.exists())
        .filter_map(|path| match read_raw_stacks(&path) {
            Ok(_) => None,
            Err(e) => Some((path, e)),
        })
        .collect()
}

/// Stack numbers that appear in more than one gh-stack source — only possible when the
/// degraded union read (see the module docs) actually combines canonical with an unlinked
/// worktree file. For `doctor`'s `GhStackDivergentStacks` check.
pub(crate) fn divergent_stack_numbers(repo: &Repository) -> Vec<u64> {
    let mut sources = vec![canonical_path(repo)];
    sources.extend(unlinked_files(repo));

    let mut counts: HashMap<u64, usize> = HashMap::new();
    for path in &sources {
        if let Ok(Some(entries)) = read_doc(path) {
            for entry in entries {
                if entry.number != 0 {
                    *counts.entry(entry.number).or_insert(0) += 1;
                }
            }
        }
    }

    let mut divergent: Vec<u64> = counts
        .into_iter()
        .filter(|&(_, count)| count > 1)
        .map(|(number, _)| number)
        .collect();
    divergent.sort_unstable();
    divergent
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

    // ── link_worktree / migrate_worktree ────────────────────────────────────────

    #[test]
    fn link_worktree_plants_relative_symlinks() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        link_worktree(repo, "feat-a").unwrap();

        repo.assert(predicate::repo::gh_stack_is_linked("feat-a"));
        let lock_target = std::fs::read_link(
            repo.commondir()
                .join("worktrees")
                .join("feat-a")
                .join("gh-stack.lock"),
        )
        .unwrap();
        assert_eq!(lock_target, Path::new("../../gh-stack.lock"));
    }

    #[test]
    fn link_worktree_is_idempotent() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        link_worktree(repo, "feat-a").unwrap();
        link_worktree(repo, "feat-a").unwrap();

        repo.assert(predicate::repo::gh_stack_is_linked("feat-a"));
    }

    #[test]
    fn link_worktree_replaces_a_symlink_pointing_somewhere_wrong() {
        // plant_link has three arms: already-correct (link_worktree_is_idempotent), missing
        // (link_worktree_plants_relative_symlinks), and remove-and-recreate for a symlink that
        // exists but resolves elsewhere. Only the first two had coverage.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();
        let admin_dir = repo.commondir().join("worktrees").join("feat-a");

        create_symlink(Path::new("../../nonsense"), &admin_dir.join("gh-stack")).unwrap();

        link_worktree(repo, "feat-a").unwrap();

        repo.assert(predicate::repo::gh_stack_is_linked("feat-a"));
    }

    #[test]
    fn link_worktree_never_replaces_a_real_file() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(Some("feat-a"), 3, "main", &["feat-a"])
            .gh_stack_unlinked("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        link_worktree(repo, "feat-a").unwrap();

        // Still a real file — link_worktree must never clobber it.
        repo.assert(predicate::repo::gh_stack_contains_branch(
            Some("feat-a"),
            "feat-a",
            0,
        ));
        let meta =
            std::fs::symlink_metadata(repo.commondir().join("worktrees/feat-a/gh-stack")).unwrap();
        assert!(!meta.file_type().is_symlink());
    }

    #[test]
    fn migrate_worktree_merges_into_canonical_and_leaves_backup() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(Some("feat-a"), 7, "main", &["feat-a"])
            .gh_stack_unlinked("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        migrate_worktree(repo, "feat-a").unwrap();

        // Merged into canonical...
        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-a", 0));
        // ...and the worktree is now linked to it.
        repo.assert(predicate::repo::gh_stack_is_linked("feat-a"));
        // ...with a backup of the original left behind.
        assert!(repo
            .commondir()
            .join("worktrees/feat-a/gh-stack.bak")
            .exists());

        let meta = read_metadata(repo).unwrap();
        assert_eq!(meta.stack_numbers["feat-a"], 7);
    }

    #[test]
    fn migrate_worktree_preserves_top_level_fields_when_canonical_is_absent() {
        // No `gh_stack`/`gh_stack_at` call targets `None` (canonical), so canonical doesn't
        // exist before migration and the merged document must be seeded from the worktree
        // file's whole `Value` — including `repository` — not synthesized from scratch.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(Some("feat-a"), 7, "main", &["feat-a"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        migrate_worktree(repo, "feat-a").unwrap();

        repo.assert(predicate::repo::gh_stack_preserves(
            None,
            "/repository",
            "git-workon-fixture/gh-stack",
        ));
        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-a", 0));
    }

    #[test]
    fn migrate_worktree_falls_back_to_link_when_nothing_to_merge() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        migrate_worktree(repo, "feat-a").unwrap();

        repo.assert(predicate::repo::gh_stack_is_linked("feat-a"));
        assert!(!repo
            .commondir()
            .join("worktrees/feat-a/gh-stack.bak")
            .exists());
    }

    #[test]
    fn migrate_worktree_never_clobbers_an_existing_backup() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(Some("feat-a"), 7, "main", &["feat-a"])
            .gh_stack_unlinked("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        migrate_worktree(repo, "feat-a").unwrap();

        let admin_dir = repo.commondir().join("worktrees/feat-a");
        let first_backup = admin_dir.join("gh-stack.bak");
        assert!(first_backup.exists());
        let first_backup_contents = std::fs::read(&first_backup).unwrap();

        // Simulate a gh-stack release switching to temp-and-rename: the symlink this
        // worktree's `gh-stack` path was left as gets replaced with a real file again.
        std::fs::remove_file(admin_dir.join("gh-stack")).unwrap();
        std::fs::write(
            admin_dir.join("gh-stack"),
            br#"{"schemaVersion":1,"stacks":[]}"#,
        )
        .unwrap();

        migrate_worktree(repo, "feat-a").unwrap();

        // The first backup is untouched...
        assert_eq!(std::fs::read(&first_backup).unwrap(), first_backup_contents);
        // ...and the second migration's original landed in a numbered backup instead.
        assert!(admin_dir.join("gh-stack.bak.1").exists());
        repo.assert(predicate::repo::gh_stack_is_linked("feat-a"));
    }

    #[test]
    fn migrate_worktree_also_migrates_a_stale_lock_file() {
        // A worktree that has already run `gh stack` almost always has a real `gh-stack.lock`
        // alongside its real `gh-stack` file (upstream opens it O_CREATE on every lock
        // attempt). Both must end up symlinked, or this worktree's `gh stack` keeps flocking a
        // private inode while `register_branch` flocks the shared canonical lock.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(Some("feat-a"), 7, "main", &["feat-a"])
            .gh_stack_unlinked("feat-a")
            .gh_stack_lock_unlinked("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let admin_dir = repo.commondir().join("worktrees/feat-a");
        let lock_meta_before = std::fs::symlink_metadata(admin_dir.join("gh-stack.lock")).unwrap();
        assert!(!lock_meta_before.file_type().is_symlink());

        migrate_worktree(repo, "feat-a").unwrap();

        repo.assert(predicate::repo::gh_stack_is_linked("feat-a"));
        let lock_meta_after = std::fs::symlink_metadata(admin_dir.join("gh-stack.lock")).unwrap();
        assert!(
            lock_meta_after.file_type().is_symlink(),
            "gh-stack.lock must be a symlink after migration"
        );
        let lock_target = std::fs::read_link(admin_dir.join("gh-stack.lock")).unwrap();
        assert_eq!(lock_target, Path::new("../../gh-stack.lock"));
    }

    #[test]
    fn worktree_link_status_reports_linked_for_a_fully_linked_worktree() {
        // Regression test: link_status_for_path used to compare BOTH the gh-stack and
        // gh-stack.lock paths against canonical_path (the gh-stack target), but plant_link
        // symlinks gh-stack.lock to `../../gh-stack.lock`, which can never resolve to
        // `../../gh-stack`. A correctly, fully linked worktree reported NotLinked forever, and
        // no test anywhere asserted LinkStatus::Linked, which is why this regression shipped.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(None, 1, "main", &["feat-a"])
            .gh_stack_linked("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        assert_eq!(worktree_link_status(repo, "feat-a"), LinkStatus::Linked);

        // Healthy-path invariants that also had no coverage: a fully linked worktree leaves
        // nothing for the degraded union fallback to pick up, and no stack number collides
        // with itself across sources.
        assert!(unlinked_files(repo).is_empty());
        assert!(divergent_stack_numbers(repo).is_empty());
    }

    #[test]
    fn worktree_link_status_reports_not_linked_when_lock_is_a_regular_file() {
        // gh-stack itself is correctly linked, but gh-stack.lock reverted to a real file (the
        // write-in-place risk this module's docs describe). worktree_link_status must catch
        // this from the gh-stack path alone being insufficient — doctor would otherwise report
        // `Linked` forever with no way to detect the lost cross-worktree exclusion.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack_linked("feat-a")
            .gh_stack_lock_unlinked("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        match worktree_link_status(repo, "feat-a") {
            LinkStatus::NotLinked { holds_file } => {
                // holds_file describes the gh-stack path, not the lock, and the gh-stack path
                // here is correctly linked (not holding a real file).
                assert!(!holds_file);
            }
            LinkStatus::Linked => panic!("expected NotLinked, got Linked"),
        }
    }
}
