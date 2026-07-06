use crate::fixture_builder::MetadataFormat;
use crate::predicates::metadata_common::{refs_metadata_json, sqlite_metadata_field};
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
                sqlite_metadata_field(repo, &self.branch, "parent_branch_revision")
                    .map(|revision| revision == self.expected_revision)
                    .unwrap_or(false)
            }
            MetadataFormat::Refs => {
                let Some(json) = refs_metadata_json(repo, &self.branch) else {
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
