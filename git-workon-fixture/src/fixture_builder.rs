use crate::fixture::Fixture;
use assert_fs::TempDir;
use git2::{BranchType, ConfigLevel, Repository, WorktreeAddOptions};
use std::path::{Path, PathBuf};
use std::sync::Once;
use workon::empty_commit;

/// Point libgit2's system/global/XDG config search paths at an empty directory, once per
/// process, so fixture repos never layer in the developer's real `~/.gitconfig` (a global
/// `workon.*` key would otherwise poison every "unset config" assertion in the workspace).
/// Only affects in-process git2 use — spawned `git` binaries still read the real files.
fn isolate_ambient_git_config() {
    static ISOLATE: Once = Once::new();
    ISOLATE.call_once(|| {
        let empty = TempDir::new().expect("temp dir for git config isolation");
        for level in [ConfigLevel::System, ConfigLevel::XDG, ConfigLevel::Global] {
            // SAFETY: mutates libgit2's process-global search paths; guarded by `ISOLATE`
            // and called from `build()` before this fixture's repo (and, in practice, any
            // other test's git2 work in this process) touches config discovery.
            unsafe { git2::opts::set_search_path(level, empty.path()) }
                .expect("set git config search path");
        }
        // Keep the (empty) directory alive for the whole process — the search paths hold it.
        std::mem::forget(empty);
    });
}

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

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
            || !self.untracked_files.is_empty();
        if has_index_state && self.bare && self.worktrees.is_empty() {
            return Err(
                "staged_file/unstaged_file/untracked_file require a working tree: fixture is \
                 bare(true) with no worktree"
                    .into(),
            );
        }

        // `unstaged_file` baseline commits land BEFORE Graphite-metadata live-tip resolution
        // below: they move the cwd branch's tip, and any metadata entry recording that tip
        // must reflect the moved one, not the pre-baseline commit.
        if !self.unstaged_files.is_empty() {
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
