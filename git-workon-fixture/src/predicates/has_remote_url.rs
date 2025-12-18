use std::fmt;

use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;

pub struct HasRemoteUrlPredicate {
    remote_name: String,
    url: Option<String>,
}

impl PredicateReflection for HasRemoteUrlPredicate {}

impl fmt::Display for HasRemoteUrlPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.url {
            Some(url) => write!(f, "remote '{}' has URL '{}'", self.remote_name, url),
            None => write!(f, "remote '{}' has a URL", self.remote_name),
        }
    }
}

impl Predicate<Repository> for HasRemoteUrlPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        match repo.find_remote(&self.remote_name) {
            Ok(remote) => match &self.url {
                Some(expected_url) => remote.url().map(|u| u == expected_url).unwrap_or(false),
                None => remote.url().is_some(),
            },
            Err(_) => false,
        }
    }
}

pub fn has_remote_url(remote_name: &str, url: Option<&str>) -> HasRemoteUrlPredicate {
    HasRemoteUrlPredicate {
        remote_name: remote_name.to_string(),
        url: url.map(|u| u.to_string()),
    }
}
