use std::path::{Path, PathBuf};

use assert_fs::fixture::ChildPath;
use assert_fs::TempDir;
use git2::{Oid, Repository};
use miette::{bail, IntoDiagnostic, Result};

pub struct Fixture {
    pub repo: Option<Repository>,
    pub path: Option<PathBuf>,
    tempdir: Option<TempDir>,
}

impl Fixture {
    pub fn new(repo: Repository, path: PathBuf, tempdir: TempDir) -> Self {
        Self {
            repo: Some(repo),
            tempdir: Some(tempdir),
            path: Some(path),
        }
    }

    pub fn with<F>(&self, f: F)
    where
        F: FnOnce(&Repository, &ChildPath),
    {
        let repo = self.repo.as_ref().unwrap();
        let path = self.path.as_ref().unwrap();
        let path_child = ChildPath::new(path);
        f(repo, &path_child);
    }

    pub fn destroy(&mut self) -> Result<()> {
        if let Some(tempdir) = self.tempdir.take() {
            tempdir.close().into_diagnostic()?
        }
        self.path = None;
        self.tempdir = None;
        self.repo = None;
        Ok(())
    }

    /// Add a remote to the repository
    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        let repo = self
            .repo
            .as_ref()
            .ok_or_else(|| miette::miette!("No repository"))?;
        repo.remote(name, url).into_diagnostic()?;
        Ok(())
    }

    /// Create a remote tracking reference
    pub fn create_remote_ref(&self, ref_name: &str, commit_oid: Oid) -> Result<()> {
        let repo = self
            .repo
            .as_ref()
            .ok_or_else(|| miette::miette!("No repository"))?;

        // Normalize ref name - add refs/remotes/ prefix if not present
        let full_ref = if ref_name.starts_with("refs/") {
            ref_name.to_string()
        } else {
            format!("refs/remotes/{}", ref_name)
        };

        repo.reference(&full_ref, commit_oid, false, "create remote ref")
            .into_diagnostic()?;
        Ok(())
    }

    /// Set upstream tracking for a branch
    pub fn set_upstream(&self, branch: &str, remote_branch: &str) -> Result<()> {
        let repo = self
            .repo
            .as_ref()
            .ok_or_else(|| miette::miette!("No repository"))?;

        let mut local_branch = repo
            .find_branch(branch, git2::BranchType::Local)
            .into_diagnostic()?;

        local_branch
            .set_upstream(Some(remote_branch))
            .into_diagnostic()?;
        Ok(())
    }

    /// Update a branch to point to a specific commit
    pub fn update_branch(&self, branch: &str, commit_oid: Oid) -> Result<()> {
        let repo = self
            .repo
            .as_ref()
            .ok_or_else(|| miette::miette!("No repository"))?;

        let mut local_branch = repo
            .find_branch(branch, git2::BranchType::Local)
            .into_diagnostic()?;

        local_branch
            .get_mut()
            .set_target(commit_oid, &format!("Update {} to {}", branch, commit_oid))
            .into_diagnostic()?;
        Ok(())
    }

    /// Start building a commit in a worktree
    pub fn commit<'a>(&'a self, worktree_name: &'a str) -> CommitBuilder<'a> {
        CommitBuilder::new(self, worktree_name)
    }
}

/// Builder for creating commits with multiple files
pub struct CommitBuilder<'a> {
    fixture: &'a Fixture,
    worktree_name: &'a str,
    files: Vec<(String, String)>, // (path, content)
}

impl<'a> CommitBuilder<'a> {
    fn new(fixture: &'a Fixture, worktree_name: &'a str) -> Self {
        Self {
            fixture,
            worktree_name,
            files: Vec::new(),
        }
    }

    /// Add a file to be committed
    pub fn file(mut self, path: &str, content: &str) -> Self {
        self.files.push((path.to_string(), content.to_string()));
        self
    }

    /// Create the commit with the specified message
    pub fn create(self, message: &str) -> Result<Oid> {
        let repo_path = self
            .fixture
            .path
            .as_ref()
            .ok_or_else(|| miette::miette!("No fixture path"))?;

        // Find the worktree path
        let worktree_path =
            if self.worktree_name == repo_path.file_name().unwrap().to_str().unwrap() {
                // This is the main worktree
                repo_path.clone()
            } else {
                // This is a linked worktree - look in parent directory
                repo_path.parent().unwrap().join(self.worktree_name)
            };

        if !worktree_path.exists() {
            bail!("Worktree {} does not exist", self.worktree_name);
        }

        let worktree_repo = Repository::open(&worktree_path).into_diagnostic()?;

        // Write all files
        for (path, content) in &self.files {
            let file_path = worktree_path.join(path);

            // Create parent directories if needed
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).into_diagnostic()?;
            }

            std::fs::write(&file_path, content).into_diagnostic()?;
        }

        // Stage all files
        let mut index = worktree_repo.index().into_diagnostic()?;
        for (path, _) in &self.files {
            index.add_path(Path::new(path)).into_diagnostic()?;
        }
        index.write().into_diagnostic()?;

        // Create commit
        let tree_id = index.write_tree().into_diagnostic()?;
        let tree = worktree_repo.find_tree(tree_id).into_diagnostic()?;
        let sig = git2::Signature::now("Test User", "test@example.com").into_diagnostic()?;

        let parent_commit = worktree_repo
            .head()
            .into_diagnostic()?
            .peel_to_commit()
            .into_diagnostic()?;

        let commit_oid = worktree_repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent_commit])
            .into_diagnostic()?;

        Ok(commit_oid)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.destroy().unwrap();
    }
}
