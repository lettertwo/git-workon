use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HasRemotePredicate {
    name: String,
}

impl PredicateReflection for HasRemotePredicate {}

impl fmt::Display for HasRemotePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "remote {} exists", self.name)
    }
}

impl Predicate<Repository> for HasRemotePredicate {
    fn eval(&self, repo: &Repository) -> bool {
        repo.find_remote(&self.name).is_ok()
    }
}

pub fn has_remote(name: &str) -> HasRemotePredicate {
    HasRemotePredicate {
        name: name.to_string(),
    }
}
