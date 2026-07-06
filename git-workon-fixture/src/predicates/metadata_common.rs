//! Shared lookups behind the Graphite branch-metadata predicates.
//!
//! Both on-disk formats (`refs/branch-metadata/<branch>` blobs and
//! `.graphite_metadata.db`) get read by more than one predicate; these helpers keep the
//! git2/rusqlite plumbing in one place instead of duplicated per predicate file.

use git2::Repository;

/// Read and parse the `refs/branch-metadata/<branch>` JSON blob, if it exists and parses.
pub(crate) fn refs_metadata_json(repo: &Repository, branch: &str) -> Option<serde_json::Value> {
    let refname = format!("refs/branch-metadata/{branch}");
    let reference = repo.find_reference(&refname).ok()?;
    let object = reference.peel(git2::ObjectType::Blob).ok()?;
    let blob = object.into_blob().ok()?;
    serde_json::from_slice::<serde_json::Value>(blob.content()).ok()
}

/// Read one `column` from the `branch_metadata` row for `branch` in
/// `.graphite_metadata.db`, opened read-only off `repo.commondir()`.
pub(crate) fn sqlite_metadata_field(
    repo: &Repository,
    branch: &str,
    column: &str,
) -> Option<String> {
    let db_path = repo.commondir().join(".graphite_metadata.db");
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row(
        &format!("SELECT {column} FROM branch_metadata WHERE branch_name = ?1"),
        [branch],
        |row| row.get(0),
    )
    .ok()
}
