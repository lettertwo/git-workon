use crate::predicates::metadata_common::refs_metadata_json;
use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasBranchMetadataPredicate {
    branch: String,
    expected_parent: String,
}

impl PredicateReflection for HasBranchMetadataPredicate {}

impl fmt::Display for HasBranchMetadataPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refs/branch-metadata/{} has parentBranchName '{}'",
            self.branch, self.expected_parent
        )
    }
}

impl Predicate<Repository> for HasBranchMetadataPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let Some(json) = refs_metadata_json(repo, &self.branch) else {
            return false;
        };
        json.get("parentBranchName")
            .and_then(|v| v.as_str())
            .map(|p| p == self.expected_parent)
            .unwrap_or(false)
    }
}

pub fn has_branch_metadata(
    branch: impl Into<String>,
    parent: impl Into<String>,
) -> HasBranchMetadataPredicate {
    HasBranchMetadataPredicate {
        branch: branch.into(),
        expected_parent: parent.into(),
    }
}
