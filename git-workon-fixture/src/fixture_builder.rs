use crate::fixture::Fixture;
use assert_fs::TempDir;
use git2::{BranchType, Repository, WorktreeAddOptions};
use std::path::PathBuf;
use workon::empty_commit;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

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
    branch_metadata: Vec<(String, String)>, // (branch, parent) for refs/branch-metadata/*
    raw_branch_metadata: Vec<(String, Vec<u8>)>, // (branch, raw_bytes) for malformed-blob tests
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
            branch_metadata: Vec::new(),
            raw_branch_metadata: Vec::new(),
        }
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

    /// Write Graphite branch-metadata for `branch` with parent `parent`.
    ///
    /// Creates a blob at `refs/branch-metadata/<branch>` containing
    /// `{"branchName": "<branch>", "parentBranchName": "<parent>"}`, mirroring
    /// what `gt track` writes.
    pub fn branch_metadata(mut self, branch: &str, parent: &str) -> Self {
        self.branch_metadata
            .push((branch.to_string(), parent.to_string()));
        self
    }

    /// Write Graphite branch-metadata for a **deleted** branch (ghost entry).
    ///
    /// Same as [`branch_metadata`] but intentionally does NOT create a local git branch ref.
    /// Use this to simulate a branch that was merged/deleted after being tracked by Graphite —
    /// metadata lingers in `refs/branch-metadata/<branch>` but the branch ref is gone.
    /// This is the precondition for the `DeletedNode` resolution and ghost-branch filtering tests.
    ///
    /// [`branch_metadata`]: Self::branch_metadata
    pub fn ghost_branch_metadata(mut self, branch: &str, parent: &str) -> Self {
        // Store the same way as branch_metadata but mark it by NOT creating a git branch.
        // We reuse `raw_branch_metadata` storage with valid JSON so the parser succeeds
        // while bypassing the auto-branch-creation in the build step.
        let content = serde_json::json!({
            "branchName": branch,
            "parentBranchName": parent,
        })
        .to_string();
        self.raw_branch_metadata
            .push((branch.to_string(), content.into_bytes()));
        self
    }

    /// Write a raw blob at `refs/branch-metadata/<branch>`.
    ///
    /// Use this to simulate malformed metadata (e.g. non-JSON content) for error-path tests.
    pub fn raw_branch_metadata(mut self, branch: &str, bytes: Vec<u8>) -> Self {
        self.raw_branch_metadata.push((branch.to_string(), bytes));
        self
    }

    pub fn build(self) -> Result<Fixture> {
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

        // Write branch-metadata blobs for Graphite stack tracking.
        // Also create a local branch ref when one does not already exist: in real Graphite
        // repos every tracked branch has a corresponding git ref. Creating it here lets
        // branch_exists() correctly identify live branches vs ghost entries in tests.
        for (branch, parent) in &self.branch_metadata {
            let content = serde_json::json!({
                "branchName": branch,
                "parentBranchName": parent,
            })
            .to_string();
            let oid = repo.blob(content.as_bytes())?;
            repo.reference(
                &format!("refs/branch-metadata/{branch}"),
                oid,
                false,
                "add branch metadata",
            )?;
            // Trunk and worktree branches already have refs; metadata-only stack
            // diffs do not — create a local branch pointing to HEAD for them.
            if repo.find_branch(branch, BranchType::Local).is_err() {
                let head = repo.head()?;
                let commit = head.peel_to_commit()?;
                repo.branch(branch, &commit, false)?;
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

        if self.worktrees.is_empty() {
            // No worktrees specified - return the main repo
            Ok(Fixture::new(repo, path, tmpdir))
        } else {
            // Open the repository from the worktree path instead of using the bare/main repo
            let worktree_path = tmpdir.path().join(self.worktrees.last().unwrap());
            let worktree_repo = Repository::open(&worktree_path)?;
            Ok(Fixture::new(worktree_repo, worktree_path, tmpdir))
        }
    }
}

impl<'fixture> Default for FixtureBuilder<'fixture> {
    fn default() -> Self {
        Self::new()
    }
}
