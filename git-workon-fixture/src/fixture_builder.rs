use crate::fixture::Fixture;
use assert_fs::TempDir;
use git2::{BranchType, Repository, WorktreeAddOptions};
use miette::{IntoDiagnostic, Result};
use workon::empty_commit;

pub struct FixtureBuilder<'fixture> {
    bare: bool,
    default_branch: &'fixture str,
    worktree: Option<&'fixture str>,
}

impl<'fixture> FixtureBuilder<'fixture> {
    pub fn new() -> Self {
        Self {
            bare: false,
            default_branch: "main",
            worktree: None,
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

    pub fn worktree(mut self, worktree: &'fixture str) -> Self {
        self.worktree = Some(worktree);
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

        match self.worktree {
            Some(worktree) if worktree != self.default_branch || self.bare => {
                let worktree_path = tmpdir.path().join(worktree);

                let mut worktree_opts = WorktreeAddOptions::new();
                worktree_opts.checkout_existing(self.bare);

                repo.worktree(worktree, &worktree_path, Some(&worktree_opts))
                    .into_diagnostic()?;

                let worktree_repo = Repository::open(&worktree_path).into_diagnostic()?;

                Ok(Fixture::new(worktree_repo, worktree_path, tmpdir))
            }
            Some(worktree) if worktree == self.default_branch && !self.bare => {
                Err(miette::miette!("Cannot create a worktree with the same name as the default branch ({}) in a non-bare repository", self.default_branch))
            }
            Some(_) | None => Ok(Fixture::new(repo, path, tmpdir)),
        }
    }
}

impl<'fixture> Default for FixtureBuilder<'fixture> {
    fn default() -> Self {
        Self::new()
    }
}
