//! Applying a [`PatchText`] to a repository's index or working tree — the one chokepoint
//! (per the diff-model-and-patch-synthesis design decision) parameterizable over two backends:
//! [`Git2Applier`] (libgit2's `Repository::apply`) and [`CliApplier`] (`git apply` on stdin).
//! The round-trip verdict corpus runs every scenario against both; `CliApplier` is the oracle.
//!
//! ## The flag matrix (the direction-dependent-drop-rules chokepoint, prototype-verified)
//!
//! `git apply` takes ONLY `--cached`/`--reverse`, patch on stdin — never `--unidiff-zero`,
//! never `--3way`. [`StageVerb::plan`] encodes the same matrix for both backends:
//!
//! | verb    | patch base | destination | direction |
//! |---------|------------|-------------|-----------|
//! | Stage   | Old        | Index       | Forward   |
//! | Unstage | New        | Index       | Reverse   |
//! | Discard | New        | Workdir     | Reverse   |
//!
//! ## `ApplyLocation::Index` preimage (plan risk #3)
//!
//! The index is not HEAD. A Stage patch must be synthesized from the unstaged model
//! (`index_to_workdir` — old side is the INDEX); an Unstage patch must be synthesized from the
//! staged model (`tree_to_index` — old side is HEAD). Feeding the wrong model's patch to
//! `ApplyLocation::Index` is the classic corruption source. Never `ApplyLocation::Both`.

use std::io::Write;
use std::process::{Command, Stdio};

use git2::Repository;

use crate::error::ApplyError;
use crate::synthesis::PatchText;

/// Where a patch is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDestination {
    Index,
    Workdir,
}

/// Whether the patch is applied as synthesized, or inverted first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDirection {
    Forward,
    Reverse,
}

/// The three staging actions a review session performs. [`StageVerb::plan`] is the flag
/// matrix above, encoded once so `ops.rs` (the whole-file ops and routing layer) and the
/// applier tests share one source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageVerb {
    Stage,
    Unstage,
    Discard,
}

impl StageVerb {
    /// `Stage`->(Old, Index, Forward); `Unstage`->(New, Index, Reverse);
    /// `Discard`->(New, Workdir, Reverse).
    pub fn plan(
        self,
    ) -> (
        crate::synthesis::PatchBase,
        ApplyDestination,
        ApplyDirection,
    ) {
        use crate::synthesis::PatchBase;
        match self {
            StageVerb::Stage => (
                PatchBase::Old,
                ApplyDestination::Index,
                ApplyDirection::Forward,
            ),
            StageVerb::Unstage => (
                PatchBase::New,
                ApplyDestination::Index,
                ApplyDirection::Reverse,
            ),
            StageVerb::Discard => (
                PatchBase::New,
                ApplyDestination::Workdir,
                ApplyDirection::Reverse,
            ),
        }
    }
}

/// A patch-application backend. [`Git2Applier`] and [`CliApplier`] both implement this over
/// the same [`PatchText`] — the round-trip corpus drives whichever `dyn Applier` it's handed.
pub trait Applier {
    fn apply(
        &self,
        repo: &Repository,
        patch: &PatchText,
        dest: ApplyDestination,
        dir: ApplyDirection,
    ) -> Result<(), ApplyError>;
}

/// Applies via libgit2's `Repository::apply`. `Reverse` is [`PatchText::invert`] followed by a
/// forward apply — `Repository::apply` itself has no reverse flag (plan risk #1).
pub struct Git2Applier;

impl Applier for Git2Applier {
    fn apply(
        &self,
        repo: &Repository,
        patch: &PatchText,
        dest: ApplyDestination,
        dir: ApplyDirection,
    ) -> Result<(), ApplyError> {
        let bytes = match dir {
            ApplyDirection::Forward => patch.to_bytes(),
            ApplyDirection::Reverse => patch.invert().to_bytes(),
        };
        let diff = git2::Diff::from_buffer(&bytes)?;
        let location = match dest {
            ApplyDestination::Index => git2::ApplyLocation::Index,
            ApplyDestination::Workdir => git2::ApplyLocation::WorkDir,
        };
        repo.apply(&diff, location, None)?;
        Ok(())
    }
}

/// Applies by spawning `git apply` with the patch on stdin, cwd set to the repository's
/// working directory. `Index` destination -> `--cached`; `Reverse` direction -> `--reverse`.
/// Never `--unidiff-zero`, never `--3way` (prototype chokepoint, direction-dependent drop rules).
pub struct CliApplier;

impl Applier for CliApplier {
    fn apply(
        &self,
        repo: &Repository,
        patch: &PatchText,
        dest: ApplyDestination,
        dir: ApplyDirection,
    ) -> Result<(), ApplyError> {
        let workdir = repo
            .workdir()
            .expect("CliApplier requires a repository with a working directory");

        let mut args = vec!["apply".to_string()];
        if dest == ApplyDestination::Index {
            args.push("--cached".to_string());
        }
        if dir == ApplyDirection::Reverse {
            args.push("--reverse".to_string());
        }

        let mut child = Command::new("git")
            .args(&args)
            .current_dir(workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ApplyError::GitSpawn)?;

        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(&patch.to_bytes())
            .map_err(ApplyError::GitSpawn)?;

        let output = child.wait_with_output().map_err(ApplyError::GitSpawn)?;
        if !output.status.success() {
            return Err(ApplyError::CliApplyFailed {
                args,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(())
    }
}

/// Classify an [`ApplyError`] as index-lock contention (plan risk #8), spanning both backends:
/// git2's `ErrorCode::Locked`, or its `Index`/`Os` error classes with "lock" in the message;
/// the CLI backend via `"index.lock"` in `git apply`'s stderr.
pub fn is_lock_contention(err: &ApplyError) -> bool {
    match err {
        ApplyError::Git(e) => {
            e.code() == git2::ErrorCode::Locked
                || ((e.class() == git2::ErrorClass::Index || e.class() == git2::ErrorClass::Os)
                    && e.message().to_lowercase().contains("lock"))
        }
        ApplyError::IndexLocked { .. } => true,
        ApplyError::CliApplyFailed { stderr, .. } => stderr.contains("index.lock"),
        ApplyError::GitSpawn(_) | ApplyError::Io { .. } => false,
    }
}
