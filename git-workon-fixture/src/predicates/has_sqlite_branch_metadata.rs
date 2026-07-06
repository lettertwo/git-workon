use crate::predicates::metadata_common::sqlite_metadata_field;
use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasSqliteBranchMetadataPredicate {
    branch: String,
    expected_parent: String,
}

impl PredicateReflection for HasSqliteBranchMetadataPredicate {}

impl fmt::Display for HasSqliteBranchMetadataPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            ".graphite_metadata.db has branch_metadata row for '{}' with parent_branch_name '{}'",
            self.branch, self.expected_parent
        )
    }
}

impl Predicate<Repository> for HasSqliteBranchMetadataPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        sqlite_metadata_field(repo, &self.branch, "parent_branch_name")
            .map(|parent| parent == self.expected_parent)
            .unwrap_or(false)
    }
}

pub fn has_sqlite_branch_metadata(
    branch: impl Into<String>,
    parent: impl Into<String>,
) -> HasSqliteBranchMetadataPredicate {
    HasSqliteBranchMetadataPredicate {
        branch: branch.into(),
        expected_parent: parent.into(),
    }
}
