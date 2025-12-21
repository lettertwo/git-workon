use std::fmt;

use git2::{Oid, Repository};
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct BranchPointsToPredicate {
    branch: String,
    commit_oid: Oid,
}

impl PredicateReflection for BranchPointsToPredicate {}

impl fmt::Display for BranchPointsToPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "branch '{}' points to commit {}",
            self.branch, self.commit_oid
        )
    }
}

impl Predicate<Repository> for BranchPointsToPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        match repo.find_branch(&self.branch, git2::BranchType::Local) {
            Ok(branch) => match branch.get().target() {
                Some(oid) => oid == self.commit_oid,
                None => false,
            },
            Err(_) => false,
        }
    }
}

pub fn branch_points_to(branch: &str, commit_oid: Oid) -> BranchPointsToPredicate {
    BranchPointsToPredicate {
        branch: branch.to_string(),
        commit_oid,
    }
}
