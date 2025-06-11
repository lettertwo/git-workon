mod branch;
mod reference;
mod repository;

use assert_fs::TempDir;
use miette::{IntoDiagnostic, Result};

pub use self::branch::*;
pub use self::reference::*;
pub use self::repository::*;

pub struct Fixture {
    pub repo: Option<Repository>,
    pub path: Option<TempDir>,
}

impl Fixture {
    pub fn new(repo: Repository, path: TempDir) -> Self {
        Self {
            repo: Some(repo),
            path: Some(path),
        }
    }

    pub fn with<F>(&self, f: F)
    where
        F: FnOnce(&Repository, &TempDir),
    {
        let repo = self.repo.as_ref().unwrap();
        let path = self.path.as_ref().unwrap();
        f(repo, path);
    }

    pub fn destroy(&mut self) -> Result<()> {
        if let Some(path) = self.path.take() {
            path.close().into_diagnostic()?
        }
        self.path = None;
        self.repo = None;
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.destroy().unwrap();
    }
}
