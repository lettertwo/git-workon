#[cfg(test)]
mod fixture_builder {
    use git2::BranchType;
    use git_workon_fixture::prelude::*;

    #[test]
    fn default() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().build()?;
        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(!git_repo.is_bare(), "Repo should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("main", BranchType::Local);
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
        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(git_repo.is_bare(), "Repo should be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("main", BranchType::Local);
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

    #[test]
    fn default_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().default_branch("develop").build()?;

        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(!git_repo.is_bare(), "Repo should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("develop", BranchType::Local);
        assert!(branch.is_ok(), "Default branch 'develop' should exist");

        assert!(
            git_repo.find_branch("main", BranchType::Local).is_err(),
            "Main branch should not exist when default branch is set"
        );

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
            repo.assert(predicate::repo::has_branch("develop"));
            repo.assert(predicate::repo::has_branch("main").not());
            repo.assert(predicate::repo::head_matches("develop"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }

    #[test]
    fn default_branch_bare() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("develop")
            .build()?;

        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(git_repo.is_bare(), "Repo should be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("develop", BranchType::Local);
        assert!(branch.is_ok(), "Default branch 'develop' should exist");

        assert!(
            git_repo.find_branch("main", BranchType::Local).is_err(),
            "Main branch should not exist when default branch is set"
        );

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
            repo.assert(predicate::repo::has_branch("develop"));
            repo.assert(predicate::repo::has_branch("main").not());
            repo.assert(predicate::repo::head_matches("develop"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }

    #[test]
    fn worktree() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().worktree("dev").build()?;

        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(!git_repo.is_bare(), "Repo should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("main", BranchType::Local);
        assert!(branch.is_ok(), "Default branch 'main' should exist");

        // Check that the worktree branch exists
        let worktree_branch = git_repo.find_branch("dev", BranchType::Local);
        assert!(
            worktree_branch.is_ok(),
            "Worktree branch 'dev' should exist"
        );

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
            path.child(".git").assert(predicate::path::is_file());
            path.child(".git")
                .assert(predicate::str::contains("gitdir: "));
            path.child(".git").assert(predicate::str::contains(
                path.parent().unwrap().to_string_lossy(),
            ));

            repo.assert(predicate::repo::is_empty().not());
            repo.assert(predicate::repo::is_bare().not());
            repo.assert(predicate::repo::is_worktree());
            repo.assert(predicate::repo::has_branch("main"));
            repo.assert(predicate::repo::has_branch("dev"));
            repo.assert(predicate::repo::has_worktree("dev"));
            repo.assert(predicate::repo::head_matches("dev"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }

    #[test]
    fn worktree_default_branch() -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            FixtureBuilder::new().worktree("main").build().is_err(),
            "Worktree should not be created without a bare repository"
        );

        let fixture = FixtureBuilder::new()
            .worktree("main")
            .default_branch("develop")
            .build()?;

        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(!git_repo.is_bare(), "Repo should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("develop", BranchType::Local);
        assert!(branch.is_ok(), "Default branch 'develop' should exist");

        assert!(
            git_repo.find_branch("main", BranchType::Local).is_ok(),
            "Main branch should exist because worktree is set to main"
        );

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
            path.child(".git").assert(predicate::path::is_file());
            path.child(".git")
                .assert(predicate::str::contains("gitdir: "));
            path.child(".git").assert(predicate::str::contains(
                path.parent().unwrap().to_string_lossy(),
            ));

            repo.assert(predicate::repo::is_empty().not());
            repo.assert(predicate::repo::is_bare().not());
            repo.assert(predicate::repo::is_worktree());
            repo.assert(predicate::repo::has_branch("develop"));
            repo.assert(predicate::repo::has_branch("main"));
            repo.assert(predicate::repo::has_worktree("main"));
            repo.assert(predicate::repo::head_matches("main"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }

    #[test]
    fn worktree_bare() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).worktree("dev").build()?;

        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(!git_repo.is_bare(), "worktree should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("main", BranchType::Local);
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

            // Shoud have a .git file in a worktree
            path.child(".git").assert(predicate::path::is_file());

            repo.assert(predicate::repo::is_empty().not());
            repo.assert(predicate::repo::is_bare().not());
            repo.assert(predicate::repo::is_worktree());
            repo.assert(predicate::repo::has_branch("main"));
            repo.assert(predicate::repo::has_branch("dev"));
            repo.assert(predicate::repo::has_worktree("dev"));
            repo.assert(predicate::repo::head_matches("dev"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }

    #[test]
    fn worktree_bare_custom_default_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("develop")
            .worktree("dev")
            .build()?;

        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(!git_repo.is_bare(), "worktree should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("develop", BranchType::Local);
        assert!(branch.is_ok(), "Default branch 'develop' should exist");

        // Check that the worktree branch exists
        let worktree_branch = git_repo.find_branch("dev", BranchType::Local);
        assert!(
            worktree_branch.is_ok(),
            "Worktree branch 'dev' should exist"
        );

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

            // Shoud have a .git file in a worktree
            path.child(".git").assert(predicate::path::is_file());

            repo.assert(predicate::repo::is_empty().not());
            repo.assert(predicate::repo::is_bare().not());
            repo.assert(predicate::repo::is_worktree());
            repo.assert(predicate::repo::has_branch("develop"));
            repo.assert(predicate::repo::has_branch("dev"));
            repo.assert(predicate::repo::has_worktree("dev"));
            repo.assert(predicate::repo::head_matches("dev"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }

    #[test]
    fn worktree_bare_matching_default_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).worktree("main").build()?;

        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(!git_repo.is_bare(), "worktree should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("main", BranchType::Local);
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

            // Shoud have a .git file in a worktree
            path.child(".git").assert(predicate::path::is_file());

            repo.assert(predicate::repo::is_empty().not());
            repo.assert(predicate::repo::is_bare().not());
            repo.assert(predicate::repo::is_worktree());
            repo.assert(predicate::repo::has_worktree("main"));
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
    fn worktree_bare_matching_custom_default_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("dev")
            .worktree("dev")
            .build()?;

        let git_repo = fixture.repo.as_ref().unwrap();

        assert!(!git_repo.is_bare(), "worktree should not be bare");
        assert!(!git_repo.is_empty().unwrap(), "Repo should not be empty");

        // Check that the default branch exists
        let branch = git_repo.find_branch("dev", BranchType::Local);
        assert!(branch.is_ok(), "Default branch 'dev' should exist");

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

            // Shoud have a .git file in a worktree
            path.child(".git").assert(predicate::path::is_file());

            repo.assert(predicate::repo::is_empty().not());
            repo.assert(predicate::repo::is_bare().not());
            repo.assert(predicate::repo::is_worktree());
            repo.assert(predicate::repo::has_branch("dev"));
            repo.assert(predicate::repo::has_worktree("dev"));
            repo.assert(predicate::repo::head_matches("dev"));
            repo.assert(predicate::repo::head_commit_message_contains(
                "Initial commit",
            ));
            repo.assert(predicate::repo::head_commit_parent_count(0));
        });

        Ok(())
    }
}
