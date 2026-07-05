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
}
