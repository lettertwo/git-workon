//! `gh stack` (`github/gh-stack` CLI extension) stack detection — read path.
//!
//! Stack metadata is read without invoking `gh`. gh-stack >=0.2 keeps one shared JSON catalog
//! at `<common-dir>/gh-stack` (`schemaVersion: 1`, `{ repository, stacks: [{ id, number,
//! trunk: branchRef, branches: [branchRef] }] }`, `branchRef = { branch, head, base,
//! pullRequest }`), written with temp-and-rename, and migrates any per-worktree file it finds
//! into it. Upstream owns that store: workon never creates, links, or migrates it. It reads
//! the catalog, unions in older per-worktree files (see below), and appends new branches via
//! [`register_branch`]. Earlier workon versions symlinked each worktree's admin-dir path to
//! the catalog; gh-stack >=0.2 rejects a symlink there, so workon no longer plants any.
//!
//! The write path ([`register_branch`]) follows upstream's locking: flock
//! `<common-dir>/gh-stack-operation.lock`, then `<common-dir>/gh-stack.lock`, then refuse if
//! the `<common-dir>/gh-stack-migration` journal exists, and only then read, plan, and write.
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
//! order. With gh-stack >=0.2 the union is empty once upstream has migrated those files. It
//! stays as the fallback for gh-stack <0.2, which still writes a real file per worktree, and
//! for files upstream hasn't migrated yet. `doctor` flags any unlinked file it finds.
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
//! A partial file can be observed during a concurrent `gh stack` command that writes in
//! place, which gh-stack <0.2 still does for its per-worktree files (>=0.2 writes the catalog
//! with temp-and-rename). [`read_metadata`] retries a read-and-parse up to 3 times, 25ms apart, and skips the file with `log::warn!` if every
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

#[derive(Debug)]
struct GhStackBranchRef {
    branch: String,
    base: String,
    merged: bool,
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
            merged: value
                .get("pullRequest")
                .and_then(|pr| pr.get("merged"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
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
                    merged: branch_ref.merged,
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

/// RAII guard holding a gh-stack lock file's `flock`. Released on drop.
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

/// Take the `flock(LOCK_EX | LOCK_NB)` on `lock_path`, retried every 100ms up to 5s, the same
/// policy gh-stack uses for both of its locks. The file is created if missing and never
/// deleted, matching upstream. A no-op guard on non-unix targets, mirroring `graphite.rs`'s
/// `#[cfg(not(unix))]` fallback.
#[cfg(unix)]
fn flock_path(lock_path: PathBuf) -> Result<LockGuard, StackError> {
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
fn flock_path(_lock_path: PathBuf) -> Result<LockGuard, StackError> {
    Ok(LockGuard)
}

/// Take `<common-dir>/gh-stack-operation.lock`, which gh-stack >=0.2 holds for a whole command
/// and acquires before loading state and before the catalog lock. Take it first, always, so
/// the two processes can't deadlock on opposite orders.
fn lock_operation(repo: &Repository) -> Result<LockGuard, StackError> {
    flock_path(repo.commondir().join("gh-stack-operation.lock"))
}

/// Take `<common-dir>/gh-stack.lock`, the same catalog lock gh-stack takes, so a concurrent
/// `gh stack` run is excluded while [`register_branch`] rewrites the catalog. Always taken
/// after [`lock_operation`].
fn lock_canonical(repo: &Repository) -> Result<LockGuard, StackError> {
    flock_path(repo.commondir().join("gh-stack.lock"))
}

/// Read `path` as a whole raw `Value` (no [`GhStackEntry`] parsing, so every top-level field —
/// `repository`, `id`, `pullRequest`, anything a future gh-stack adds — survives), rejecting
/// `schemaVersion > 1`. `Ok(None)` for a missing file. A single attempt, no retries: called
/// only under the locks during `doctor --fix` or [`register_branch`], not on the hot
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

// ── Registering new branches (write path) ───────────────────────────────────────────────

/// Resolve `name`'s local branch tip. `workon new` has already created both `branch` and
/// (normally) `base_branch` by the time [`register_branch`] runs, so failure here means
/// something is badly wrong rather than an expected condition — reported as
/// [`StackError::GhStackWriteFailed`] since there's no more specific variant for "the thing
/// we were asked to register doesn't resolve to a commit".
fn branch_tip(repo: &Repository, name: &str) -> Result<git2::Oid, StackError> {
    let branch = repo
        .find_branch(name, git2::BranchType::Local)
        .map_err(|e| StackError::GhStackWriteFailed {
            path: canonical_path(repo),
            message: format!("branch '{name}' not found: {e}"),
        })?;
    branch
        .get()
        .target()
        .ok_or_else(|| StackError::GhStackWriteFailed {
            path: canonical_path(repo),
            message: format!("branch '{name}' has no target (unborn?)"),
        })
}

/// Find the index in `stacks` (a `stacks[]` array of raw `Value`s) whose stack currently ends
/// at `base_branch`: either its last `branches` element is `base_branch`, or it has no
/// `branches` yet and its `trunk.branch` is `base_branch`. First match wins when more than
/// one qualifies — `doctor`'s `GhStackDivergentStacks` check is what flags that situation,
/// not this function.
fn select_target_index(stacks: &[Value], base_branch: &str) -> Option<usize> {
    stacks
        .iter()
        .position(|stack| {
            stack
                .get("branches")
                .and_then(|b| b.as_array())
                .and_then(|arr| arr.last())
                .and_then(|b| b.get("branch"))
                .and_then(|v| v.as_str())
                == Some(base_branch)
        })
        .or_else(|| {
            stacks.iter().position(|stack| {
                let branches_empty = stack
                    .get("branches")
                    .and_then(|b| b.as_array())
                    .map(|arr| arr.is_empty())
                    .unwrap_or(true);
                branches_empty
                    && stack
                        .get("trunk")
                        .and_then(|t| t.get("branch"))
                        .and_then(|v| v.as_str())
                        == Some(base_branch)
            })
        })
}

/// Build the full replacement document (as pretty-printed bytes) for `register_branch`,
/// given the raw bytes currently on disk (`existing`, possibly empty for "file doesn't exist
/// yet"). Round-trips through `serde_json::Value` rather than a typed struct so `id`,
/// `pullRequest`, and any other field on untouched `stacks[]` entries survive unchanged —
/// only the target stack's `branches` array gains one new, minimal entry.
fn plan_registered_doc(
    existing: &[u8],
    branch: &str,
    base_branch: &str,
    base: &str,
    head: &str,
    canonical: &Path,
) -> Result<Vec<u8>, StackError> {
    let mut doc: Value = if existing.is_empty() {
        serde_json::json!({ "schemaVersion": 1, "stacks": [] })
    } else {
        serde_json::from_slice(existing).map_err(|e| StackError::GhStackParseFailed {
            path: canonical.to_path_buf(),
            message: e.to_string(),
        })?
    };

    let version = doc
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .filter(|&v| v != 0)
        .unwrap_or(1);
    if version > 1 {
        return Err(StackError::GhStackSchemaUnsupported {
            path: canonical.to_path_buf(),
            version,
        });
    }

    let stacks = doc
        .get_mut("stacks")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| StackError::GhStackNoStackForBase {
            base: base_branch.to_string(),
        })?;

    let idx = select_target_index(stacks, base_branch).ok_or_else(|| {
        StackError::GhStackNoStackForBase {
            base: base_branch.to_string(),
        }
    })?;

    // `pullRequest` is deliberately omitted, matching upstream's `omitempty` on a fresh entry.
    let new_entry = serde_json::json!({ "branch": branch, "head": head, "base": base });
    match stacks[idx]
        .get_mut("branches")
        .and_then(|v| v.as_array_mut())
    {
        Some(arr) => arr.push(new_entry),
        None => stacks[idx]["branches"] = serde_json::json!([new_entry]),
    }

    serde_json::to_vec_pretty(&doc).map_err(|e| StackError::GhStackWriteFailed {
        path: canonical.to_path_buf(),
        message: e.to_string(),
    })
}

/// Write `bytes` to `canonical` via `<common-dir>/gh-stack.tmp` then `fs::rename`, mode 0644.
/// Atomic, unlike upstream's `os.WriteFile` — always call this with `canonical` itself
/// ([`canonical_path`]'s return value), never a worktree's symlinked admin-dir path: renaming
/// onto a symlink replaces the link with a real file instead of updating what it points to,
/// silently detaching that worktree from the shared store.
fn write_canonical_atomic(canonical: &Path, bytes: &[u8]) -> Result<(), StackError> {
    let tmp_path = canonical.with_extension("tmp");
    std::fs::write(&tmp_path, bytes).map_err(|e| StackError::GhStackWriteFailed {
        path: tmp_path.clone(),
        message: e.to_string(),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o644)).map_err(
            |e| StackError::GhStackWriteFailed {
                path: tmp_path.clone(),
                message: e.to_string(),
            },
        )?;
    }

    std::fs::rename(&tmp_path, canonical).map_err(|e| StackError::GhStackWriteFailed {
        path: canonical.to_path_buf(),
        message: e.to_string(),
    })
}

/// Append `branch` to the canonical file's stack that currently ends at `base_branch`.
/// Round-trips through `serde_json::Value` — a typed struct would silently drop `id`,
/// `pullRequest`, and any field a future gh-stack adds.
///
/// `base` is `repo.merge_base(base_branch's tip, branch's tip)` — equal to `base_branch`'s
/// tip in the normal case, but still correct if `base_branch` moved between worktree creation
/// and this call. Guarded by [`lock_operation`] then [`lock_canonical`] (upstream's order), so
/// a concurrent `gh stack` run in any worktree is genuinely excluded. Once both are held, a
/// present `<common-dir>/gh-stack-migration` journal aborts with
/// [`StackError::GhStackMigrationPending`]: gh-stack >=0.2 refuses catalog writes mid-migration
/// and so does workon. Locks release in reverse order on drop.
///
/// The file is read only after both locks are held. No lock-respecting writer (every `gh stack`
/// invocation, and every other `git-workon` call into this module) can be mid-write once the
/// lock is ours, so a read-under-lock always sees a complete file — there is nothing to
/// compare-and-swap against. A pre-lock read would risk observing upstream's non-atomic
/// `os.WriteFile` mid-truncation and handing a partial prefix to [`plan_registered_doc`],
/// which is exactly the failure this ordering avoids.
pub fn register_branch(
    repo: &Repository,
    branch: &str,
    base_branch: &str,
) -> Result<(), StackError> {
    let head = branch_tip(repo, branch)?;
    let base_tip = branch_tip(repo, base_branch)?;
    let base = repo.merge_base(base_tip, head).unwrap_or(base_tip);

    let canonical = canonical_path(repo);
    let _operation_lock = lock_operation(repo)?;
    let _lock = lock_canonical(repo)?;

    let journal = repo.commondir().join("gh-stack-migration");
    if journal.exists() {
        return Err(StackError::GhStackMigrationPending { path: journal });
    }

    let existing = std::fs::read(&canonical).unwrap_or_default();
    let new_bytes = match plan_registered_doc(
        &existing,
        branch,
        base_branch,
        &base.to_string(),
        &head.to_string(),
        &canonical,
    ) {
        // `read_metadata` (used by `list`/`find`) unions canonical with `unlinked_files`, but
        // this function reads canonical alone — deliberately, since it must never write
        // through a worktree symlink (see `write_canonical_atomic`'s docs). So the spec's
        // accepted chicken-and-egg case (someone runs `gh stack init` inside a worktree before
        // ever running `doctor --fix`) reads fine everywhere but fails registration here with
        // a message that looks identical to "no such stack at all". Point at the fix instead.
        Err(StackError::GhStackNoStackForBase { base }) if !unlinked_files(repo).is_empty() => {
            return Err(StackError::GhStackStackInUnlinkedWorktree { base });
        }
        Err(e) => return Err(e),
        Ok(bytes) => bytes,
    };

    write_canonical_atomic(&canonical, &new_bytes)
}

// ── `doctor` support ─────────────────────────────────────────────────────────────────────

/// Upstream's recovery records (`gh-stack-rebase-state`, `gh-stack-modify-state`). One in an admin
/// dir that also holds a legacy catalog blocks upstream's migration (`MigrationBlockedError`).
const RECOVERY_FILES: [&str; 2] = ["gh-stack-rebase-state", "gh-stack-modify-state"];

/// What a worktree's admin dir holds that gh-stack >=0.2 cares about, for `doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorktreeGhStackState {
    /// `gh-stack` or `gh-stack.lock` is a symlink — what workon planted before gh-stack 0.2.
    /// Upstream's `readMigrationFile` rejects it, so every `gh stack` command fails.
    pub symlinked: bool,
    /// `gh-stack` is a real file: a legacy per-worktree catalog upstream migrates on its next run.
    pub legacy_file: bool,
    /// Recovery records present in the admin dir, by file name.
    pub recovery_files: Vec<&'static str>,
}

/// Inspect `worktree_name`'s admin dir with `symlink_metadata`, so a symlink is never followed.
/// A worktree with no `gh-stack*` entries yields the default (all-clear) state.
pub(crate) fn worktree_state(repo: &Repository, worktree_name: &str) -> WorktreeGhStackState {
    let admin_dir = repo.commondir().join("worktrees").join(worktree_name);
    let file_type = |name: &str| {
        std::fs::symlink_metadata(admin_dir.join(name))
            .ok()
            .map(|m| m.file_type())
    };

    WorktreeGhStackState {
        symlinked: ["gh-stack", "gh-stack.lock"]
            .iter()
            .any(|name| file_type(name).is_some_and(|t| t.is_symlink())),
        legacy_file: file_type("gh-stack").is_some_and(|t| t.is_file()),
        recovery_files: RECOVERY_FILES
            .into_iter()
            .filter(|name| file_type(name).is_some())
            .collect(),
    }
}

/// Remove the `gh-stack` and `gh-stack.lock` symlinks in `worktree_name`'s admin dir, returning
/// the names removed. Anything that is not a symlink (a legacy catalog upstream still has to
/// migrate, a real lock file) is left alone, and `symlink_metadata` keeps a symlink from being
/// followed to its target.
pub(crate) fn unlink_worktree(
    repo: &Repository,
    worktree_name: &str,
) -> crate::error::Result<Vec<&'static str>> {
    let admin_dir = repo.commondir().join("worktrees").join(worktree_name);
    let mut removed = Vec::new();
    for name in ["gh-stack", "gh-stack.lock"] {
        let path = admin_dir.join(name);
        let is_symlink = std::fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_symlink {
            std::fs::remove_file(&path)?;
            removed.push(name);
        }
    }
    Ok(removed)
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

/// Stack numbers that appear, with genuinely different content, in more than one gh-stack
/// source — only possible when the degraded union read (see the module docs) actually combines
/// canonical with an unlinked worktree file. For `doctor`'s `GhStackDivergentStacks` check.
///
/// Each number is counted at most once *per source*, so two stacks numbered 1 inside a single
/// file don't get flagged as spanning "more than one gh-stack source" — that phrase means
/// files, not array entries. And a number is only reported when its sources disagree: an
/// unlinked worktree file holding a byte-identical copy of a canonical stack (the common state
/// right after someone copies a worktree) is compared by content — at minimum its branch list —
/// so an identical copy is not reported.
pub(crate) fn divergent_stack_numbers(repo: &Repository) -> Vec<u64> {
    let mut sources = vec![canonical_path(repo)];
    sources.extend(unlinked_files(repo));

    // number -> one branch-list signature per source that contains it (deduped within that
    // source, so a file with two same-numbered stacks contributes one signature, not two).
    let mut signatures_by_number: HashMap<u64, Vec<Vec<String>>> = HashMap::new();
    for path in &sources {
        if let Ok(Some(entries)) = read_doc(path) {
            let mut numbers_in_this_source: HashSet<u64> = HashSet::new();
            for entry in entries {
                if entry.number != 0 && numbers_in_this_source.insert(entry.number) {
                    let branches: Vec<String> =
                        entry.branches.iter().map(|b| b.branch.clone()).collect();
                    signatures_by_number
                        .entry(entry.number)
                        .or_default()
                        .push(branches);
                }
            }
        }
    }

    let mut divergent: Vec<u64> = signatures_by_number
        .into_iter()
        .filter(|(_, signatures)| signatures.iter().any(|s| s != &signatures[0]))
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
    fn merged_true_is_read_from_pull_request() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .branch("feat-b")
            .raw_gh_stack(
                None,
                br#"{"schemaVersion": 1, "stacks": [{"number": 1, "trunk": {"branch": "main", "head": "", "base": ""}, "branches": [{"branch": "feat-a", "head": "", "base": "", "pullRequest": {"number": 1, "merged": true}}, {"branch": "feat-b", "head": "", "base": "", "pullRequest": null}]}]}"#.to_vec(),
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert!(meta.parents["feat-a"].merged);
        assert!(!meta.parents["feat-b"].merged, "sibling must stay unmerged");
    }

    #[test]
    fn missing_pull_request_reads_merged_false() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .raw_gh_stack(
                None,
                br#"{"schemaVersion": 1, "stacks": [{"number": 1, "trunk": {"branch": "main", "head": "", "base": ""}, "branches": [{"branch": "feat-a", "head": "", "base": ""}]}]}"#.to_vec(),
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert!(!meta.parents["feat-a"].merged);
    }

    #[test]
    fn null_pull_request_reads_merged_false() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .raw_gh_stack(
                None,
                br#"{"schemaVersion": 1, "stacks": [{"number": 1, "trunk": {"branch": "main", "head": "", "base": ""}, "branches": [{"branch": "feat-a", "head": "", "base": "", "pullRequest": null}]}]}"#.to_vec(),
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let meta = read_metadata(repo).unwrap();
        assert!(!meta.parents["feat-a"].merged);
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

    // ── divergent_stack_numbers ─────────────────────────────────────────────────

    #[test]
    fn two_numbered_stacks_in_one_file_are_not_divergent() {
        // Regression test for finding H(a): divergent_stack_numbers used to increment a
        // global per-number counter across all sources, so two *different* stacks both
        // numbered 1 inside the SAME file tripped count > 1, contradicting the doc comment
        // "appear in more than one gh-stack source" (source means file, not array entry).
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .branch("other-trunk")
            .worktree("main")
            .gh_stack(None, 1, "main", &["feat-a"])
            .gh_stack(None, 1, "other-trunk", &["feat-b"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        assert!(divergent_stack_numbers(repo).is_empty());
    }

    #[test]
    fn identical_copy_across_canonical_and_unlinked_is_not_divergent() {
        // Regression test for finding H(b): an unlinked worktree file holding a byte-identical
        // copy of a canonical stack is the common state right after someone copies a
        // worktree, not a real divergence, so it must not be flagged.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(None, 4, "main", &["feat-a"])
            .gh_stack(Some("feat-a"), 4, "main", &["feat-a"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        assert!(divergent_stack_numbers(repo).is_empty());
    }

    #[test]
    fn genuinely_differing_copy_across_sources_is_divergent() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .branch("feat-b")
            .gh_stack(None, 4, "main", &["feat-a"])
            .gh_stack(Some("feat-a"), 4, "main", &["feat-b"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        assert_eq!(divergent_stack_numbers(repo), vec![4]);
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

    #[test]
    fn worktree_state_is_clear_for_a_worktree_with_no_gh_stack_entries() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(None, 1, "main", &["feat-a"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        assert_eq!(
            worktree_state(repo, "feat-a"),
            WorktreeGhStackState::default()
        );
    }

    #[test]
    fn worktree_state_reports_symlinks_and_unlink_removes_only_them() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(None, 1, "main", &["feat-a"])
            .gh_stack_symlinked("feat-a")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let state = worktree_state(repo, "feat-a");
        assert!(state.symlinked);
        assert!(!state.legacy_file);

        let removed = unlink_worktree(repo, "feat-a").unwrap();
        assert_eq!(removed, ["gh-stack", "gh-stack.lock"]);
        assert_eq!(
            worktree_state(repo, "feat-a"),
            WorktreeGhStackState::default()
        );
        // The canonical catalog the links pointed at survives.
        assert!(canonical_path(repo).is_file());
    }

    #[test]
    fn unlink_worktree_leaves_a_real_legacy_file_alone() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack(Some("feat-a"), 2, "main", &["feat-a"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        let state = worktree_state(repo, "feat-a");
        assert!(state.legacy_file);
        assert!(!state.symlinked);

        assert!(unlink_worktree(repo, "feat-a").unwrap().is_empty());
        let file = repo.commondir().join("worktrees/feat-a/gh-stack");
        assert!(file.is_file());
    }

    #[test]
    fn worktree_state_reports_recovery_files() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .gh_stack_recovery_file("feat-a", "gh-stack-rebase-state")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        assert_eq!(
            worktree_state(repo, "feat-a").recovery_files,
            ["gh-stack-rebase-state"]
        );
    }

    // ── register_branch ─────────────────────────────────────────────────────────

    #[test]
    fn register_branch_appends_onto_a_trunk_with_no_branches_yet() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .gh_stack(None, 1, "main", &[])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        register_branch(repo, "feat-a", "main").unwrap();

        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-a", 0));
        let head_oid = repo
            .find_branch("feat-a", git2::BranchType::Local)
            .unwrap()
            .get()
            .target()
            .unwrap();
        repo.assert(predicate::repo::gh_stack_branch_base(
            None,
            "feat-a",
            head_oid.to_string(),
        ));
    }

    #[test]
    fn register_branch_appends_onto_the_top_of_an_existing_stack() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .gh_stack(None, 1, "main", &["feat-a"])
            .branch("feat-b")
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        register_branch(repo, "feat-b", "feat-a").unwrap();

        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-a", 0));
        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-b", 1));
    }

    #[test]
    fn register_branch_preserves_id_and_pull_request_on_untouched_entries() {
        // The existing entry's `id` and its branch's `pullRequest` are fields workon never
        // reads. A typed struct would silently drop them on write; the raw-Value round-trip
        // must not.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .branch("feat-b")
            .raw_gh_stack(
                None,
                br#"{
                    "schemaVersion": 1,
                    "stacks": [{
                        "id": "stack-abc",
                        "number": 3,
                        "trunk": { "branch": "main", "head": "", "base": "" },
                        "branches": [{
                            "branch": "feat-a",
                            "head": "0000000000000000000000000000000000000a",
                            "base": "0000000000000000000000000000000000000b",
                            "pullRequest": { "number": 42, "id": "PR_1", "merged": false }
                        }]
                    }]
                }"#
                .to_vec(),
            )
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        register_branch(repo, "feat-b", "feat-a").unwrap();

        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-a", 0));
        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-b", 1));
        repo.assert(predicate::repo::gh_stack_preserves(
            None,
            "/stacks/0/id",
            "stack-abc",
        ));
        repo.assert(predicate::repo::gh_stack_preserves(
            None,
            "/stacks/0/branches/0/pullRequest/number",
            "42",
        ));
    }

    #[test]
    fn register_branch_surfaces_parse_failed_for_truncated_canonical() {
        // A truncated canonical file under an uncontended lock is genuine corruption, not a
        // mid-write race (a lock-respecting writer can't be mid-write once we hold the lock).
        // The read-under-lock ordering means this must deterministically surface
        // GhStackParseFailed rather than a stale pre-lock snapshot masking the truncation.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .branch("feat-b")
            .gh_stack(None, 1, "main", &["feat-a"])
            .raw_gh_stack(None, b"{\"schemaVersion\": 1, \"stacks\": [".to_vec())
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        match register_branch(repo, "feat-b", "feat-a") {
            Err(StackError::GhStackParseFailed { .. }) => {}
            other => panic!("expected GhStackParseFailed, got {other:?}"),
        }
    }

    #[test]
    fn register_branch_errors_when_no_stack_ends_at_base() {
        // "main" is a real branch, but the only stack's last branch is "feat-a", not "main",
        // and its `branches` isn't empty, so "main" doesn't match either target-selection
        // rule.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .branch("feat-b")
            .gh_stack(None, 1, "main", &["feat-a"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        match register_branch(repo, "feat-b", "main") {
            Err(StackError::GhStackNoStackForBase { base }) => {
                assert_eq!(base, "main");
            }
            other => panic!("expected GhStackNoStackForBase, got {other:?}"),
        }
    }

    #[test]
    fn register_branch_points_at_doctor_fix_when_stack_is_unlinked_only() {
        // Finding F: the chicken-and-egg case the spec explicitly accepts — `gh stack init`
        // run inside a worktree before `doctor --fix` ever migrates it. `read_metadata` unions
        // in unlinked_files, so `list`/`find` render the stack correctly, but `register_branch`
        // reads canonical alone (it must never write through a worktree symlink) and would
        // otherwise report the same generic GhStackNoStackForBase as "no such stack anywhere",
        // which is indistinguishable from user error. It must instead point at `doctor --fix`.
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .worktree("feat-a")
            .branch("feat-b")
            .gh_stack(Some("feat-a"), 1, "main", &["feat-a"])
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        // Sanity: read_metadata (the list/find path) sees the stack fine via the degraded
        // union, so the failure below is specific to register_branch's canonical-only read.
        let meta = read_metadata(repo).unwrap();
        assert_eq!(meta.parents["feat-a"].parent, "main");

        match register_branch(repo, "feat-b", "feat-a") {
            Err(StackError::GhStackStackInUnlinkedWorktree { base }) => {
                assert_eq!(base, "feat-a");
            }
            other => panic!("expected GhStackStackInUnlinkedWorktree, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn register_branch_times_out_when_operation_lock_is_held() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .gh_stack(None, 1, "main", &[])
            .gh_stack_operation_lock_held()
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        match register_branch(repo, "feat-a", "main") {
            Err(StackError::GhStackLocked { path }) => {
                assert_eq!(path, repo.commondir().join("gh-stack-operation.lock"));
            }
            other => panic!("expected GhStackLocked, got {other:?}"),
        }
        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-a", 0).not());
    }

    #[test]
    fn register_branch_refuses_while_migration_journal_exists() {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .branch("feat-a")
            .gh_stack(None, 1, "main", &[])
            .gh_stack_migration_journal()
            .build()
            .unwrap();
        let repo = fixture.repo().unwrap();

        match register_branch(repo, "feat-a", "main") {
            Err(StackError::GhStackMigrationPending { path }) => {
                assert_eq!(path, repo.commondir().join("gh-stack-migration"));
            }
            other => panic!("expected GhStackMigrationPending, got {other:?}"),
        }
        repo.assert(predicate::repo::gh_stack_contains_branch(None, "feat-a", 0).not());
    }
}
