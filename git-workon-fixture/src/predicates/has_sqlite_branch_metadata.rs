use git2::Repository;
use predicates::prelude::Predicate;
use predicates::reflection::PredicateReflection;
use std::fmt;

pub struct HasSqliteBranchMetadataPredicate {
    branch: String,
    expected_parent: String,
}

impl PredicateReflection for HasSqliteBranchMetadataPredicate {}

impl fmt::Display for HasSqliteBranchMetadataPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            ".graphite_metadata.db has branch_metadata row for '{}' with parent_branch_name '{}'",
            self.branch, self.expected_parent
        )
    }
}

impl Predicate<Repository> for HasSqliteBranchMetadataPredicate {
    fn eval(&self, repo: &Repository) -> bool {
        let db_path = repo.commondir().join(".graphite_metadata.db");
        let Ok(conn) = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            return false;
        };
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT parent_branch_name FROM branch_metadata WHERE branch_name = ?1",
            [&self.branch],
            |row| row.get(0),
        );
        result
            .map(|parent| parent == self.expected_parent)
            .unwrap_or(false)
    }
}

pub fn has_sqlite_branch_metadata(
    branch: impl Into<String>,
    parent: impl Into<String>,
) -> HasSqliteBranchMetadataPredicate {
    HasSqliteBranchMetadataPredicate {
        branch: branch.into(),
        expected_parent: parent.into(),
    }
}
