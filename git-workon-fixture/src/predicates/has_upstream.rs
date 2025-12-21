use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HasUpstreamPredicate {
    branch: String,
    expected_upstream: Option<String>,
}

impl PredicateReflection for HasUpstreamPredicate {}

impl fmt::Display for HasUpstreamPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.expected_upstream {
            Some(upstream) => write!(f, "branch '{}' has upstream '{}'", self.branch, upstream),
            None => write!(f, "branch '{}' has an upstream", self.branch),
        }
    }
}

impl Predicate<Repository> for HasUpstreamPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let branch = match repo.find_branch(&self.branch, git2::BranchType::Local) {
            Ok(b) => b,
            Err(_) => return false,
        };

        match branch.upstream() {
            Ok(upstream) => match &self.expected_upstream {
                Some(expected) => {
                    // Check if the upstream name matches
                    upstream.name().ok().flatten() == Some(expected.as_str())
                        || upstream.name_bytes().ok()
                            == Some(format!("refs/remotes/{}", expected).as_bytes())
                }
                None => true, // Just checking that upstream exists
            },
            Err(_) => false,
        }
    }
}

pub fn has_upstream(branch: &str, expected_upstream: Option<&str>) -> HasUpstreamPredicate {
    HasUpstreamPredicate {
        branch: branch.to_string(),
        expected_upstream: expected_upstream.map(|s| s.to_string()),
    }
}
