use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasStashPredicate {
    label_substring: Option<String>,
}

impl PredicateReflection for HasStashPredicate {}

impl fmt::Display for HasStashPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.label_substring {
            Some(s) => write!(f, "stash contains entry matching '{}'", s),
            None => write!(f, "stash is empty"),
        }
    }
}

impl Predicate<Repository> for HasStashPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        // stash_foreach needs &mut, so re-open via the repo path.
        let Ok(mut mutable) = Repository::open(repo.path()) else {
            return false;
        };
        let want = self.label_substring.clone();
        let mut found = false;
        let _ = mutable.stash_foreach(|_index, message, _oid| {
            if let Some(ref substr) = want {
                if message.contains(substr.as_str()) {
                    found = true;
                    return false; // stop iterating
                }
            } else {
                // has_no_stash variant: any entry means predicate is false
                found = true;
                return false;
            }
            true
        });
        match &self.label_substring {
            Some(_) => found,
            None => !found, // has_no_stash: true when stash is empty
        }
    }
}

/// Assert that the stash contains at least one entry whose message contains
/// `label_substring`.
pub fn has_stash(label_substring: impl Into<String>) -> HasStashPredicate {
    HasStashPredicate {
        label_substring: Some(label_substring.into()),
    }
}

/// Assert that the stash is empty (no entries at all).
pub fn has_no_stash() -> HasStashPredicate {
    HasStashPredicate {
        label_substring: None,
    }
}
