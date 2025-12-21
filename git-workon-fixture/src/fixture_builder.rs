use crate::fixture::Fixture;
use assert_fs::TempDir;
use git2::{BranchType, Repository, WorktreeAddOptions};
use miette::{IntoDiagnostic, Result};
use std::path::PathBuf;
use workon::empty_commit;

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
    remotes: Vec<(String, RemoteSource)>,
    upstreams: Vec<(String, String)>, // (local_branch, remote_branch)
}

impl<'fixture> FixtureBuilder<'fixture> {
    pub fn new() -> Self {
        Self {
            bare: false,
            default_branch: "main",
            worktrees: Vec::new(),
            remotes: Vec::new(),
            upstreams: Vec::new(),
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

    pub fn build(self) -> Result<Fixture> {
        let tmpdir = TempDir::new().into_diagnostic()?;
        let path = tmpdir.path().join(if self.bare {
            ".bare"
        } else {
            self.default_branch
        });
        let repo = if self.bare {
            Repository::init_bare(&path).into_diagnostic()?
        } else {
            Repository::init(&path).into_diagnostic()?
        };

        let mut config = repo.config().into_diagnostic()?;

        config
            .set_str("user.name", "git-workon-fixture")
            .into_diagnostic()?;

        config
            .set_str("user.email", "git-workon-fixture@fake.com")
            .into_diagnostic()?;

        empty_commit(&repo)?;

        if repo
            .find_branch(self.default_branch, BranchType::Local)
            .is_err()
        {
            let head = repo.head().into_diagnostic()?;
            let head_ref = head.shorthand().unwrap_or("");
            if head_ref != self.default_branch {
                if let Ok(mut branch) = repo.find_branch(head_ref, BranchType::Local) {
                    branch.rename(self.default_branch, true).into_diagnostic()?;
                }
                if !self.bare {
                    repo.set_head(&format!("refs/heads/{}", self.default_branch))
                        .into_diagnostic()?;
                }
            }
        }

        // Create worktrees
        for worktree in &self.worktrees {
            if *worktree == self.default_branch && !self.bare {
                return Err(miette::miette!(
                        "Cannot create a worktree with the same name as the default branch ({}) in a non-bare repository",
                        self.default_branch
                    ));
            }

            let worktree_path = tmpdir.path().join(worktree);
            let mut worktree_opts = WorktreeAddOptions::new();
            worktree_opts.checkout_existing(self.bare);

            repo.worktree(worktree, &worktree_path, Some(&worktree_opts))
                .into_diagnostic()?;
        }

        // Apply remotes
        for (name, source) in &self.remotes {
            repo.remote(name, &source.as_url()).into_diagnostic()?;
        }

        // Apply upstreams
        for (branch, remote_branch) in &self.upstreams {
            // Get the current commit of the local branch
            let local_branch = repo
                .find_branch(branch, BranchType::Local)
                .into_diagnostic()?;
            let commit_oid = local_branch
                .get()
                .target()
                .ok_or_else(|| miette::miette!("Branch {} has no target", branch))?;

            // Create the remote tracking ref at the same commit
            let remote_ref = if remote_branch.starts_with("refs/") {
                remote_branch.clone()
            } else {
                format!("refs/remotes/{}", remote_branch)
            };

            repo.reference(&remote_ref, commit_oid, false, "create remote ref")
                .into_diagnostic()?;

            // Set upstream tracking
            let mut local_branch = repo
                .find_branch(branch, BranchType::Local)
                .into_diagnostic()?;
            local_branch
                .set_upstream(Some(remote_branch))
                .into_diagnostic()?;
        }

        if self.worktrees.is_empty() {
            // No worktrees specified - return the main repo
            Ok(Fixture::new(repo, path, tmpdir))
        } else {
            let worktree_path = tmpdir.path().join(self.worktrees.last().unwrap());
            Ok(Fixture::new(repo, worktree_path, tmpdir))
        }
    }
}

impl<'fixture> Default for FixtureBuilder<'fixture> {
    fn default() -> Self {
        Self::new()
    }
}
