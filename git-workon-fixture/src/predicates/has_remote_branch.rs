use std::fmt;

use git2::{BranchType, Repository};
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HasRemoteBranchPredicate {
    name: String,
}

impl PredicateReflection for HasRemoteBranchPredicate {}

impl fmt::Display for HasRemoteBranchPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "remote branch {} exists", self.name)
    }
}

impl Predicate<Repository> for HasRemoteBranchPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        repo.find_branch(&self.name, BranchType::Remote).is_ok()
    }
}

pub fn has_remote_branch(name: &str) -> HasRemoteBranchPredicate {
    HasRemoteBranchPredicate {
        name: name.to_string(),
    }
}
