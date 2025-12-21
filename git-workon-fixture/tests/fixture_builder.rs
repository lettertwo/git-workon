#[cfg(test)]
mod fixture_builder {
    use std::ffi::OsStr;

    use git2::{BranchType, Repository};
    use git_workon_fixture::prelude::*;

    #[test]
    fn default() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().build()?;
        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        dir.child(".git").assert(predicate::path::is_dir());
        dir.child(".git/config")
            .assert(predicate::str::contains("bare = false"));

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare().not());
        fixture.assert(predicate::repo::is_worktree().not());
        fixture.assert(predicate::repo::has_branch("main"));
        fixture.assert(predicate::repo::head_matches("main"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn bare() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).build()?;
        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        // Shoud not have a .git directory in a bare repo
        dir.child(".git").assert(predicate::path::missing());
        dir.child("config")
            .assert(predicate::str::contains("bare = true"));

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare());
        fixture.assert(predicate::repo::is_worktree().not());
        fixture.assert(predicate::repo::has_branch("main"));
        fixture.assert(predicate::repo::head_matches("main"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn default_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().default_branch("develop").build()?;

        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        dir.child(".git").assert(predicate::path::is_dir());
        dir.child(".git/config")
            .assert(predicate::str::contains("bare = false"));

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare().not());
        fixture.assert(predicate::repo::is_worktree().not());
        fixture.assert(predicate::repo::has_branch("develop"));
        fixture.assert(predicate::repo::has_branch("main").not());
        fixture.assert(predicate::repo::head_matches("develop"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn default_branch_bare() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("develop")
            .build()?;

        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        // Shoud not have a .git directory in a bare repo
        dir.child(".git").assert(predicate::path::missing());
        dir.child("config")
            .assert(predicate::str::contains("bare = true"));

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare());
        fixture.assert(predicate::repo::is_worktree().not());
        fixture.assert(predicate::repo::has_branch("develop"));
        fixture.assert(predicate::repo::has_branch("main").not());
        fixture.assert(predicate::repo::head_matches("develop"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn worktree() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().worktree("dev").build()?;

        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        dir.child(".git").assert(predicate::path::is_file());
        dir.child(".git")
            .assert(predicate::str::contains("gitdir: "));
        dir.child(".git").assert(predicate::str::contains(
            dir.parent().unwrap().to_string_lossy(),
        ));

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare().not());
        fixture.assert(predicate::repo::is_worktree());
        fixture.assert(predicate::repo::has_branch("main"));
        fixture.assert(predicate::repo::has_branch("dev"));
        fixture.assert(predicate::repo::has_worktree("dev"));
        fixture.assert(predicate::repo::head_matches("dev"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

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

        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        dir.child(".git").assert(predicate::path::is_file());
        dir.child(".git")
            .assert(predicate::str::contains("gitdir: "));
        dir.child(".git").assert(predicate::str::contains(
            dir.parent().unwrap().to_string_lossy(),
        ));

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare().not());
        fixture.assert(predicate::repo::is_worktree());
        fixture.assert(predicate::repo::has_branch("develop"));
        fixture.assert(predicate::repo::has_branch("main"));
        fixture.assert(predicate::repo::has_worktree("main"));
        fixture.assert(predicate::repo::head_matches("main"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn worktree_bare() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).worktree("dev").build()?;

        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        // Shoud have a .git file in a worktree
        dir.child(".git").assert(predicate::path::is_file());

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare().not());
        fixture.assert(predicate::repo::is_worktree());
        fixture.assert(predicate::repo::has_branch("main"));
        fixture.assert(predicate::repo::has_branch("dev"));
        fixture.assert(predicate::repo::has_worktree("dev"));
        fixture.assert(predicate::repo::head_matches("dev"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn worktree_bare_custom_default_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("develop")
            .worktree("dev")
            .build()?;

        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        // Shoud have a .git file in a worktree
        dir.child(".git").assert(predicate::path::is_file());

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare().not());
        fixture.assert(predicate::repo::is_worktree());
        fixture.assert(predicate::repo::has_branch("develop"));
        fixture.assert(predicate::repo::has_branch("dev"));
        fixture.assert(predicate::repo::has_worktree("dev"));
        fixture.assert(predicate::repo::head_matches("dev"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn worktree_bare_matching_default_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).worktree("main").build()?;

        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        // Shoud have a .git file in a worktree
        dir.child(".git").assert(predicate::path::is_file());

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare().not());
        fixture.assert(predicate::repo::is_worktree());
        fixture.assert(predicate::repo::has_worktree("main"));
        fixture.assert(predicate::repo::has_branch("main"));
        fixture.assert(predicate::repo::head_matches("main"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn worktree_bare_matching_custom_default_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("dev")
            .worktree("dev")
            .build()?;

        let git_repo = fixture.repo()?;

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

        let dir = fixture.cwd()?;
        dir.assert(predicate::path::is_dir());
        // Shoud have a .git file in a worktree
        dir.child(".git").assert(predicate::path::is_file());

        fixture.assert(predicate::repo::is_empty().not());
        fixture.assert(predicate::repo::is_bare().not());
        fixture.assert(predicate::repo::is_worktree());
        fixture.assert(predicate::repo::has_branch("dev"));
        fixture.assert(predicate::repo::has_worktree("dev"));
        fixture.assert(predicate::repo::head_matches("dev"));
        fixture.assert(predicate::repo::head_commit_message_contains(
            "Initial commit",
        ));
        fixture.assert(predicate::repo::head_commit_parent_count(0));

        Ok(())
    }

    #[test]
    fn with_remote() -> Result<(), Box<dyn std::error::Error>> {
        // Create origin repository
        let origin = FixtureBuilder::new().bare(true).build()?;

        // Create local repository with remote
        let local = FixtureBuilder::new()
            .bare(true)
            .remote("origin", &origin)
            .build()?;

        // Verify remote exists
        local.assert(predicate::repo::has_remote("origin"));
        local.assert(predicate::repo::has_remote_url(
            "origin",
            origin.cwd()?.to_str(),
        ));

        Ok(())
    }

    #[test]
    fn with_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create origin repository
        let origin = FixtureBuilder::new().bare(true).build()?;

        // Create local repository with remote and upstream tracking
        let local = FixtureBuilder::new()
            .bare(true)
            .remote("origin", &origin)
            .upstream("main", "origin/main")
            .build()?;

        // Verify remote exists
        local.assert(predicate::repo::has_remote("origin"));

        // Verify upstream is configured
        local.assert(predicate::repo::has_upstream("main", Some("origin/main")));

        // Verify remote tracking ref was created
        local.assert(predicate::repo::has_remote_branch("origin/main"));

        Ok(())
    }

    #[test]
    fn fixture_add_remote() -> Result<(), Box<dyn std::error::Error>> {
        let origin = FixtureBuilder::new().bare(true).build()?;
        let local = FixtureBuilder::new().bare(true).build()?;

        // Add remote after fixture creation
        local.add_remote("origin", origin.cwd()?.to_str().unwrap())?;

        local.assert(predicate::repo::has_remote("origin"));

        Ok(())
    }

    #[test]
    fn fixture_create_remote_ref() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).build()?;
        let commit_oid = fixture.head()?.peel_to_commit()?.id();

        // Create remote ref
        fixture.create_remote_ref("origin/main", commit_oid)?;

        // Verify remote ref exists
        fixture.assert(predicate::repo::has_remote_branch("origin/main"));

        Ok(())
    }

    #[test]
    fn fixture_set_upstream() -> Result<(), Box<dyn std::error::Error>> {
        let origin = FixtureBuilder::new().bare(true).build()?;
        let fixture = FixtureBuilder::new().bare(true).build()?;

        let commit_oid = fixture.head()?.peel_to_commit()?.id();

        // Add remote first
        fixture.add_remote("origin", origin.cwd()?.to_str().unwrap())?;

        // Create remote ref
        fixture.create_remote_ref("origin/main", commit_oid)?;

        // Set upstream
        fixture.set_upstream("main", "origin/main")?;

        // Verify upstream is configured
        fixture.assert(predicate::repo::has_upstream("main", Some("origin/main")));

        Ok(())
    }

    #[test]
    fn fixture_commit_builder() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).worktree("main").build()?;

        // Create a commit with multiple files
        let commit_oid = fixture
            .commit("main")
            .file("file1.txt", "content1")
            .file("file2.txt", "content2")
            .create("Add two files")?;

        let repo = fixture.repo()?;

        // Verify commit was created
        let commit = repo.find_commit(commit_oid)?;
        assert_eq!(commit.message(), Some("Add two files"));
        assert_eq!(commit.parent_count(), 1);

        // Verify files exist in the commit tree
        let tree = commit.tree()?;
        assert!(tree.get_name("file1.txt").is_some());
        assert!(tree.get_name("file2.txt").is_some());

        Ok(())
    }

    #[test]
    fn fixture_update_branch() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).worktree("main").build()?;

        // Create a commit
        let commit_oid = fixture
            .commit("main")
            .file("test.txt", "content")
            .create("Test commit")?;

        // Create a new branch pointing to initial commit
        let repo = fixture.repo()?;
        let initial_commit = repo.head()?.peel_to_commit()?.parent(0)?.id();
        repo.branch("feature", &repo.find_commit(initial_commit)?, false)?;

        // Update feature branch to point to new commit
        fixture.update_branch("feature", commit_oid)?;

        // Verify branch points to the new commit
        repo.assert(predicate::repo::branch_points_to("feature", commit_oid));

        Ok(())
    }

    #[test]
    fn multiple_worktrees() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .worktree("main")
            .worktree("feature")
            .worktree("docs")
            .build()?;

        let bare_path = fixture.root()?.join(".bare");
        let bare_repo = Repository::open(&bare_path)?;

        // Verify all worktrees were created
        bare_repo.assert(predicate::repo::has_worktree("main"));
        bare_repo.assert(predicate::repo::has_worktree("feature"));
        bare_repo.assert(predicate::repo::has_worktree("docs"));

        // Verify the Fixture is opened in the last worktree (docs)
        assert_eq!(fixture.cwd()?.file_name(), Some(OsStr::new("docs")));

        // Verify we can use the fixture to access the docs worktree
        assert_eq!(fixture.head()?.name(), Some("refs/heads/docs"));

        Ok(())
    }

    #[test]
    fn multiple_worktrees_with_commits() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .worktree("main")
            .worktree("feature")
            .build()?;

        // Create commits in each worktree
        let parent_path = fixture.root()?;

        // Commit in main
        fixture
            .commit("main")
            .file("main.txt", "main content")
            .create("Add main.txt")?;

        // Commit in feature (this is the current worktree)
        fixture
            .commit("feature")
            .file("feature.txt", "feature content")
            .create("Add feature.txt")?;

        // Verify both worktrees have their respective files
        assert!(parent_path.join("main/main.txt").exists());
        assert!(parent_path.join("feature/feature.txt").exists());

        Ok(())
    }

    #[test]
    fn multiple_worktrees_order_matters() -> Result<(), Box<dyn std::error::Error>> {
        // Create fixture with worktrees in different order
        let fixture1 = FixtureBuilder::new()
            .bare(true)
            .worktree("first")
            .worktree("second")
            .build()?;

        let fixture2 = FixtureBuilder::new()
            .bare(true)
            .worktree("second")
            .worktree("first")
            .build()?;

        // Fixture should be opened in the last specified worktree
        assert_eq!(fixture1.cwd()?.file_name(), Some(OsStr::new("second")));
        assert_eq!(fixture2.cwd()?.file_name(), Some(OsStr::new("first")));

        Ok(())
    }
}
