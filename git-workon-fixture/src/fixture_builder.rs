use crate::fixture::Fixture;
use assert_fs::TempDir;
use git2::{BranchType, ConfigLevel, Repository, WorktreeAddOptions};
use std::path::{Path, PathBuf};
use std::sync::Once;
use workon::empty_commit;

/// Point libgit2's system/global/XDG config search paths at an isolated directory, once per
/// process, so fixture repos never layer in the developer's real `~/.gitconfig` (a global
/// `workon.*` key would otherwise poison every "unset config" assertion in the workspace).
/// Only affects in-process git2 use — spawned `git` binaries still read the real files.
///
/// The isolated directory carries a minimal `user.name`/`user.email` at the Global level (not
/// truly empty): since the test harness now runs every `tests/*.rs` file as one merged binary
/// (single process) instead of one process per file, this isolation — which used to apply only
/// within whichever single file called it — now applies process-wide to every test, including
/// ones that never touch `FixtureBuilder` but still need `repo.signature()` to resolve (e.g.
/// `init.rs`'s direct exercise of `workon::init`/`empty_commit`). Those tests used to pass by
/// accident, reading the developer's real identity in their own isolated process; a fully empty
/// search path now makes them fail deterministically instead. A placeholder identity keeps this
/// isolated (never the developer's real config) while giving every in-process signature lookup
/// something to resolve.
fn isolate_ambient_git_config() {
    static ISOLATE: Once = Once::new();
    ISOLATE.call_once(|| {
        let empty = TempDir::new().expect("temp dir for git config isolation");
        std::fs::write(
            empty.path().join(".gitconfig"),
            "[user]\n\tname = git-workon-fixture\n\temail = fixture@git-workon.invalid\n\
             [init]\n\tdefaultBranch = main\n",
        )
        .expect("write isolated global git config");
        for level in [ConfigLevel::System, ConfigLevel::XDG, ConfigLevel::Global] {
            // SAFETY: mutates libgit2's process-global search paths; guarded by `ISOLATE`
            // and called from `build()` before this fixture's repo (and, in practice, any
            // other test's git2 work in this process) touches config discovery.
            unsafe { git2::opts::set_search_path(level, empty.path()) }
                .expect("set git config search path");
        }
        // Keep the (isolated) directory alive for the whole process — the search paths hold it.
        std::mem::forget(empty);
    });
}

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Create a symlink at `link` pointing at `target` (may be relative, may be dangling).
#[cfg(unix)]
fn symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Which on-disk format Graphite branch-metadata is written in.
///
/// Real `gt` versions before 1.8 wrote `refs/branch-metadata/<branch>` blobs; 1.8+ writes
/// a SQLite database at `<git-common-dir>/.graphite_metadata.db`. The fixture can emit either
/// so lib/review tests can be parameterized over both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataFormat {
    /// gt < 1.8: `refs/branch-metadata/<branch>` JSON blobs.
    #[default]
    Refs,
    /// gt >= 1.8: `<git-common-dir>/.graphite_metadata.db` SQLite database.
    Sqlite,
}

/// One tracked (or ghost) Graphite branch-metadata entry.
///
/// `branch_rev`/`parent_rev` of `None` resolve to the live `refs/heads/<name>` tip at
/// `build()` time, after all build-time commits have landed. Use
/// [`FixtureBuilder::branch_metadata_at`] to pin verbatim (possibly stale or bogus) strings.
#[derive(Debug, Clone)]
struct MetadataEntry {
    branch: String,
    parent: String,
    branch_rev: Option<String>,
    parent_rev: Option<String>,
    /// Ghost entries simulate a branch that Graphite still tracks but whose git ref was
    /// deleted (merged/removed). No local branch ref is created for these.
    ghost: bool,
}

/// A [`MetadataEntry`] with revisions resolved (or left as verbatim overrides), ready to
/// write in whichever [`MetadataFormat`] is active.
struct ResolvedMetadataEntry {
    branch: String,
    parent: String,
    branch_rev: Option<String>,
    parent_rev: Option<String>,
}

/// One `prInfos[]` entry for `.graphite_pr_info` (see [`FixtureBuilder::graphite_pr_info`]).
struct PrInfoEntry {
    branch: String,
    number: u64,
    title: String,
}

/// Which gh-stack file a fixture write targets.
///
/// `None` is the canonical store at `<common-dir>/gh-stack`; `Some(name)` is the per-worktree
/// file at `<common-dir>/worktrees/<name>/gh-stack` — mirroring what a real `gh stack`
/// invocation would write from inside that worktree before workon's symlink plumbing exists.
type GhStackTarget = Option<String>;

/// How a gh-stack `branchRef.base` is written for one fixtured branch.
#[derive(Debug, Clone)]
enum GhStackBase {
    /// Resolve the parent's live `refs/heads/<name>` tip at `build()` time, mirroring what
    /// `gh stack add`/`gh stack sync` record.
    ResolveParentTip,
    /// Write this string verbatim — stale or bogus `base` values for trap-style tests.
    Verbatim(String),
}

/// One `branchRef` entry queued for a [`GhStackStackSpec`].
#[derive(Debug, Clone)]
struct GhStackBranchSpec {
    branch: String,
    base: GhStackBase,
    /// Ghost entries simulate a branch gh-stack still tracks but whose git ref was
    /// deleted/merged — no local branch ref is created for these.
    ghost: bool,
}

/// One `stacks[]` entry queued by [`FixtureBuilder::gh_stack`]/[`FixtureBuilder::gh_stack_at`].
#[derive(Debug, Clone)]
struct GhStackStackSpec {
    number: u64,
    trunk: String,
    branches: Vec<GhStackBranchSpec>,
}

/// A queued gh-stack fixture write, replayed in call order at `build()` time (after branch
/// refs exist, so live-tip `base`/`head` resolution works the same way the Graphite block
/// resolves `parentBranchRevision`).
#[derive(Debug, Clone)]
enum GhStackOp {
    /// A whole new `stacks[]` entry for `target`.
    Stack {
        target: GhStackTarget,
        spec: GhStackStackSpec,
    },
    /// Append a ghost branch onto the `target` file's stack numbered `number` (which must
    /// already have been queued via a prior [`GhStackOp::Stack`]).
    GhostBranch {
        target: GhStackTarget,
        number: u64,
        branch: String,
    },
    /// Overwrite `target`'s file with raw bytes after all JSON writes — truncated, `v2`, or
    /// plain garbage content for error-path tests.
    Raw {
        target: GhStackTarget,
        bytes: Vec<u8>,
    },
    /// Take and hold `target`'s `gh-stack.lock` (unix only) for the fixture's lifetime, to
    /// exercise lock-contention handling.
    LockHeld { target: GhStackTarget },
    /// Plant `gh-stack`/`gh-stack.lock` in `worktree`'s admin dir as relative symlinks to the
    /// canonical store.
    Linked { worktree: String },
    /// Ensure `worktree`'s admin dir has a real (non-symlink) `gh-stack` file — a stand-in for
    /// migration-test fixtures when no explicit stack content targets that worktree.
    Unlinked { worktree: String },
    /// Ensure `worktree`'s admin dir has a real (non-symlink) `gh-stack.lock` file, replacing
    /// any symlink a prior `Linked` op planted there — the pre-migration lock layout
    /// `migrate_worktree` must also clean up (see the ADR-028 handoff's Finding A).
    LockUnlinked { worktree: String },
}

/// Represents a remote URL source
pub enum RemoteSource {
    /// Path to another repository (from a Fixture)
    Path(PathBuf),
    /// URL string
    Url(String),
}

impl From<&Fixture> for RemoteSource {
    fn from(fixture: &Fixture) -> Self {
        match fixture.repo().unwrap().is_bare() {
            true => RemoteSource::Path(fixture.root().unwrap().path().join(".bare")),
            false => RemoteSource::Path(fixture.root().unwrap().to_path_buf()),
        }
    }
}

impl From<&str> for RemoteSource {
    fn from(s: &str) -> Self {
        RemoteSource::Url(s.to_string())
    }
}

impl From<String> for RemoteSource {
    fn from(s: String) -> Self {
        RemoteSource::Url(s)
    }
}

impl RemoteSource {
    fn as_url(&self) -> String {
        match self {
            RemoteSource::Path(p) => p.to_string_lossy().to_string(),
            RemoteSource::Url(s) => s.clone(),
        }
    }
}

pub struct FixtureBuilder<'fixture> {
    bare: bool,
    default_branch: &'fixture str,
    worktrees: Vec<&'fixture str>,
    branches: Vec<&'fixture str>, // local branches without worktrees
    remotes: Vec<(String, RemoteSource)>,
    upstreams: Vec<(String, String)>, // (local_branch, remote_branch)
    configs: Vec<(String, String)>,   // (key, value) for git config
    graphite_config: Option<Vec<String>>, // trunk branch names for .graphite_repo_config
    metadata_format: MetadataFormat,
    metadata_entries: Vec<MetadataEntry>,
    raw_branch_metadata: Vec<(String, Vec<u8>)>, // (branch, raw_bytes) for malformed-blob tests
    pr_infos: Vec<PrInfoEntry>,
    staged_files: Vec<(String, String)>, // (path, content)
    unstaged_files: Vec<(String, String, String)>, // (path, committed, modified)
    untracked_files: Vec<(String, String)>, // (path, content)
    deleted_files: Vec<(String, String)>, // (path, committed)
    gh_stack_ops: Vec<GhStackOp>,
}

impl<'fixture> FixtureBuilder<'fixture> {
    pub fn new() -> Self {
        Self {
            bare: false,
            default_branch: "main",
            worktrees: Vec::new(),
            branches: Vec::new(),
            remotes: Vec::new(),
            upstreams: Vec::new(),
            configs: Vec::new(),
            graphite_config: None,
            metadata_format: MetadataFormat::default(),
            metadata_entries: Vec::new(),
            raw_branch_metadata: Vec::new(),
            pr_infos: Vec::new(),
            staged_files: Vec::new(),
            unstaged_files: Vec::new(),
            untracked_files: Vec::new(),
            deleted_files: Vec::new(),
            gh_stack_ops: Vec::new(),
        }
    }

    /// Select the on-disk format for Graphite branch-metadata written by
    /// [`branch_metadata`](Self::branch_metadata), [`ghost_branch_metadata`](Self::ghost_branch_metadata),
    /// and [`branch_metadata_at`](Self::branch_metadata_at). Defaults to [`MetadataFormat::Refs`].
    pub fn metadata_format(mut self, format: MetadataFormat) -> Self {
        self.metadata_format = format;
        self
    }

    pub fn bare(mut self, bare: bool) -> Self {
        self.bare = bare;
        self
    }

    pub fn default_branch(mut self, default_branch: &'fixture str) -> Self {
        self.default_branch = default_branch;
        self
    }

    /// Add a worktree to be created
    /// Can be called multiple times to create multiple worktrees
    /// The Fixture will be opened in the last worktree specified
    pub fn worktree(mut self, worktree: &'fixture str) -> Self {
        self.worktrees.push(worktree);
        self
    }

    /// Create a local branch at HEAD without a worktree.
    ///
    /// Use this to set up "branch exists but no worktree" scenarios for testing
    /// the existing-branch worktree creation flow.
    pub fn branch(mut self, branch: &'fixture str) -> Self {
        self.branches.push(branch);
        self
    }

    /// Add a remote to the repository
    pub fn remote(mut self, name: &str, source: impl Into<RemoteSource>) -> Self {
        self.remotes.push((name.to_string(), source.into()));
        self
    }

    /// Configure upstream tracking for a branch
    /// The remote branch will be created automatically at the current branch HEAD
    pub fn upstream(mut self, branch: &str, remote_branch: &str) -> Self {
        self.upstreams
            .push((branch.to_string(), remote_branch.to_string()));
        self
    }

    /// Set a git config value in the repository
    /// Can be called multiple times with the same key for multi-value configs
    pub fn config(mut self, key: &str, value: &str) -> Self {
        self.configs.push((key.to_string(), value.to_string()));
        self
    }

    /// Write `.graphite_repo_config` marking this as a Graphite-managed repository.
    ///
    /// `trunks` is the list of trunk branch names (typically `["main"]`).
    pub fn graphite_config(mut self, trunks: &[&str]) -> Self {
        self.graphite_config = Some(trunks.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Write Graphite branch-metadata for `branch` with parent `parent`, in the active
    /// [`MetadataFormat`] (see [`metadata_format`](Self::metadata_format)).
    ///
    /// Revisions resolve to the live `refs/heads/<name>` tips at `build()` time, mirroring
    /// what `gt track` writes right after tracking a branch. `parentBranchRevision` is
    /// persisted in both formats; `branch_revision` only in [`MetadataFormat::Sqlite`] —
    /// legacy refs blobs have no branch-revision field.
    /// Also creates a local branch ref for `branch` if one does not already exist, so
    /// metadata-only stack diffs resolve as live branches rather than ghosts.
    pub fn branch_metadata(mut self, branch: &str, parent: &str) -> Self {
        self.metadata_entries.push(MetadataEntry {
            branch: branch.to_string(),
            parent: parent.to_string(),
            branch_rev: None,
            parent_rev: None,
            ghost: false,
        });
        self
    }

    /// Write Graphite branch-metadata for `branch` with parent `parent`, pinning **verbatim**
    /// revision strings instead of resolving live tips.
    ///
    /// Use this to fixture stale or bogus recorded revisions (e.g. a `branch_revision` left
    /// behind after plain-git commits advanced the branch past what Graphite recorded, or a
    /// non-resolving hex string) for trap-7-style tests. `parent_rev` is written exactly as
    /// given in either format; `branch_rev` is persisted only in [`MetadataFormat::Sqlite`]
    /// (legacy refs blobs have no branch-revision field, so it is ignored in
    /// [`MetadataFormat::Refs`]). Neither needs to resolve to a real commit. Also creates a
    /// local branch ref for `branch` if one does not already exist, same as
    /// [`branch_metadata`](Self::branch_metadata).
    pub fn branch_metadata_at(
        mut self,
        branch: &str,
        parent: &str,
        branch_rev: &str,
        parent_rev: &str,
    ) -> Self {
        self.metadata_entries.push(MetadataEntry {
            branch: branch.to_string(),
            parent: parent.to_string(),
            branch_rev: Some(branch_rev.to_string()),
            parent_rev: Some(parent_rev.to_string()),
            ghost: false,
        });
        self
    }

    /// Write Graphite branch-metadata for a **deleted** branch (ghost entry), in the active
    /// [`MetadataFormat`].
    ///
    /// Same as [`branch_metadata`] but intentionally does NOT create a local git branch ref.
    /// Use this to simulate a branch that was merged/deleted after being tracked by Graphite —
    /// metadata lingers (a blob or a db row) but the branch ref is gone.
    /// This is the precondition for the `DeletedNode` resolution and ghost-branch filtering tests.
    ///
    /// [`branch_metadata`]: Self::branch_metadata
    pub fn ghost_branch_metadata(mut self, branch: &str, parent: &str) -> Self {
        self.metadata_entries.push(MetadataEntry {
            branch: branch.to_string(),
            parent: parent.to_string(),
            branch_rev: None,
            parent_rev: None,
            ghost: true,
        });
        self
    }

    /// Write a raw blob at `refs/branch-metadata/<branch>`.
    ///
    /// Use this to simulate malformed metadata (e.g. non-JSON content) for error-path tests.
    /// Refs-only: writes the legacy blob format regardless of [`metadata_format`](Self::metadata_format),
    /// since malformed sqlite rows aren't a meaningful scenario to fixture the same way.
    pub fn raw_branch_metadata(mut self, branch: &str, bytes: Vec<u8>) -> Self {
        self.raw_branch_metadata.push((branch.to_string(), bytes));
        self
    }

    /// Append a PR entry to `.graphite_pr_info` in the main repo's git dir.
    ///
    /// Repeated calls accumulate into one `prInfos` array, mirroring how `gt` maintains a
    /// single file across multiple tracked PRs. `body` is always written as an empty string.
    pub fn graphite_pr_info(mut self, branch: &str, number: u64, title: &str) -> Self {
        self.pr_infos.push(PrInfoEntry {
            branch: branch.to_string(),
            number,
            title: title.to_string(),
        });
        self
    }

    /// Queue a `stacks[]` entry for `worktree`'s gh-stack file (`None` = canonical
    /// `<common-dir>/gh-stack`, `Some(name)` = `<common-dir>/worktrees/<name>/gh-stack`).
    ///
    /// `base` for each branch resolves to its parent's live `refs/heads/<name>` tip at
    /// `build()` time (parent is the previous entry in `branches`, or `trunk` for the first).
    /// Repeated calls with the same `worktree` accumulate into that file's `stacks` array; a
    /// local branch ref is created for each entry that doesn't already have one, mirroring
    /// [`branch_metadata`](Self::branch_metadata). Use [`gh_stack_at`](Self::gh_stack_at) to
    /// pin verbatim/stale `base` values instead.
    pub fn gh_stack(
        mut self,
        worktree: Option<&str>,
        number: u64,
        trunk: &str,
        branches: &[&str],
    ) -> Self {
        let spec = GhStackStackSpec {
            number,
            trunk: trunk.to_string(),
            branches: branches
                .iter()
                .map(|branch| GhStackBranchSpec {
                    branch: branch.to_string(),
                    base: GhStackBase::ResolveParentTip,
                    ghost: false,
                })
                .collect(),
        };
        self.gh_stack_ops.push(GhStackOp::Stack {
            target: worktree.map(str::to_string),
            spec,
        });
        self
    }

    /// Same as [`gh_stack`](Self::gh_stack), but `branches` pairs each branch with a verbatim
    /// `base` string instead of resolving the parent's live tip — for stale/bogus-`base`
    /// (`needs_restack`) trap tests.
    pub fn gh_stack_at(
        mut self,
        worktree: Option<&str>,
        number: u64,
        trunk: &str,
        branches: &[(&str, &str)],
    ) -> Self {
        let spec = GhStackStackSpec {
            number,
            trunk: trunk.to_string(),
            branches: branches
                .iter()
                .map(|(branch, base)| GhStackBranchSpec {
                    branch: branch.to_string(),
                    base: GhStackBase::Verbatim(base.to_string()),
                    ghost: false,
                })
                .collect(),
        };
        self.gh_stack_ops.push(GhStackOp::Stack {
            target: worktree.map(str::to_string),
            spec,
        });
        self
    }

    /// Append a ghost branch (no local branch ref) onto `worktree`'s stack numbered `number`.
    ///
    /// A [`gh_stack`](Self::gh_stack)/[`gh_stack_at`](Self::gh_stack_at) call for the same
    /// `(worktree, number)` must be queued first — `build()` panics otherwise, since there is
    /// no trunk to derive the ghost's position from.
    pub fn gh_stack_ghost_branch(
        mut self,
        worktree: Option<&str>,
        number: u64,
        branch: &str,
    ) -> Self {
        self.gh_stack_ops.push(GhStackOp::GhostBranch {
            target: worktree.map(str::to_string),
            number,
            branch: branch.to_string(),
        });
        self
    }

    /// Overwrite `worktree`'s gh-stack file with raw `bytes` after all other gh-stack writes —
    /// for truncated-read, `schemaVersion: 2`, and garbage-content error-path tests.
    pub fn raw_gh_stack(mut self, worktree: Option<&str>, bytes: Vec<u8>) -> Self {
        self.gh_stack_ops.push(GhStackOp::Raw {
            target: worktree.map(str::to_string),
            bytes,
        });
        self
    }

    /// Take and hold `worktree`'s `gh-stack.lock` (`flock(LOCK_EX)`) for the fixture's
    /// lifetime, to exercise lock-contention handling in the write path. Unix only.
    #[cfg(unix)]
    pub fn gh_stack_lock_held(mut self, worktree: Option<&str>) -> Self {
        self.gh_stack_ops.push(GhStackOp::LockHeld {
            target: worktree.map(str::to_string),
        });
        self
    }

    /// Plant `gh-stack`/`gh-stack.lock` in `worktree`'s admin dir as relative symlinks
    /// (`../../gh-stack`, `../../gh-stack.lock`) resolving to the canonical store — the
    /// healthy-path layout `link_worktree` produces.
    pub fn gh_stack_linked(mut self, worktree: &str) -> Self {
        self.gh_stack_ops.push(GhStackOp::Linked {
            worktree: worktree.to_string(),
        });
        self
    }

    /// Ensure `worktree`'s admin dir has a real (non-symlink) `gh-stack` file — the
    /// pre-migration layout `doctor --fix` / `migrate_worktree` targets. A no-op beyond
    /// ensuring the file exists if [`gh_stack`](Self::gh_stack)/[`gh_stack_at`](Self::gh_stack_at)
    /// already queued content for this worktree.
    pub fn gh_stack_unlinked(mut self, worktree: &str) -> Self {
        self.gh_stack_ops.push(GhStackOp::Unlinked {
            worktree: worktree.to_string(),
        });
        self
    }

    /// Ensure `worktree`'s admin dir has a real (non-symlink) `gh-stack.lock` file — the
    /// pre-migration layout for the lock path, mirroring [`gh_stack_unlinked`](Self::gh_stack_unlinked)
    /// but for `gh-stack.lock` instead of `gh-stack`. Its content is irrelevant (upstream never
    /// reads it, it's a pure `flock` target) so this writes an empty file. Runs after `Linked`,
    /// so it also covers "gh-stack is linked but gh-stack.lock reverted to a real file."
    pub fn gh_stack_lock_unlinked(mut self, worktree: &str) -> Self {
        self.gh_stack_ops.push(GhStackOp::LockUnlinked {
            worktree: worktree.to_string(),
        });
        self
    }

    /// Write `path` with `content` in the fixture's cwd repo working tree and stage it
    /// (added to the index but not committed).
    ///
    /// Applies to the LAST worktree added, or the main repo if none. Errors at [`build`](Self::build)
    /// if the fixture is `bare(true)` with no worktree (there is no working tree to stage into).
    pub fn staged_file(mut self, path: &str, content: &str) -> Self {
        self.staged_files
            .push((path.to_string(), content.to_string()));
        self
    }

    /// Commit `path` with `committed` content on the cwd repo's branch during `build()`
    /// (moving the branch tip), then rewrite the working tree copy to `modified` — an
    /// unstaged modification with a clean index entry for `path`.
    ///
    /// The baseline commit lands before Graphite-metadata live-tip resolution, so any
    /// `branch_metadata` entry recording this branch's tip reflects the moved tip, not the
    /// pre-baseline one. Applies to the LAST worktree added, or the main repo if none. Errors
    /// at [`build`](Self::build) if the fixture is `bare(true)` with no worktree.
    pub fn unstaged_file(mut self, path: &str, committed: &str, modified: &str) -> Self {
        self.unstaged_files.push((
            path.to_string(),
            committed.to_string(),
            modified.to_string(),
        ));
        self
    }

    /// Write `path` with `content` in the fixture's cwd repo working tree; never staged
    /// (untracked).
    ///
    /// Applies to the LAST worktree added, or the main repo if none. Errors at [`build`](Self::build)
    /// if the fixture is `bare(true)` with no worktree.
    pub fn untracked_file(mut self, path: &str, content: &str) -> Self {
        self.untracked_files
            .push((path.to_string(), content.to_string()));
        self
    }

    /// Commit `path` with `committed_content` on the cwd repo's branch during `build()`
    /// (moving the branch tip, in the same baseline-commit block as
    /// [`unstaged_file`](Self::unstaged_file)), then remove it from the working tree — a
    /// staged-clean, working-tree-deleted file (`WT_DELETED`).
    ///
    /// The baseline commit lands before Graphite-metadata live-tip resolution, same rationale
    /// as [`unstaged_file`](Self::unstaged_file). Applies to the LAST worktree added, or the
    /// main repo if none. Errors at [`build`](Self::build) if the fixture is `bare(true)` with
    /// no worktree.
    pub fn deleted_file(mut self, path: &str, committed_content: &str) -> Self {
        self.deleted_files
            .push((path.to_string(), committed_content.to_string()));
        self
    }

    pub fn build(self) -> Result<Fixture> {
        isolate_ambient_git_config();
        let tmpdir = TempDir::new()?;
        let path = tmpdir.path().join(if self.bare {
            ".bare"
        } else {
            self.default_branch
        });
        let repo = if self.bare {
            Repository::init_bare(&path)?
        } else {
            Repository::init(&path)?
        };

        let mut config = repo.config()?;

        config.set_str("user.name", "git-workon-fixture")?;

        config.set_str("user.email", "git-workon-fixture@fake.com")?;

        // Apply custom configs
        for (key, value) in &self.configs {
            // Use set_multivar to support multi-value configs
            // The regex "^$" matches nothing, so this always adds a new value
            config.set_multivar(key, "^$", value)?;
        }

        empty_commit(&repo)?;

        if repo
            .find_branch(self.default_branch, BranchType::Local)
            .is_err()
        {
            let head = repo.head()?;
            let head_ref = head.shorthand().unwrap_or("");
            if head_ref != self.default_branch {
                if let Ok(mut branch) = repo.find_branch(head_ref, BranchType::Local) {
                    branch.rename(self.default_branch, true)?;
                }
                if !self.bare {
                    repo.set_head(&format!("refs/heads/{}", self.default_branch))?;
                }
            }
        }

        // Create worktrees
        for worktree in &self.worktrees {
            if *worktree == self.default_branch && !self.bare {
                return Err(format!(
                        "Cannot create a worktree with the same name as the default branch ({}) in a non-bare repository",
                        self.default_branch
                    ).into());
            }

            let worktree_path = tmpdir.path().join(worktree);
            let mut worktree_opts = WorktreeAddOptions::new();
            worktree_opts.checkout_existing(self.bare);

            repo.worktree(worktree, &worktree_path, Some(&worktree_opts))?;
        }

        // Create local branches without worktrees
        for branch_name in &self.branches {
            let head = repo.head()?;
            let commit = head.peel_to_commit()?;
            repo.branch(branch_name, &commit, false)?;
        }

        // Apply remotes
        for (name, source) in &self.remotes {
            repo.remote(name, &source.as_url())?;
        }

        // Apply upstreams
        for (branch, remote_branch) in &self.upstreams {
            // Get the current commit of the local branch
            let local_branch = repo.find_branch(branch, BranchType::Local)?;
            let commit_oid = local_branch
                .get()
                .target()
                .ok_or_else(|| format!("Branch {} has no target", branch))?;

            // Create the remote tracking ref at the same commit
            let remote_ref = if remote_branch.starts_with("refs/") {
                remote_branch.clone()
            } else {
                format!("refs/remotes/{}", remote_branch)
            };

            repo.reference(&remote_ref, commit_oid, false, "create remote ref")?;

            // Set upstream tracking
            let mut local_branch = repo.find_branch(branch, BranchType::Local)?;
            local_branch.set_upstream(Some(remote_branch))?;
        }

        // Resolve the fixture's cwd repo path — the LAST worktree added, or the main repo if
        // none. Index-state builders (staged/unstaged/untracked files) apply here.
        let cwd_path = if self.worktrees.is_empty() {
            path.clone()
        } else {
            tmpdir.path().join(self.worktrees.last().unwrap())
        };

        let has_index_state = !self.staged_files.is_empty()
            || !self.unstaged_files.is_empty()
            || !self.untracked_files.is_empty()
            || !self.deleted_files.is_empty();
        if has_index_state && self.bare && self.worktrees.is_empty() {
            return Err(
                "staged_file/unstaged_file/untracked_file/deleted_file require a working tree: \
                 fixture is bare(true) with no worktree"
                    .into(),
            );
        }

        // `unstaged_file`/`deleted_file` baseline commits land BEFORE Graphite-metadata
        // live-tip resolution below: they move the cwd branch's tip, and any metadata entry
        // recording that tip must reflect the moved one, not the pre-baseline commit. Both
        // builders share one baseline commit.
        if !self.unstaged_files.is_empty() || !self.deleted_files.is_empty() {
            let cwd_repo = Repository::open(&cwd_path)?;
            let mut index = cwd_repo.index()?;
            for (file_path, committed, _modified) in &self.unstaged_files {
                let abs_path = cwd_path.join(file_path);
                if let Some(parent) = abs_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&abs_path, committed)?;
                index.add_path(Path::new(file_path))?;
            }
            for (file_path, committed) in &self.deleted_files {
                let abs_path = cwd_path.join(file_path);
                if let Some(parent) = abs_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&abs_path, committed)?;
                index.add_path(Path::new(file_path))?;
            }
            index.write()?;

            let tree_id = index.write_tree()?;
            let tree = cwd_repo.find_tree(tree_id)?;
            let sig = cwd_repo.signature()?;
            let parent_commit = cwd_repo.head()?.peel_to_commit()?;
            cwd_repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                "unstaged_file baseline",
                &tree,
                &[&parent_commit],
            )?;
        }

        // Write .graphite_repo_config
        if let Some(trunks) = &self.graphite_config {
            let trunk_objects: Vec<serde_json::Value> = trunks
                .iter()
                .map(|t| serde_json::json!({"name": t}))
                .collect();
            let config_json = serde_json::json!({
                "trunk": trunks.first().map(|s| s.as_str()).unwrap_or("main"),
                "trunks": trunk_objects,
            });
            let config_path = repo.path().join(".graphite_repo_config");
            std::fs::write(&config_path, config_json.to_string())?;
        }

        // Create local branch refs for tracked (non-ghost) metadata entries before resolving
        // live tips below. Trunk and worktree branches already have refs; metadata-only stack
        // diffs do not — create a local branch pointing to HEAD for them. branch_exists() then
        // correctly identifies live branches vs ghost entries in tests.
        for entry in &self.metadata_entries {
            if !entry.ghost && repo.find_branch(&entry.branch, BranchType::Local).is_err() {
                let head = repo.head()?;
                let commit = head.peel_to_commit()?;
                repo.branch(&entry.branch, &commit, false)?;
            }
        }

        // Resolve live `refs/heads/<name>` tips for entries that didn't pin a verbatim
        // revision. Ghost entries have no branch ref to resolve `branch_rev` from, so theirs
        // stays None unless a verbatim value was given.
        let resolve_tip = |name: &str| -> Option<String> {
            repo.find_branch(name, BranchType::Local)
                .ok()
                .and_then(|b| b.get().target())
                .map(|oid| oid.to_string())
        };
        let resolved_metadata: Vec<ResolvedMetadataEntry> = self
            .metadata_entries
            .iter()
            .map(|entry| ResolvedMetadataEntry {
                branch: entry.branch.clone(),
                parent: entry.parent.clone(),
                branch_rev: entry
                    .branch_rev
                    .clone()
                    .or_else(|| resolve_tip(&entry.branch)),
                parent_rev: entry
                    .parent_rev
                    .clone()
                    .or_else(|| resolve_tip(&entry.parent)),
            })
            .collect();

        // Write Graphite branch-metadata in the active format.
        match self.metadata_format {
            MetadataFormat::Refs => {
                for entry in &resolved_metadata {
                    let content = serde_json::json!({
                        "branchName": entry.branch,
                        "parentBranchName": entry.parent,
                        "parentBranchRevision": entry.parent_rev,
                    })
                    .to_string();
                    let oid = repo.blob(content.as_bytes())?;
                    repo.reference(
                        &format!("refs/branch-metadata/{}", entry.branch),
                        oid,
                        false,
                        "add branch metadata",
                    )?;
                }
            }
            MetadataFormat::Sqlite => {
                let db_path = repo.path().join(".graphite_metadata.db");
                let conn = rusqlite::Connection::open(&db_path)?;
                conn.execute_batch(
                    r#"CREATE TABLE "branch_metadata" ("branch_name" text not null primary key,
                      "parent_branch_name" text, "parent_branch_revision" text,
                      "last_submitted_version" text, "state" text, "children" text,
                      "branch_revision" text, "validation_result" text, "parent_head_revision" text);
                    CREATE INDEX "idx_branch_metadata_parent" on "branch_metadata" ("parent_branch_name");"#,
                )?;
                for entry in &resolved_metadata {
                    conn.execute(
                        "INSERT INTO branch_metadata \
                         (branch_name, parent_branch_name, parent_branch_revision, branch_revision) \
                         VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![
                            entry.branch,
                            entry.parent,
                            entry.parent_rev,
                            entry.branch_rev
                        ],
                    )?;
                }
                // One trunk row per configured trunk, with the real-gt-faithful empty-string
                // parent (NOT NULL) — the lib's parent-name filter excludes both NULL and ''.
                if let Some(trunks) = &self.graphite_config {
                    for trunk in trunks {
                        let branch_rev = resolve_tip(trunk);
                        conn.execute(
                            "INSERT INTO branch_metadata (branch_name, parent_branch_name, branch_revision) \
                             VALUES (?1, '', ?2)",
                            rusqlite::params![trunk, branch_rev],
                        )?;
                    }
                }
            }
        }

        // Write raw branch-metadata blobs (for malformed-content tests)
        for (branch, bytes) in &self.raw_branch_metadata {
            let oid = repo.blob(bytes)?;
            repo.reference(
                &format!("refs/branch-metadata/{branch}"),
                oid,
                false,
                "add raw branch metadata",
            )?;
        }

        // Write .graphite_pr_info (PR titles, read by branch name)
        if !self.pr_infos.is_empty() {
            let pr_info_objects: Vec<serde_json::Value> = self
                .pr_infos
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "headRefName": entry.branch,
                        "number": entry.number,
                        "title": entry.title,
                        "body": "",
                    })
                })
                .collect();
            let pr_info_json = serde_json::json!({ "prInfos": pr_info_objects });
            let pr_info_path = repo.path().join(".graphite_pr_info");
            std::fs::write(&pr_info_path, pr_info_json.to_string())?;
        }

        // Write gh-stack files: canonical (`<common-dir>/gh-stack`) and/or per-worktree
        // (`<common-dir>/worktrees/<name>/gh-stack`), grouped by target and written after
        // branch refs exist so `resolve_tip` sees live tips. `gh_stack`/`gh_stack_at` calls
        // accumulate into one `stacks` array per target, in call order; `Raw` overwrites
        // whatever JSON was written for its target; `Linked`/`Unlinked` run last, after every
        // file exists, so symlink planting can see (or knowingly precede — dangling is valid)
        // the canonical file.
        if !self.gh_stack_ops.is_empty() {
            fn gh_stack_target_path(repo: &Repository, target: &GhStackTarget) -> PathBuf {
                match target {
                    None => repo.commondir().join("gh-stack"),
                    Some(name) => repo
                        .commondir()
                        .join("worktrees")
                        .join(name)
                        .join("gh-stack"),
                }
            }

            fn branch_ref_json(branch: &str, head: String, base: String) -> serde_json::Value {
                serde_json::json!({
                    "branch": branch,
                    "head": head,
                    "base": base,
                    "pullRequest": serde_json::Value::Null,
                })
            }

            // Create local branch refs for tracked (non-ghost) gh-stack entries before
            // resolving live tips, mirroring the Graphite block above. Ghost entries and
            // trunks are left alone — trunks are expected to already exist from repo/worktree
            // setup, ghosts intentionally have no ref.
            for op in &self.gh_stack_ops {
                if let GhStackOp::Stack { spec, .. } = op {
                    for b in &spec.branches {
                        if !b.ghost && repo.find_branch(&b.branch, BranchType::Local).is_err() {
                            let head = repo.head()?;
                            let commit = head.peel_to_commit()?;
                            repo.branch(&b.branch, &commit, false)?;
                        }
                    }
                }
            }

            // Group queued `Stack` entries by target, preserving call order (dedupe/identity
            // rules live in the lib, not the fixture — this just replays what was queued).
            let mut stacks_by_target: Vec<(GhStackTarget, Vec<GhStackStackSpec>)> = Vec::new();
            let target_index = |stacks_by_target: &[(GhStackTarget, Vec<GhStackStackSpec>)],
                                target: &GhStackTarget| {
                stacks_by_target.iter().position(|(t, _)| t == target)
            };

            for op in &self.gh_stack_ops {
                match op {
                    GhStackOp::Stack { target, spec } => {
                        match target_index(&stacks_by_target, target) {
                            Some(i) => stacks_by_target[i].1.push(spec.clone()),
                            None => stacks_by_target.push((target.clone(), vec![spec.clone()])),
                        }
                    }
                    GhStackOp::GhostBranch {
                        target,
                        number,
                        branch,
                    } => {
                        let i = target_index(&stacks_by_target, target).unwrap_or_else(|| {
                            panic!(
                                "gh_stack_ghost_branch({target:?}, {number}, {branch:?}): \
                                 no prior gh_stack/gh_stack_at queued a stack numbered {number} \
                                 for this target"
                            )
                        });
                        let entries = &mut stacks_by_target[i].1;
                        let spec = entries
                            .iter_mut()
                            .find(|s| s.number == *number)
                            .unwrap_or_else(|| {
                                panic!(
                                    "gh_stack_ghost_branch({target:?}, {number}, {branch:?}): \
                                     no queued stack numbered {number} for this target"
                                )
                            });
                        spec.branches.push(GhStackBranchSpec {
                            branch: branch.clone(),
                            base: GhStackBase::ResolveParentTip,
                            ghost: true,
                        });
                    }
                    _ => {}
                }
            }

            for (target, specs) in &stacks_by_target {
                let stacks_json: Vec<serde_json::Value> = specs
                    .iter()
                    .map(|spec| {
                        let trunk_tip = resolve_tip(&spec.trunk).unwrap_or_default();
                        let branches_json: Vec<serde_json::Value> = spec
                            .branches
                            .iter()
                            .enumerate()
                            .map(|(i, b)| {
                                let parent = if i == 0 {
                                    spec.trunk.as_str()
                                } else {
                                    spec.branches[i - 1].branch.as_str()
                                };
                                let base = match &b.base {
                                    GhStackBase::Verbatim(s) => s.clone(),
                                    GhStackBase::ResolveParentTip => {
                                        resolve_tip(parent).unwrap_or_default()
                                    }
                                };
                                let head = resolve_tip(&b.branch).unwrap_or_default();
                                branch_ref_json(&b.branch, head, base)
                            })
                            .collect();
                        serde_json::json!({
                            "id": format!("id-{}", spec.number),
                            "number": spec.number,
                            "trunk": branch_ref_json(&spec.trunk, trunk_tip, String::new()),
                            "branches": branches_json,
                        })
                    })
                    .collect();
                let doc = serde_json::json!({
                    "schemaVersion": 1,
                    "repository": "git-workon-fixture/gh-stack",
                    "stacks": stacks_json,
                });
                let path = gh_stack_target_path(&repo, target);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, doc.to_string())?;
            }

            // `Unlinked` ensures a real file exists even when no `Stack`/`GhostBranch` op
            // targeted that worktree — write a minimal valid empty-stacks doc.
            for op in &self.gh_stack_ops {
                if let GhStackOp::Unlinked { worktree } = op {
                    let target = Some(worktree.clone());
                    let path = gh_stack_target_path(&repo, &target);
                    if !path.exists() {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        let doc = serde_json::json!({
                            "schemaVersion": 1,
                            "repository": "git-workon-fixture/gh-stack",
                            "stacks": [],
                        });
                        std::fs::write(&path, doc.to_string())?;
                    }
                }
            }

            // `Raw` overwrites whatever was just written for its target, last write wins.
            for op in &self.gh_stack_ops {
                if let GhStackOp::Raw { target, bytes } = op {
                    let path = gh_stack_target_path(&repo, target);
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, bytes)?;
                }
            }

            // `Linked` plants relative symlinks resolving to the canonical store. A dangling
            // symlink (canonical not yet written) is intentionally valid — see the ADR-028
            // handoff's "shared canonical file" self-healing note.
            for op in &self.gh_stack_ops {
                if let GhStackOp::Linked { worktree } = op {
                    let admin_dir = repo.commondir().join("worktrees").join(worktree);
                    std::fs::create_dir_all(&admin_dir)?;
                    for (name, relative_target) in [
                        ("gh-stack", "../../gh-stack"),
                        ("gh-stack.lock", "../../gh-stack.lock"),
                    ] {
                        let link_path = admin_dir.join(name);
                        let _ = std::fs::remove_file(&link_path);
                        symlink(relative_target, &link_path)?;
                    }
                }
            }

            // `LockUnlinked` runs after `Linked` so it can also model a `gh-stack.lock` that
            // reverted from a symlink back to a real file (removing any symlink `Linked` left).
            for op in &self.gh_stack_ops {
                if let GhStackOp::LockUnlinked { worktree } = op {
                    let admin_dir = repo.commondir().join("worktrees").join(worktree);
                    std::fs::create_dir_all(&admin_dir)?;
                    let lock_path = admin_dir.join("gh-stack.lock");
                    let _ = std::fs::remove_file(&lock_path);
                    std::fs::write(&lock_path, [])?;
                }
            }

            #[cfg(unix)]
            for op in &self.gh_stack_ops {
                if let GhStackOp::LockHeld { target } = op {
                    let admin_target = match target {
                        None => repo.commondir().to_path_buf(),
                        Some(name) => repo.commondir().join("worktrees").join(name),
                    };
                    std::fs::create_dir_all(&admin_target)?;
                    let lock_path = admin_target.join("gh-stack.lock");
                    let file = std::fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(false)
                        .open(&lock_path)?;
                    // SAFETY: `flock` on a freshly opened fd we own; leaked below so the lock
                    // is held for the fixture's (process) lifetime, matching a real concurrent
                    // `gh stack` process for tests exercising lock-contention handling.
                    let rc = unsafe {
                        libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&file), libc::LOCK_EX)
                    };
                    if rc != 0 {
                        return Err(std::io::Error::last_os_error().into());
                    }
                    std::mem::forget(file);
                }
            }
        }

        // Index mutations last: staged adds, unstaged working-tree rewrites, untracked
        // writes. These run after metadata writes so they never affect resolved revisions.
        if has_index_state {
            let cwd_repo = Repository::open(&cwd_path)?;

            if !self.staged_files.is_empty() {
                let mut index = cwd_repo.index()?;
                for (file_path, content) in &self.staged_files {
                    let abs_path = cwd_path.join(file_path);
                    if let Some(parent) = abs_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&abs_path, content)?;
                    index.add_path(Path::new(file_path))?;
                }
                index.write()?;
            }

            for (file_path, _committed, modified) in &self.unstaged_files {
                std::fs::write(cwd_path.join(file_path), modified)?;
            }

            for (file_path, content) in &self.untracked_files {
                let abs_path = cwd_path.join(file_path);
                if let Some(parent) = abs_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&abs_path, content)?;
            }

            for (file_path, _committed) in &self.deleted_files {
                std::fs::remove_file(cwd_path.join(file_path))?;
            }
        }

        if self.worktrees.is_empty() {
            // No worktrees specified - return the main repo
            Ok(Fixture::new(repo, path, tmpdir))
        } else {
            // Open the repository from the worktree path instead of using the bare/main repo
            let worktree_repo = Repository::open(&cwd_path)?;
            Ok(Fixture::new(worktree_repo, cwd_path, tmpdir))
        }
    }
}

impl<'fixture> Default for FixtureBuilder<'fixture> {
    fn default() -> Self {
        Self::new()
    }
}
