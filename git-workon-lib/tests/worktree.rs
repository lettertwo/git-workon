#[cfg(test)]
mod tests {
    use git2::Repository;
    use git_workon_fixture::prelude::*;
    use workon::{add_worktree, AddWorktreeOptions};

    #[test]
    fn test_add_worktree_basic() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = Repository::open(fixture.path.as_ref().unwrap())?;

        // Add a new worktree
        let worktree = add_worktree(&repo, "feature-branch", &AddWorktreeOptions::default())?;

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
        let worktree = add_worktree(&repo, "user/feature-branch", &AddWorktreeOptions::default())?;

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
        let options = AddWorktreeOptions {
            orphan: true,
            detach: false,
        };
        let worktree = add_worktree(&repo, "docs", &options)?;

        // Verify the worktree was created
        assert!(worktree.path().exists());
        assert_eq!(worktree.name(), Some("docs"));

        repo.assert(predicate::repo::has_worktree("docs"));

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
        let options = AddWorktreeOptions {
            orphan: false,
            detach: true,
        };
        let worktree = add_worktree(&repo, "detached", &options)?;

        // Verify the worktree was created
        assert!(worktree.path().exists());
        assert_eq!(worktree.name(), Some("detached"));

        repo.assert(predicate::repo::has_worktree("detached"));

        Ok(())
    }
}
