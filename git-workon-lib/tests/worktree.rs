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

        let repo = fixture.repo()?;

        // Add a worktree with slashes in the branch name
        let worktree = add_worktree(repo, "user/feature-branch", BranchType::Normal)?;

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

        let repo = fixture.repo()?;

        // Add an orphan worktree
        let worktree = add_worktree(repo, "docs", BranchType::Orphan)?;

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

        let repo = fixture.repo()?;

        // Add a detached worktree
        let worktree = add_worktree(repo, "detached", BranchType::Detached)?;

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

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

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

        let repo = fixture.repo()?;

        // Create and commit a file in main worktree
        let main_path = fixture.root()?.join("main");
        std::fs::write(main_path.join("test.txt"), "original content")?;

        let worktree_repo = fixture.repo()?;
        let mut index = worktree_repo.index()?;
        index.add_path(std::path::Path::new("test.txt"))?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = worktree_repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Test", "test@test.com")?;
        let parent = worktree_repo.head()?.peel_to_commit()?;
        worktree_repo.commit(Some("HEAD"), &sig, &sig, "Add test file", &tree, &[&parent])?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

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

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

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

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

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

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Create a remote (pointing to the bare repo itself)
        repo.remote("origin", fixture.cwd()?.to_str().unwrap())?;

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

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

        // Create a remote (pointing to the bare repo itself)
        repo.remote("origin", fixture.cwd()?.to_str().unwrap())?;

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

        let repo = fixture.repo()?;

        // Add a new worktree
        let worktree = add_worktree(repo, "feature", BranchType::Normal)?;

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

        let repo = fixture.repo()?;

        // Add a detached worktree
        let worktree = add_worktree(repo, "detached", BranchType::Detached)?;

        // Verify no unpushed commits (detached HEAD has no branch)
        assert_eq!(worktree.has_unpushed_commits()?, false);

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
        assert_eq!(worktree.is_merged_into("main")?, true);

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
        let worktree_repo = Repository::open(worktree.path())?;
        std::fs::write(worktree.path().join("test.txt"), "test")?;
        let mut index = worktree_repo.index()?;
        index.add_path(std::path::Path::new("test.txt"))?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = worktree_repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Test", "test@test.com")?;
        let parent = worktree_repo.head()?.peel_to_commit()?;
        worktree_repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "Feature commit",
            &tree,
            &[&parent],
        )?;

        // Verify feature is NOT merged into main (has additional commits)
        assert_eq!(worktree.is_merged_into("main")?, false);

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
        let feature_repo = Repository::open(feature_wt.path())?;
        std::fs::write(feature_wt.path().join("feature.txt"), "feature")?;
        let mut index = feature_repo.index()?;
        index.add_path(std::path::Path::new("feature.txt"))?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = feature_repo.find_tree(tree_id)?;
        let sig = git2::Signature::now("Test", "test@test.com")?;
        let parent = feature_repo.head()?.peel_to_commit()?;
        let feature_commit_oid = feature_repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "Feature commit",
            &tree,
            &[&parent],
        )?;

        // Fast-forward main to include the feature commit
        let feature_commit = repo.find_commit(feature_commit_oid)?;
        repo.find_branch("main", git2::BranchType::Local)?
            .get_mut()
            .set_target(feature_commit.id(), "Fast-forward to feature")?;

        // Verify feature is now merged into main (same commit)
        assert_eq!(feature_wt.is_merged_into("main")?, true);

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
        assert_eq!(worktree.is_merged_into("nonexistent")?, false);

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
        assert_eq!(worktree.is_merged_into("main")?, false);

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
        assert_eq!(worktree.is_merged_into("main")?, false);

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
        use std::process::Command;

        // Create a bare "origin" repository
        let origin_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let origin_path = origin_fixture.cwd()?;

        // Create a local bare repo and add origin as remote
        let local_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let local_path = local_fixture.cwd()?;

        // Add origin remote
        Command::new("git")
            .args([
                "-C",
                local_path.to_str().unwrap(),
                "remote",
                "add",
                "origin",
                origin_path.to_str().unwrap(),
            ])
            .output()?;

        // Fetch from origin
        Command::new("git")
            .args(["-C", local_path.to_str().unwrap(), "fetch", "origin"])
            .output()?;

        // Set upstream for main branch
        Command::new("git")
            .args([
                "-C",
                local_path.to_str().unwrap(),
                "branch",
                "--set-upstream-to=origin/main",
                "main",
            ])
            .output()?;

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
        use std::process::Command;

        // Create a bare "origin" repository
        let origin_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let origin_path = origin_fixture.cwd()?;

        // Create a local bare repo and add origin as remote
        let local_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let local_path = local_fixture.cwd()?;

        // Add origin remote
        Command::new("git")
            .args([
                "-C",
                local_path.to_str().unwrap(),
                "remote",
                "add",
                "origin",
                origin_path.to_str().unwrap(),
            ])
            .output()?;

        // Fetch from origin
        Command::new("git")
            .args(["-C", local_path.to_str().unwrap(), "fetch", "origin"])
            .output()?;

        // Set upstream for main branch
        Command::new("git")
            .args([
                "-C",
                local_path.to_str().unwrap(),
                "branch",
                "--set-upstream-to=origin/main",
                "main",
            ])
            .output()?;

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
        use std::process::Command;

        // Create a bare "origin" repository
        let origin_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let origin_path = origin_fixture.cwd()?;

        // Create a local bare repo and add origin as remote
        let local_fixture = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;

        let local_path = local_fixture.cwd()?;

        // Add origin remote
        Command::new("git")
            .args([
                "-C",
                local_path.to_str().unwrap(),
                "remote",
                "add",
                "origin",
                origin_path.to_str().unwrap(),
            ])
            .output()?;

        // Fetch from origin
        Command::new("git")
            .args(["-C", local_path.to_str().unwrap(), "fetch", "origin"])
            .output()?;

        // Set upstream for main branch
        Command::new("git")
            .args([
                "-C",
                local_path.to_str().unwrap(),
                "branch",
                "--set-upstream-to=origin/main",
                "main",
            ])
            .output()?;

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
