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

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature-branch", BranchType::Normal)?;

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

        let repo = fixture.repo()?;

        // Add a worktree with slashes in the name
        let worktree = add_worktree(repo, "user/feature-branch", BranchType::Normal)?;

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

        let repo = fixture.repo()?;

        // Add an orphan worktree
        let worktree = add_worktree(repo, "docs", BranchType::Orphan)?;

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

        let repo = fixture.repo()?;

        // Add a detached worktree
        let worktree = add_worktree(repo, "detached", BranchType::Detached)?;

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

        let repo = fixture.repo()?;

        // Add a normal worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify branch() returns the correct branch name
        assert_eq!(worktree.branch()?, Some("feature".to_string()));

        // Verify is_detached() returns false
        assert!(!(worktree.is_detached()?));

        Ok(())
    }

    #[test]
    fn test_worktree_branch_with_slashes() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a worktree with slashes in the branch name
        let worktree = add_worktree(repo, "user/feature-branch", BranchType::Normal)?;

        // Verify branch() returns the full branch name with slashes
        assert_eq!(worktree.branch()?, Some("user/feature-branch".to_string()));

        // Verify is_detached() returns false
        assert!(!(worktree.is_detached()?));

        Ok(())
    }

    #[test]
    fn test_worktree_branch_orphan() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add an orphan worktree
        let worktree = add_worktree(repo, "docs", BranchType::Orphan)?;

        // Verify branch() returns the correct branch name
        assert_eq!(worktree.branch()?, Some("docs".to_string()));

        // Verify is_detached() returns false (orphan is still on a branch)
        assert!(!(worktree.is_detached()?));

        Ok(())
    }

    #[test]
    fn test_worktree_branch_detached() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a detached worktree
        let worktree = add_worktree(repo, "detached", BranchType::Detached)?;

        // Verify branch() returns None for detached HEAD
        assert_eq!(worktree.branch()?, None);

        // Verify is_detached() returns true
        assert!(worktree.is_detached()?);

        Ok(())
    }

    #[test]
    fn test_is_dirty_clean_worktree() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify the worktree is clean
        assert!(!(worktree.is_dirty()?));

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

        let repo = fixture.repo()?;

        // Create and commit a file in main worktree
        fixture
            .commit("main")
            .file("test.txt", "original content")
            .create("Add test file")?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify the worktree is clean
        assert!(!(worktree.is_dirty()?));

        // Modify the file
        std::fs::write(worktree.path().join("test.txt"), "modified content")?;

        // Verify the worktree is now dirty
        assert!(worktree.is_dirty()?);

        Ok(())
    }

    #[test]
    fn test_is_dirty_with_untracked_file() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify the worktree is clean
        assert!(!(worktree.is_dirty()?));

        // Add an untracked file
        std::fs::write(worktree.path().join("untracked.txt"), "new file")?;

        // Verify the worktree is now dirty
        assert!(worktree.is_dirty()?);

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_no_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify no unpushed commits (no upstream configured)
        assert!(!(worktree.has_unpushed_commits()?));

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_with_upstream_up_to_date() -> Result<(), Box<dyn std::error::Error>>
    {
        // Create a bare fixture with a default branch and upstream configured
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .remote("origin", "https://example.com/repo.git")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Set up upstream tracking
        let feature_branch = repo.find_branch("feature", git2::BranchType::Local)?;
        let commit = feature_branch.get().peel_to_commit()?;

        fixture.create_remote_ref("origin/feature", commit.id())?;
        fixture.set_upstream("feature", "origin/feature")?;

        // Verify no unpushed commits (up to date with upstream)
        assert!(!(worktree.has_unpushed_commits()?));

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_with_local_commits() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch and upstream configured
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .remote("origin", "https://example.com/repo.git")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Set up upstream tracking
        let feature_branch = repo.find_branch("feature", git2::BranchType::Local)?;
        let commit = feature_branch.get().peel_to_commit()?;

        fixture.create_remote_ref("origin/feature", commit.id())?;
        fixture.set_upstream("feature", "origin/feature")?;

        // Create a new commit in the worktree (will be ahead of upstream)
        fixture
            .commit("feature")
            .file("test.txt", "test")
            .create("New commit")?;

        // Verify has unpushed commits
        assert!(worktree.has_unpushed_commits()?);

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_upstream_gone() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Set up upstream tracking with config (but no actual remote ref)
        let mut config = repo.config()?;
        config.set_str("branch.feature.remote", "origin")?;
        config.set_str("branch.feature.merge", "refs/heads/feature")?;

        // Verify has unpushed commits (upstream is gone, conservative)
        assert!(worktree.has_unpushed_commits()?);

        Ok(())
    }

    #[test]
    fn test_has_unpushed_commits_detached_head() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a detached worktree
        let worktree = add_worktree(repo, "detached", BranchType::Detached)?;

        // Verify no unpushed commits (detached HEAD has no branch)
        assert!(!(worktree.has_unpushed_commits()?));

        Ok(())
    }

    #[test]
    fn test_is_merged_into_same_commit() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a feature worktree at the same commit as main
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify feature is merged into main (same commit)
        assert!(worktree.is_merged_into("main")?);

        Ok(())
    }

    #[test]
    fn test_is_merged_into_with_additional_commits() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a feature worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Add a commit to feature branch
        fixture
            .commit("feature")
            .file("test.txt", "test")
            .create("Feature commit")?;

        // Verify feature is NOT merged into main (has additional commits)
        assert!(!(worktree.is_merged_into("main")?));

        Ok(())
    }

    #[test]
    fn test_is_merged_into_after_fast_forward() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a feature worktree
        let feature_wt = add_worktree(repo, "feature", BranchType::Normal)?;

        // Add a commit to feature branch
        let feature_commit_oid = fixture
            .commit("feature")
            .file("feature.txt", "feature")
            .create("Feature commit")?;

        // Fast-forward main to include the feature commit
        fixture.update_branch("main", feature_commit_oid)?;

        // Verify feature is now merged into main (same commit)
        assert!(feature_wt.is_merged_into("main")?);

        Ok(())
    }

    #[test]
    fn test_is_merged_into_target_not_found() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a feature worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify returns false when target branch doesn't exist
        assert!(!(worktree.is_merged_into("nonexistent")?));

        Ok(())
    }

    #[test]
    fn test_is_merged_into_same_branch() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a main worktree
        let worktree = add_worktree(repo, "main", BranchType::Normal)?;

        // Verify main is not considered merged into itself
        assert!(!(worktree.is_merged_into("main")?));

        Ok(())
    }

    #[test]
    fn test_is_merged_into_detached_head() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a detached worktree
        let worktree = add_worktree(repo, "detached", BranchType::Detached)?;

        // Verify detached HEAD returns false
        assert!(!(worktree.is_merged_into("main")?));

        Ok(())
    }

    #[test]
    fn test_head_commit_normal_branch() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify head_commit returns a SHA
        let commit_sha = worktree.head_commit()?;
        assert!(commit_sha.is_some());

        // Verify it's a valid 40-character hex string
        let sha = commit_sha.unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        Ok(())
    }

    #[test]
    fn test_head_commit_detached_head() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a detached worktree
        let worktree = add_worktree(repo, "detached", BranchType::Detached)?;

        // Verify head_commit returns a SHA even for detached HEAD
        let commit_sha = worktree.head_commit()?;
        assert!(commit_sha.is_some());

        // Verify it's a valid 40-character hex string
        let sha = commit_sha.unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        Ok(())
    }

    #[test]
    fn test_head_commit_orphan_branch() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add an orphan worktree
        let worktree = add_worktree(repo, "docs", BranchType::Orphan)?;

        // Verify head_commit returns a SHA for orphan branch
        let commit_sha = worktree.head_commit()?;
        assert!(commit_sha.is_some());

        // Verify it's a valid 40-character hex string
        let sha = commit_sha.unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        Ok(())
    }

    #[test]
    fn test_remote_with_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare "origin" repository
        let origin_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        // Create a local bare repo with origin remote and upsream configured
        let local_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .remote("origin", &origin_fixture)
            .upstream("main", "origin/main")
            .build()?;

        let repo = local_fixture.repo()?;

        // Add a worktree for the main branch
        let worktree = add_worktree(repo, "main", BranchType::Normal)?;

        // Verify remote returns "origin"
        assert_eq!(worktree.remote()?, Some("origin".to_string()));

        Ok(())
    }

    #[test]
    fn test_remote_without_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree without upstream
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify remote returns None
        assert_eq!(worktree.remote()?, None);

        Ok(())
    }

    #[test]
    fn test_remote_detached_head() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a detached worktree
        let worktree = add_worktree(repo, "detached", BranchType::Detached)?;

        // Verify remote returns None for detached HEAD
        assert_eq!(worktree.remote()?, None);

        Ok(())
    }

    #[test]
    fn test_remote_branch_with_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare "origin" repository
        let origin_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        // Create a local bare repo with origin remote and upstream configured
        let local_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .remote("origin", &origin_fixture)
            .upstream("main", "origin/main")
            .build()?;

        let repo = local_fixture.repo()?;

        // Add a worktree for the main branch
        let worktree = add_worktree(repo, "main", BranchType::Normal)?;

        // Verify remote_branch returns the upstream branch name
        // Note: git may return shorthand "origin/main" or full "refs/remotes/origin/main"
        let remote_branch = worktree.remote_branch()?;
        assert!(remote_branch.is_some());
        let branch = remote_branch.unwrap();
        assert!(
            branch == "origin/main" || branch == "refs/remotes/origin/main",
            "Expected 'origin/main' or 'refs/remotes/origin/main', got '{}'",
            branch
        );

        Ok(())
    }

    #[test]
    fn test_remote_branch_without_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree without upstream
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify remote_branch returns None
        assert_eq!(worktree.remote_branch()?, None);

        Ok(())
    }

    #[test]
    fn test_remote_url_with_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare "origin" repository
        let origin_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let origin_path = origin_fixture.cwd()?;

        // Create a local bare repo with origin remote and upstream configured
        let local_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .remote("origin", &origin_fixture)
            .upstream("main", "origin/main")
            .build()?;

        let repo = local_fixture.repo()?;

        // Add a worktree for the main branch
        let worktree = add_worktree(repo, "main", BranchType::Normal)?;

        // Verify remote_url returns the origin path
        let remote_url = worktree.remote_url()?;
        assert!(remote_url.is_some());
        assert_eq!(remote_url.unwrap(), origin_path.to_str().unwrap());

        Ok(())
    }

    #[test]
    fn test_remote_url_without_upstream() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare fixture with a default branch
        let fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let repo = fixture.repo()?;

        // Add a new worktree without upstream
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Verify remote_url returns None
        assert_eq!(worktree.remote_url()?, None);

        Ok(())
    }
}
