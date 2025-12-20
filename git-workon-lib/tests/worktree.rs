#[cfg(test)]
mod tests {
    use git2::Repository;
    use git_workon_fixture::prelude::*;
    use workon::{add_worktree, BranchType};

    #[test]
    fn test_add_worktree_basic() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature-branch", BranchType::Normal)?;

        // Verify the worktree was created
        assert!(worktree.path().exists());
        assert_eq!(worktree.name(), Some("feature-branch"));

        // Verify the branch was created
        repo.assert(predicate::repo::has_branch("feature-branch"));
        repo.assert(predicate::repo::has_worktree("feature-branch"));

        Ok(())
    }

    #[test]
    fn test_add_worktree_with_slashes() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a worktree with slashes in the name
        let worktree = add_worktree(&repo, "user/feature-branch", BranchType::Normal)?;

        // Verify the worktree was created
        assert!(worktree.path().exists());
        assert_eq!(worktree.name(), Some("feature-branch"));

        // Verify the branch was created
        repo.assert(predicate::repo::has_branch("user/feature-branch"));
        repo.assert(predicate::repo::has_worktree("feature-branch"));

        Ok(())
    }

    #[test]
    fn test_add_worktree_orphan() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add an orphan worktree
        let worktree = add_worktree(&repo, "docs", BranchType::Orphan)?;

        // Verify the worktree was created
        assert!(worktree.path().exists());
        assert_eq!(worktree.name(), Some("docs"));
        repo.assert(predicate::repo::has_worktree("docs"));

        // Open the orphan worktree and verify it's truly orphaned
        let orphan_repo = Repository::open(worktree.path())?;

        // Verify HEAD points to the docs branch
        let head = orphan_repo.head()?;
        assert_eq!(head.name(), Some("refs/heads/docs"));
        assert!(head.is_branch(), "HEAD should be a branch");

        // Verify the branch has exactly one commit (the initial empty commit)
        let head_commit = head.peel_to_commit()?;
        assert_eq!(
            head_commit.parent_count(),
            0,
            "Orphan branch should have no parent commits"
        );
        assert_eq!(head_commit.message(), Some("Initial commit"));

        // Verify the commit tree is empty
        let tree = head_commit.tree()?;
        assert_eq!(tree.len(), 0, "Initial commit should have an empty tree");

        // Verify the index is empty
        let index = orphan_repo.index()?;
        assert_eq!(index.len(), 0, "Orphan worktree index should be empty");

        Ok(())
    }

    #[test]
    fn test_add_worktree_detach() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a detached worktree
        let worktree = add_worktree(&repo, "detached", BranchType::Detached)?;

        // Verify the worktree was created
        assert!(worktree.path().exists());
        assert_eq!(worktree.name(), Some("detached"));

        repo.assert(predicate::repo::has_worktree("detached"));

        Ok(())
    }

    #[test]
    fn test_worktree_branch_normal() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a normal worktree
        let worktree = add_worktree(&repo, "feature", BranchType::Normal)?;

        // Verify branch() returns the correct branch name
        assert_eq!(worktree.branch()?, Some("feature".to_string()));

        // Verify is_detached() returns false
        assert_eq!(worktree.is_detached()?, false);

        Ok(())
    }

    #[test]
    fn test_worktree_branch_with_slashes() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a worktree with slashes in the branch name
        let worktree = add_worktree(&repo, "user/feature-branch", BranchType::Normal)?;

        // Verify branch() returns the full branch name with slashes
        assert_eq!(worktree.branch()?, Some("user/feature-branch".to_string()));

        // Verify is_detached() returns false
        assert_eq!(worktree.is_detached()?, false);

        Ok(())
    }

    #[test]
    fn test_worktree_branch_orphan() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add an orphan worktree
        let worktree = add_worktree(&repo, "docs", BranchType::Orphan)?;

        // Verify branch() returns the correct branch name
        assert_eq!(worktree.branch()?, Some("docs".to_string()));

        // Verify is_detached() returns false (orphan is still on a branch)
        assert_eq!(worktree.is_detached()?, false);

        Ok(())
    }

    #[test]
    fn test_worktree_branch_detached() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a detached worktree
        let worktree = add_worktree(&repo, "detached", BranchType::Detached)?;

        // Verify branch() returns None for detached HEAD
        assert_eq!(worktree.branch()?, None);

        // Verify is_detached() returns true
        assert_eq!(worktree.is_detached()?, true);

        Ok(())
    }

    #[test]
    fn test_is_dirty_clean_worktree() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature", BranchType::Normal)?;

        // Verify the worktree is clean
        assert_eq!(worktree.is_dirty()?, false);

        Ok(())
    }

    #[test]
    fn test_is_dirty_with_modified_file() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch and initial file
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .worktree("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Create and commit a file in main worktree
        let main_path = fixture
            .path
            .as_ref()
            .unwrap()
            .parent()
            .unwrap()
            .join("main");
        std::fs::write(main_path.join("test.txt"), "original content")?;

        let worktree_repo = Repository::open(&main_path)?;
        let mut index = worktree_repo.index()?;
        index.add_path(std::path::Path::new("test.txt"))?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = worktree_repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Test", "test@test.com")?;
        let parent = worktree_repo.head()?.peel_to_commit()?;
        worktree_repo.commit(Some("HEAD"), &sig, &sig, "Add test file", &tree, &[&parent])?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature", BranchType::Normal)?;

        // Verify the worktree is clean
        assert_eq!(worktree.is_dirty()?, false);

        // Modify the file
        std::fs::write(worktree.path().join("test.txt"), "modified content")?;

        // Verify the worktree is now dirty
        assert_eq!(worktree.is_dirty()?, true);

        Ok(())
    }

    #[test]
    fn test_is_dirty_with_untracked_file() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature", BranchType::Normal)?;

        // Verify the worktree is clean
        assert_eq!(worktree.is_dirty()?, false);

        // Add an untracked file
        std::fs::write(worktree.path().join("untracked.txt"), "new file")?;

        // Verify the worktree is now dirty
        assert_eq!(worktree.is_dirty()?, true);

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_no_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature", BranchType::Normal)?;

        // Verify no unpushed commits (no upstream configured)
        assert_eq!(worktree.has_unpushed_commits()?, false);

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_with_upstream_up_to_date() -> Result<(), Box<dyn std::error::Error>>
    {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature", BranchType::Normal)?;

        // Create a remote (pointing to the bare repo itself)
        repo.remote("origin", fixture.path.as_ref().unwrap().to_str().unwrap())?;

        // Set up upstream tracking
        let feature_branch = repo.find_branch("feature", git2::BranchType::Local)?;
        let commit = feature_branch.get().peel_to_commit()?;

        // Create a fake remote ref at the same commit
        repo.reference(
            "refs/remotes/origin/feature",
            commit.id(),
            false,
            "create remote ref",
        )?;

        // Set upstream
        repo.find_branch("feature", git2::BranchType::Local)?
            .set_upstream(Some("origin/feature"))?;

        // Verify no unpushed commits (up to date with upstream)
        assert_eq!(worktree.has_unpushed_commits()?, false);

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_with_local_commits() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature", BranchType::Normal)?;

        // Create a remote (pointing to the bare repo itself)
        repo.remote("origin", fixture.path.as_ref().unwrap().to_str().unwrap())?;

        // Set up upstream tracking
        let feature_branch = repo.find_branch("feature", git2::BranchType::Local)?;
        let commit = feature_branch.get().peel_to_commit()?;

        // Create a fake remote ref at the initial commit
        repo.reference(
            "refs/remotes/origin/feature",
            commit.id(),
            false,
            "create remote ref",
        )?;

        // Set upstream
        repo.find_branch("feature", git2::BranchType::Local)?
            .set_upstream(Some("origin/feature"))?;

        // Create a new commit in the worktree (will be ahead of upstream)
        let worktree_repo = Repository::open(worktree.path())?;
        std::fs::write(worktree.path().join("test.txt"), "test")?;
        let mut index = worktree_repo.index()?;
        index.add_path(std::path::Path::new("test.txt"))?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = worktree_repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Test", "test@test.com")?;
        let parent = worktree_repo.head()?.peel_to_commit()?;
        worktree_repo.commit(Some("HEAD"), &sig, &sig, "New commit", &tree, &[&parent])?;

        // Verify has unpushed commits
        assert_eq!(worktree.has_unpushed_commits()?, true);

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_upstream_gone() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature", BranchType::Normal)?;

        // Set up upstream tracking with config (but no actual remote ref)
        let mut config = repo.config()?;
        config.set_str("branch.feature.remote", "origin")?;
        config.set_str("branch.feature.merge", "refs/heads/feature")?;

        // Verify has unpushed commits (upstream is gone, conservative)
        assert_eq!(worktree.has_unpushed_commits()?, true);

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_detached_head() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a detached worktree
        let worktree = add_worktree(&repo, "detached", BranchType::Detached)?;

        // Verify no unpushed commits (detached HEAD has no branch)
        assert_eq!(worktree.has_unpushed_commits()?, false);

        Ok(())
    }
}
