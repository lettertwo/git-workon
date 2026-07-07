//! PATH-stub helper for testing external-subcommand dispatch
//! (`git workon <name>` → `git-workon-<name>`, see `git-workon/src/dispatch.rs`).
//!
//! Writes an executable shell-script stub named `git-workon-<name>` into a temp directory and
//! yields a `PATH` value with that directory prepended, so tests can assert dispatch and
//! argument/cwd passthrough without a real external binary.

use assert_fs::TempDir;
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A temp directory of `git-workon-<name>` stub executables for dispatch tests.
///
/// Each stub is a `/bin/sh` script that prints one `arg:<word>` line per argv element it
/// receives (excluding argv[0]) and a trailing `cwd:<path>` line, then exits 0 — deterministic
/// output tests can assert against for passthrough (args, flags, working directory).
pub struct PathStub {
    dir: TempDir,
}

impl PathStub {
    pub fn new() -> Result<Self> {
        Ok(Self {
            dir: TempDir::new()?,
        })
    }

    /// Write an executable `git-workon-<name>` stub into the stub directory.
    pub fn command(self, name: &str) -> Result<Self> {
        let path = self.dir.path().join(format!("git-workon-{name}"));
        let script = "#!/bin/sh\nfor a in \"$@\"; do echo \"arg:$a\"; done\necho \"cwd:$(pwd)\"\n";
        std::fs::write(&path, script)?;
        set_executable(&path)?;
        Ok(self)
    }

    /// `PATH` value with the stub directory prepended to the current process's `PATH`, so a
    /// stub shadows nothing else already on `PATH` unless intended (see built-in precedence).
    pub fn path(&self) -> String {
        let existing = std::env::var("PATH").unwrap_or_default();
        format!("{}:{}", self.dir.path().display(), existing)
    }

    /// The stub directory's path, for tests that need it directly.
    pub fn dir(&self) -> &std::path::Path {
        self.dir.path()
    }
}

#[cfg(unix)]
fn set_executable(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &PathBuf) -> Result<()> {
    Ok(())
}
