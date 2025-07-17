use std::fmt;

use git2::{BranchType, Repository};
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HasBranchPredicate {
    name: String,
}

impl PredicateReflection for HasBranchPredicate {}

impl fmt::Display for HasBranchPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "branch {} exists", self.name)
    }
}

impl Predicate<Repository> for HasBranchPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        repo.find_branch(&self.name, BranchType::Local).is_ok()
    }
}

pub fn has_branch(name: &str) -> HasBranchPredicate {
    HasBranchPredicate {
        name: name.to_string(),
    }
}
