use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasConfigMultivarPredicate {
    key: String,
    expected_values: Vec<String>,
}

impl PredicateReflection for HasConfigMultivarPredicate {}

impl fmt::Display for HasConfigMultivarPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "config key '{}' has values {:?}",
            self.key, self.expected_values
        )
    }
}

impl Predicate<Repository> for HasConfigMultivarPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let config = match repo.config() {
            Ok(c) => c,
            Err(_) => return false,
        };

        let mut actual_values = Vec::new();
        if let Ok(mut entries) = config.multivar(&self.key, None) {
            while let Some(entry) = entries.next() {
                if let Ok(e) = entry {
                    if let Some(v) = e.value() {
                        actual_values.push(v.to_string());
                    }
                }
            }
        }

        actual_values == self.expected_values
    }
}

pub fn has_config_multivar<K: Into<String>>(
    key: K,
    expected_values: Vec<String>,
) -> HasConfigMultivarPredicate {
    HasConfigMultivarPredicate {
        key: key.into(),
        expected_values,
    }
}
