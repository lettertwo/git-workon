use std::path::PathBuf;

use assert_fs::fixture::ChildPath;
use assert_fs::TempDir;
use git2::Repository;
use miette::{IntoDiagnostic, Result};

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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.destroy().unwrap();
    }
}
