use miette::Diagnostic;
use thiserror::Error;

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
