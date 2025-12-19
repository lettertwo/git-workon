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
}
