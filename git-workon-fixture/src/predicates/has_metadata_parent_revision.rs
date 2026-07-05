use crate::fixture_builder::MetadataFormat;
use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasMetadataParentRevisionPredicate {
    format: MetadataFormat,
    branch: String,
    expected_revision: String,
}

impl PredicateReflection for HasMetadataParentRevisionPredicate {}

impl fmt::Display for HasMetadataParentRevisionPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} branch-metadata for '{}' has parent revision '{}'",
            self.format, self.branch, self.expected_revision
        )
    }
}

impl Predicate<Repository> for HasMetadataParentRevisionPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        match self.format {
            MetadataFormat::Sqlite => {
                let db_path = repo.commondir().join(".graphite_metadata.db");
                let Ok(conn) = rusqlite::Connection::open_with_flags(
                    &db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                ) else {
                    return false;
                };
                let result: rusqlite::Result<String> = conn.query_row(
                    "SELECT parent_branch_revision FROM branch_metadata WHERE branch_name = ?1",
                    [&self.branch],
                    |row| row.get(0),
                );
                result
                    .map(|revision| revision == self.expected_revision)
                    .unwrap_or(false)
            }
            MetadataFormat::Refs => {
                let refname = format!("refs/branch-metadata/{}", self.branch);
                let Ok(reference) = repo.find_reference(&refname) else {
                    return false;
                };
                let Ok(object) = reference.peel(git2::ObjectType::Blob) else {
                    return false;
                };
                let Ok(blob) = object.into_blob() else {
                    return false;
                };
                let Ok(json) = serde_json::from_slice::<serde_json::Value>(blob.content()) else {
                    return false;
                };
                json.get("parentBranchRevision")
                    .and_then(|v| v.as_str())
                    .map(|revision| revision == self.expected_revision)
                    .unwrap_or(false)
            }
        }
    }
}

pub fn has_metadata_parent_revision(
    format: MetadataFormat,
    branch: impl Into<String>,
    revision: impl Into<String>,
) -> HasMetadataParentRevisionPredicate {
    HasMetadataParentRevisionPredicate {
        format,
        branch: branch.into(),
        expected_revision: revision.into(),
    }
}
