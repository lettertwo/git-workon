use std::path::{Path, PathBuf};

use assert_fs::fixture::ChildPath;
use assert_fs::TempDir;
use git2::{Oid, Repository};
use predicates::Predicate;

use crate::assert::{FixtureAssert, IntoFixturePredicate};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Fixture {
    repo: Option<Repository>,
    cwd: Option<PathBuf>,
    tempdir: Option<TempDir>,
}

impl Fixture {
    pub fn new(repo: Repository, cwd: PathBuf, tempdir: TempDir) -> Self {
        Self {
            repo: Some(repo),
            tempdir: Some(tempdir),
            cwd: Some(cwd),
        }
    }

    pub fn destroy(&mut self) -> Result<()> {
        if let Some(tempdir) = self.tempdir.take() {
            tempdir.close()?
        }
        self.cwd = None;
        self.tempdir = None;
        self.repo = None;
        Ok(())
    }

    /// Add a remote to the repository
    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        let repo = self.repo.as_ref().ok_or("No repository")?;
        repo.remote(name, url)?;
        Ok(())
    }

    /// Create a remote tracking reference
    pub fn create_remote_ref(&self, ref_name: &str, commit_oid: Oid) -> Result<()> {
        let repo = self.repo.as_ref().ok_or("No repository")?;

        // Normalize ref name - add refs/remotes/ prefix if not present
        let full_ref = if ref_name.starts_with("refs/") {
            ref_name.to_string()
        } else {
            format!("refs/remotes/{}", ref_name)
        };

        repo.reference(&full_ref, commit_oid, false, "create remote ref")?;
        Ok(())
    }

    /// Set upstream tracking for a branch
    pub fn set_upstream(&self, branch: &str, remote_branch: &str) -> Result<()> {
        let repo = self.repo()?;

        let mut local_branch = repo.find_branch(branch, git2::BranchType::Local)?;

        local_branch.set_upstream(Some(remote_branch))?;
        Ok(())
    }

    /// Update a branch to point to a specific commit
    pub fn update_branch(&self, branch: &str, commit_oid: Oid) -> Result<()> {
        let repo = self.repo()?;

        let mut local_branch = repo.find_branch(branch, git2::BranchType::Local)?;

        local_branch
            .get_mut()
            .set_target(commit_oid, &format!("Update {} to {}", branch, commit_oid))?;
        Ok(())
    }

    pub fn cwd(&self) -> Result<ChildPath> {
        let path = self.cwd.as_ref().ok_or("No fixture path")?;
        Ok(ChildPath::new(path))
    }

    pub fn root(&self) -> Result<ChildPath> {
        let tempdir = self.tempdir.as_ref().ok_or("No fixture root")?;
        Ok(ChildPath::new(tempdir.path()))
    }

    pub fn repo(&self) -> Result<&Repository> {
        self.repo.as_ref().ok_or_else(|| "No repository".into())
    }

    pub fn head(&self) -> Result<git2::Reference> {
        let repo = self.repo()?;
        Ok(repo.head()?)
    }

    /// Start building a commit in a worktree
    pub fn commit<'a>(&'a self, worktree_name: &'a str) -> CommitBuilder<'a> {
        CommitBuilder::new(self, worktree_name)
    }
}

impl AsRef<Path> for Fixture {
    fn as_ref(&self) -> &Path {
        self.cwd.as_ref().unwrap()
    }
}

impl AsRef<Repository> for Fixture {
    fn as_ref(&self) -> &Repository {
        self.repo.as_ref().unwrap()
    }
}

impl<'a> From<&'a Fixture> for &'a Path {
    fn from(val: &'a Fixture) -> Self {
        val.cwd.as_ref().unwrap()
    }
}

impl<'a> From<&'a Fixture> for &'a Repository {
    fn from(val: &'a Fixture) -> Self {
        val.repo.as_ref().unwrap()
    }
}

impl FixtureAssert for Fixture {
    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, Repository>,
        P: Predicate<Repository> + 'static,
    {
        self.repo
            .as_ref()
            .expect("No repository for fixture assertion")
            .assert(predicate);
        self
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
        let repo_path = self.fixture.cwd.as_ref().ok_or("No fixture path")?;

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
            return Err(format!("Worktree {} does not exist", self.worktree_name).into());
        }

        let worktree_repo = Repository::open(&worktree_path)?;

        // Write all files
        for (path, content) in &self.files {
            let file_path = worktree_path.join(path);

            // Create parent directories if needed
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            std::fs::write(&file_path, content)?;
        }

        // Stage all files
        let mut index = worktree_repo.index()?;
        for (path, _) in &self.files {
            index.add_path(Path::new(path))?;
        }
        index.write()?;

        // Create commit
        let tree_id = index.write_tree()?;
        let tree = worktree_repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Test User", "test@example.com")?;

        let parent_commit = worktree_repo.head()?.peel_to_commit()?;

        let commit_oid =
            worktree_repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent_commit])?;

        Ok(commit_oid)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.destroy().unwrap();
    }
}
