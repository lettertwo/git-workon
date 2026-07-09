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

    /// Write an executable `<name>` stub with a custom script `body`, for faking an
    /// arbitrary external binary (e.g. `gh`) rather than the `git-workon-<name>`
    /// dispatch convention `command` targets. Every invocation is logged (its
    /// space-joined arguments, one per line) before `body` runs, so tests can assert
    /// whether — and how many times — the stub was called via [`Self::invocations`].
    pub fn binary(self, name: &str, body: &str) -> Result<Self> {
        let path = self.dir.path().join(name);
        let log = self.invocation_log(name);
        // Quote the log path: an unquoted path containing a space (e.g. a TMPDIR
        // under a space-containing directory) would split the redirect target and
        // break every stub invocation.
        let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\n{}\n", log.display(), body);
        std::fs::write(&path, script)?;
        set_executable(&path)?;
        Ok(self)
    }

    /// Recorded invocation lines for a `binary` stub named `name` (empty if it was
    /// never called, or was created with `command` instead of `binary`).
    pub fn invocations(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.invocation_log(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn invocation_log(&self, name: &str) -> PathBuf {
        self.dir.path().join(format!("{name}.invocations.log"))
    }

    /// Symlink a real executable (e.g. another workspace binary's `CARGO_BIN_EXE_*` path) into
    /// the stub directory as `git-workon-<name>`, so a test can drive genuine external-binary
    /// behavior (not just canned `arg:`/`cwd:` stub output) through the same PATH-dispatch or
    /// PATH-completion surface `command` exercises.
    pub fn command_exe(self, name: &str, exe: &std::path::Path) -> Result<Self> {
        let path = self.dir.path().join(format!("git-workon-{name}"));
        symlink_exe(exe, &path)?;
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

    /// Builds a `PATH` value that excludes any directory containing a `name` binary,
    /// for testing degradation when an external tool (e.g. `gh`) is absent. Doesn't
    /// need a `PathStub` instance — it strips from the current process's `PATH`
    /// rather than adding a stub directory to it.
    pub fn path_without(name: &str) -> String {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|dir| !std::path::Path::new(dir).join(name).exists())
            .collect::<Vec<_>>()
            .join(":")
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

#[cfg(unix)]
fn symlink_exe(exe: &std::path::Path, link: &std::path::Path) -> Result<()> {
    std::os::unix::fs::symlink(exe, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn symlink_exe(exe: &std::path::Path, link: &std::path::Path) -> Result<()> {
    std::fs::copy(exe, link)?;
    Ok(())
}
