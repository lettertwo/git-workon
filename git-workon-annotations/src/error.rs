use miette::Diagnostic;
use thiserror::Error;

/// Result type alias using [`AnnotationsError`].
pub type Result<T> = std::result::Result<T, AnnotationsError>;

/// Errors from the annotation store: opening the database, migrating its schema, and
/// running the CRUD/resolver operations on top of it. Follows the two-layer pattern
/// (ADR-008): a concrete enum, no `.into_diagnostic()` calls in this crate.
#[derive(Error, Diagnostic, Debug)]
pub enum AnnotationsError {
    /// Creating the database's parent directory failed.
    #[error("failed to create annotations database directory at '{path}'")]
    #[diagnostic(code(workon::annotations::db_dir_failed))]
    DbDirFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Opening the sqlite connection failed.
    #[error("failed to open annotations database at '{path}'")]
    #[diagnostic(code(workon::annotations::open_failed))]
    OpenFailed {
        path: String,
        #[source]
        source: rusqlite::Error,
    },

    /// Applying the schema (or a migration step) failed.
    #[error("failed to migrate annotations database schema")]
    #[diagnostic(code(workon::annotations::migration_failed))]
    MigrationFailed(#[source] rusqlite::Error),

    /// A CRUD or query statement against the store failed.
    #[error("annotation store query failed")]
    #[diagnostic(code(workon::annotations::query_failed))]
    QueryFailed(#[source] rusqlite::Error),

    /// A write transaction failed to commit (or begin/rollback).
    #[error("annotation store write transaction failed")]
    #[diagnostic(code(workon::annotations::transaction_failed))]
    TransactionFailed(#[source] rusqlite::Error),

    /// A `uid` (annotation, or parent for a reply) named by the caller doesn't exist.
    #[error("no annotation with uid '{uid}'")]
    #[diagnostic(code(workon::annotations::not_found))]
    NotFound { uid: String },
}
