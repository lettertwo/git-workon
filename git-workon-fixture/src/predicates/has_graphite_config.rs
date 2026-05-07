use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasGraphiteConfigPredicate {
    expected_trunks: Vec<String>,
}

impl PredicateReflection for HasGraphiteConfigPredicate {}

impl fmt::Display for HasGraphiteConfigPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            ".graphite_repo_config has trunks {:?}",
            self.expected_trunks
        )
    }
}

impl Predicate<Repository> for HasGraphiteConfigPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let path = repo.path().join(".graphite_repo_config");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
            return false;
        };
        let actual: Vec<String> =
            if let Some(trunks) = json.get("trunks").and_then(|t| t.as_array()) {
                trunks
                    .iter()
                    .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
                    .map(String::from)
                    .collect()
            } else if let Some(trunk) = json.get("trunk").and_then(|t| t.as_str()) {
                vec![trunk.to_string()]
            } else {
                vec![]
            };
        actual == self.expected_trunks
    }
}

pub fn has_graphite_config(trunks: &[&str]) -> HasGraphiteConfigPredicate {
    HasGraphiteConfigPredicate {
        expected_trunks: trunks.iter().map(|s| s.to_string()).collect(),
    }
}
