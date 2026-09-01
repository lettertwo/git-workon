use miette::Diagnostic;
use thiserror::Error;

use crate::model::FileStatus;

/// Result type alias using ReviewError
pub type Result<T> = std::result::Result<T, ReviewError>;

/// Main error type for the review library
#[derive(Error, Diagnostic, Debug)]
pub enum ReviewError {
    /// Git operation failed
    #[error(transparent)]
    #[diagnostic(code(workon::review::git_error))]
    Git(#[from] git2::Error),

    /// Diff construction or acquisition failed
    #[error(transparent)]
    #[diagnostic(transparent)]
    Diff(#[from] DiffError),

    /// Patch synthesis from the diff model failed
    #[error(transparent)]
    #[diagnostic(transparent)]
    Synthesis(#[from] SynthesisError),

    /// Applying a synthesized patch failed
    #[error(transparent)]
    #[diagnostic(transparent)]
    Apply(#[from] ApplyError),

    /// A `git workon review <source>` argument failed to resolve to changesets
    #[error(transparent)]
    #[diagnostic(transparent)]
    Source(#[from] SourceError),
}

/// Errors building a [`crate::model::DiffModel`] from git2 structures, or acquiring one for a
/// `workon::Changeset` (`git-workon-lib`).
#[derive(Error, Diagnostic, Debug)]
pub enum DiffError {
    /// A git2 call failed while building or reading a diff/patch
    #[error(transparent)]
    #[diagnostic(code(workon::review::diff_git_error))]
    Git(#[from] git2::Error),

    /// Diffing a changeset's resolved rev pair failed — a bad/garbage `Oid` never yields an
    /// empty [`crate::model::DiffModel`], it yields this error.
    #[error("failed to diff changeset '{name}'")]
    #[diagnostic(code(workon::review::changeset_diff_failed))]
    ChangesetDiffFailed {
        name: String,
        #[source]
        source: git2::Error,
    },

    /// [`workon::assemble_changesets`] failed to walk the stack (a broken Graphite metadata
    /// snapshot, an unresolvable branch, etc.) — surfaced distinctly from
    /// [`Self::ChangesetDiffFailed`], which is a resolved-but-undiffable rev pair.
    #[error("failed to assemble the changeset stack")]
    #[diagnostic(code(workon::review::stack_assembly_failed))]
    StackAssembly(#[from] workon::WorkonError),
}

/// Errors synthesizing a [`crate::synthesis::PatchText`] from a [`crate::model::FileChange`].
#[derive(Error, Diagnostic, Debug)]
pub enum SynthesisError {
    /// No lines were kept for the patch — nothing to apply.
    #[error("no lines selected to synthesize a patch for '{path}' hunk {hunk}")]
    #[diagnostic(code(workon::review::empty_selection))]
    EmptySelection { path: String, hunk: usize },

    /// `hunk_idx` didn't name a hunk on the file.
    #[error("hunk index {index} out of range for '{path}'")]
    #[diagnostic(code(workon::review::hunk_out_of_range))]
    HunkOutOfRange { path: String, index: usize },

    /// The file's status can't be expressed as a hunk patch (whole-file-ops fallback: whole-file
    /// ops route around synthesis entirely; this is what a caller sees if it reaches synthesis
    /// anyway).
    #[error("line-precise selection is not supported for '{path}' ({status:?})")]
    #[diagnostic(
        code(workon::review::line_selection_unsupported),
        help("stage/unstage/discard the whole file instead")
    )]
    LineSelectionUnsupported { path: String, status: FileStatus },

    /// The file is binary — there are no hunks to synthesize a patch from.
    #[error("'{path}' is a binary file and cannot be patched by hunk")]
    #[diagnostic(code(workon::review::binary_file))]
    BinaryFile { path: String },
}

/// Errors applying a [`crate::synthesis::PatchText`] via a [`crate::apply::Applier`].
#[derive(Error, Diagnostic, Debug)]
pub enum ApplyError {
    /// A git2 call failed while applying a patch
    #[error(transparent)]
    #[diagnostic(code(workon::review::apply_git_error))]
    Git(#[from] git2::Error),

    /// The index stayed locked across every retry (see `queue.rs`'s retry-once policy).
    #[error("index locked after {attempts} attempt(s)")]
    #[diagnostic(code(workon::review::index_locked))]
    IndexLocked { attempts: u32 },

    /// `git apply` exited non-zero.
    #[error("git apply failed (args: {args:?}): {stderr}")]
    #[diagnostic(code(workon::review::cli_apply_failed))]
    CliApplyFailed { args: Vec<String>, stderr: String },

    /// Spawning or communicating with the `git` subprocess failed (not a nonzero exit — that's
    /// [`ApplyError::CliApplyFailed`]).
    #[error("failed to spawn or communicate with git")]
    #[diagnostic(code(workon::review::git_spawn_failed))]
    GitSpawn(#[source] std::io::Error),

    /// A whole-file operation's filesystem I/O failed (`file_ops.rs`, the whole-file ops and
    /// routing layer).
    #[error("file operation on '{path}' failed")]
    #[diagnostic(code(workon::review::file_op_io))]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Errors resolving a `git workon review <source>` positional argument to changesets
/// (ADR-036: the classifier/resolver seam is [`crate::source::Source`]).
#[derive(Error, Diagnostic, Debug)]
pub enum SourceError {
    /// The `stack` keyword found no Graphite metadata and `branch` has no upstream to infer a
    /// git-only stack from. An explicit ask deserves an explicit failure — never a silent
    /// fall-through to the uncommitted layer (ADR-036).
    #[error("branch '{branch}' has no Graphite stack and no upstream to infer one from")]
    #[diagnostic(
        code(workon::review::stack_no_upstream),
        help(
            "set an upstream (git branch --set-upstream-to=<remote>/{branch}), \
             or run 'git workon review uncommitted'"
        )
    )]
    NoUpstream { branch: String },

    /// Assembling the requested stack failed for a reason other than a missing upstream
    /// (broken Graphite metadata, an unresolvable branch, a bad recorded parent revision).
    #[error("failed to assemble the stack for '{branch}'")]
    #[diagnostic(code(workon::review::stack_resolution_failed))]
    StackResolutionFailed {
        branch: String,
        #[source]
        source: workon::WorkonError,
    },

    /// A `<ref>` argument (or one side of a `Range`) doesn't rev-parse to anything reviewable —
    /// a typo, a deleted branch, a garbage commit-ish.
    #[error("cannot resolve '{text}' as a review source")]
    #[diagnostic(
        code(workon::review::unresolvable_source),
        help(
            "try 'stack', 'uncommitted', a branch/tag/commit, a..b / a...b range, \
             or a PR reference (pr-123, #123)"
        )
    )]
    UnresolvableSource { text: String },

    /// An untracked (or remote-tracking) `<ref>` branch has neither an upstream nor a resolvable
    /// trunk to compute "what this branch adds" from.
    #[error("branch '{branch}' has no upstream and no trunk to compute a base from")]
    #[diagnostic(
        code(workon::review::no_base_for_branch),
        help(
            "set an upstream (git branch --set-upstream-to=<remote>/{branch}), \
             or ensure a trunk branch (main/master) exists"
        )
    )]
    NoBaseForBranch { branch: String },

    /// `check_gh_available` found no working `gh` CLI — a PR reference can't resolve without it,
    /// the same requirement `git workon #123`'s own PR workflow has.
    #[error("'{text}' is a PR reference, but gh is not available")]
    #[diagnostic(
        code(workon::review::gh_unavailable),
        help("install the gh CLI and run 'gh auth login', then retry")
    )]
    GhUnavailable {
        text: String,
        #[source]
        source: workon::WorkonError,
    },

    /// Resolving a PR reference failed after `gh` was confirmed available: an unknown PR number,
    /// `gh` not authenticated, a fork remote/fetch failure, or a missing base/head ref.
    #[error("failed to resolve PR reference '{text}'")]
    #[diagnostic(
        code(workon::review::pr_resolution_failed),
        help("check the PR number and that 'gh auth status' is logged in")
    )]
    PrResolutionFailed {
        text: String,
        #[source]
        source: workon::WorkonError,
    },
}
