use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub enum HasConfigPredicate {
    Exists { key: String },
    Equals { key: String, value: String },
}

impl PredicateReflection for HasConfigPredicate {}

impl fmt::Display for HasConfigPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HasConfigPredicate::Exists { key } => write!(f, "config key '{}' exists", key),
            HasConfigPredicate::Equals { key, value } => {
                write!(f, "config key '{}' equals '{}'", key, value)
            }
        }
    }
}

impl Predicate<Repository> for HasConfigPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        match self {
            HasConfigPredicate::Exists { key } => repo
                .config()
                .and_then(|config| config.get_string(key))
                .is_ok(),
            HasConfigPredicate::Equals { key, value } => repo
                .config()
                .and_then(|config| config.get_string(key))
                .map(|v| v == *value)
                .unwrap_or(false),
        }
    }
}

pub fn has_config<K: Into<String>, V: Into<String>>(
    key: K,
    value: Option<V>,
) -> HasConfigPredicate {
    match value {
        Some(v) => HasConfigPredicate::Equals {
            key: key.into(),
            value: v.into(),
        },
        None => HasConfigPredicate::Exists { key: key.into() },
    }
}
