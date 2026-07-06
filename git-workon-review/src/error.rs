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

    /// The file's status can't be expressed as a hunk patch (trap 3: whole-file ops route
    /// around synthesis entirely; this is what a caller sees if it reaches synthesis anyway).
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
