#[cfg(test)]
mod index_state {
    use git2::BranchType;
    use git_workon_fixture::prelude::*;

    #[test]
    fn staged_file_alone() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .staged_file("staged.txt", "staged content")
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::has_staged_file("staged.txt"));

        Ok(())
    }

    #[test]
    fn unstaged_file_alone() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .unstaged_file("tracked.txt", "committed content", "modified content")
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::has_unstaged_file("tracked.txt"));

        let dir = fixture.cwd()?;
        dir.child("tracked.txt")
            .assert(predicate::str::contains("modified content"));

        Ok(())
    }

    #[test]
    fn untracked_file_alone() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .untracked_file("new.txt", "new content")
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::has_untracked_file("new.txt"));

        Ok(())
    }

    #[test]
    fn untracked_file_in_subdirectory() -> Result<(), Box<dyn std::error::Error>> {
        // The builder creates parent directories; the predicate must recurse into
        // untracked directories to see the file (not just the "sub/" dir entry).
        let fixture = FixtureBuilder::new()
            .untracked_file("sub/new.txt", "new content")
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::has_untracked_file("sub/new.txt"));

        Ok(())
    }

    #[test]
    fn all_three_combined() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .staged_file("staged.txt", "staged content")
            .unstaged_file("tracked.txt", "committed content", "modified content")
            .untracked_file("new.txt", "new content")
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::has_staged_file("staged.txt"));
        repo.assert(predicate::repo::has_unstaged_file("tracked.txt"));
        repo.assert(predicate::repo::has_untracked_file("new.txt"));

        Ok(())
    }

    #[test]
    fn works_with_worktree() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .worktree("feature")
            .staged_file("staged.txt", "staged content")
            .unstaged_file("tracked.txt", "committed content", "modified content")
            .untracked_file("new.txt", "new content")
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::is_worktree());
        repo.assert(predicate::repo::has_staged_file("staged.txt"));
        repo.assert(predicate::repo::has_unstaged_file("tracked.txt"));
        repo.assert(predicate::repo::has_untracked_file("new.txt"));

        Ok(())
    }

    #[test]
    fn bare_with_no_worktree_errors() {
        let result = FixtureBuilder::new()
            .bare(true)
            .staged_file("staged.txt", "staged content")
            .build();

        assert!(
            result.is_err(),
            "bare fixture with no worktree has no working tree to stage into"
        );
    }

    #[test]
    fn bare_with_no_worktree_and_no_index_state_still_works(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Sanity check: the bare-no-worktree error only fires when index-state builders are
        // actually used — existing bare-repo fixtures without them must keep working.
        let fixture = FixtureBuilder::new().bare(true).build()?;
        let repo = fixture.repo()?;
        repo.assert(predicate::repo::is_bare());

        Ok(())
    }

    /// Sequencing: `unstaged_file` baseline commits must land BEFORE Graphite-metadata
    /// live-tip resolution, so a metadata entry whose parent branch is the cwd branch records
    /// the tip AFTER the baseline commit, not before it.
    #[test]
    fn unstaged_file_baseline_commit_lands_before_metadata_resolution(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .branch("feature")
            .branch_metadata("feature", "main")
            .unstaged_file("baseline.txt", "committed content", "modified content")
            .build()?;

        let repo = fixture.repo()?;

        // The cwd repo (main, no worktree given) is where the baseline commit landed.
        let main_tip = repo
            .find_branch("main", BranchType::Local)?
            .get()
            .target()
            .unwrap();
        let head_commit = repo.head()?.peel_to_commit()?;
        assert_eq!(
            head_commit.parent_count(),
            1,
            "the baseline commit should have landed on main, giving it a parent"
        );
        assert_eq!(main_tip, head_commit.id());

        // "feature"'s recorded parentBranchRevision must be the POST-baseline tip.
        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Refs,
            "feature",
            main_tip.to_string(),
        ));

        // Working tree still reflects the unstaged rewrite (index mutations ran last).
        repo.assert(predicate::repo::has_unstaged_file("baseline.txt"));

        Ok(())
    }
}
