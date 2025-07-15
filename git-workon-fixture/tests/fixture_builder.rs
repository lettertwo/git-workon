#[cfg(test)]
mod fixture_builder {
    use git_workon_fixture::prelude::*;

    #[test]
    fn default() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().build()?;
        let repo = fixture.repo.as_ref().unwrap();

        let git_repo = repo.unwrap();

        assert!(!git_repo.is_bare(), "Repo should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("main", git2::BranchType::Local);
        assert!(branch.is_ok(), "Default branch 'main' should exist");

        // Check that the branch has an initial commit with no parent
        let head = git_repo.head()?;
        let commit = head.peel_to_commit()?;

        assert_eq!(
            commit.parent_count(),
            0,
            "Initial commit should have no parents"
        );

        // Check that the branch has the initial commit
        assert_eq!(
            commit.message(),
            Some("Initial commit"),
            "Initial commit message should match"
        );

        fixture.with(|repo, path| {
            path.assert(predicate::path::is_dir());
            path.child(".git").assert(predicate::path::is_dir());
            path.child(".git/config")
                .assert(predicate::str::contains("bare = false"));

            repo.assert(predicate::repo::is_empty().not());
            repo.assert(predicate::repo::is_bare().not());
            repo.assert(predicate::repo::is_worktree().not());
            repo.assert(predicate::repo::has_branch("main"));
            repo.assert(predicate::repo::head_matches("main"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }

    #[test]
    fn bare() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).build()?;

        let repo = fixture.repo.as_ref().unwrap();

        // Direct assertions on git2::Repository
        let git_repo = repo.unwrap(); // Access the inner git2::Repository

        assert!(git_repo.is_bare(), "Repo should be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("main", git2::BranchType::Local);
        assert!(branch.is_ok(), "Default branch 'main' should exist");

        // Check that the branch has an initial commit with no parent
        let head = git_repo.head()?;
        let commit = head.peel_to_commit()?;

        assert_eq!(
            commit.parent_count(),
            0,
            "Initial commit should have no parents"
        );

        // Check that the branch has the initial commit
        assert_eq!(
            commit.message(),
            Some("Initial commit"),
            "Initial commit message should match"
        );

        fixture.with(|repo, path| {
            path.assert(predicate::path::is_dir());

            // Shoud not have a .git directory in a bare repo
            path.child(".git").assert(predicate::path::missing());

            path.child("config")
                .assert(predicate::str::contains("bare = true"));

            repo.assert(predicate::repo::is_empty().not());
            repo.assert(predicate::repo::is_bare());
            repo.assert(predicate::repo::is_worktree().not());
            repo.assert(predicate::repo::has_branch("main"));
            repo.assert(predicate::repo::head_matches("main"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }
}
