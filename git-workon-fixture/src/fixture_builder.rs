use crate::fixture::{Fixture, Repository};
use assert_fs::TempDir;
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
        let path = TempDir::new().into_diagnostic()?;
        let repo = git2::Repository::init(&path).into_diagnostic()?;
        match self.bare {
            true => {
                // git config core.bare true
                let mut config = repo.config().into_diagnostic()?;
                config.set_bool("core.bare", true).into_diagnostic()?;
            }
            false => match self.worktree {
                Some(worktree) => {}
                None => {}
            },
        }
        empty_commit(&repo)?;
        Ok(Fixture::new(Repository::new(repo), path))
    }
}
