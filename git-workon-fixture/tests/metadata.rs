#[cfg(test)]
mod metadata {
    use git2::BranchType;
    use git_workon_fixture::prelude::*;

    #[test]
    fn refs_mode_is_the_default_and_has_branch_metadata_still_passes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .branch_metadata("feature", "main")
            .build()?;
        let repo = fixture.repo()?;

        repo.assert(predicate::repo::has_branch_metadata("feature", "main"));

        Ok(())
    }

    #[test]
    fn sqlite_db_written_at_common_dir() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .bare(true)
            .worktree("main")
            .metadata_format(MetadataFormat::Sqlite)
            .branch_metadata("feature", "main")
            .build()?;

        let repo = fixture.repo()?;
        let db_path = repo.commondir().join(".graphite_metadata.db");
        assert!(
            db_path.exists(),
            "sqlite db should exist at the common dir, even when opened from a worktree"
        );
        repo.assert(predicate::repo::has_sqlite_branch_metadata(
            "feature", "main",
        ));

        Ok(())
    }

    #[test]
    fn sqlite_has_a_row_per_entry_plus_trunk_rows() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .graphite_config(&["main"])
            .metadata_format(MetadataFormat::Sqlite)
            .branch_metadata("a", "main")
            .branch_metadata("b", "a")
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::has_sqlite_branch_metadata("a", "main"));
        repo.assert(predicate::repo::has_sqlite_branch_metadata("b", "a"));
        // Trunk row: parent_branch_name is an empty string, not NULL.
        repo.assert(predicate::repo::has_sqlite_branch_metadata("main", ""));

        Ok(())
    }

    #[test]
    fn revision_resolves_to_live_tip_sqlite() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .metadata_format(MetadataFormat::Sqlite)
            .branch("feature")
            .branch_metadata("feature", "main")
            .build()?;

        let repo = fixture.repo()?;
        let main_tip = repo
            .find_branch("main", BranchType::Local)?
            .get()
            .target()
            .unwrap();

        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Sqlite,
            "feature",
            main_tip.to_string(),
        ));

        Ok(())
    }

    #[test]
    fn revision_resolves_to_live_tip_refs() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .branch("feature")
            .branch_metadata("feature", "main")
            .build()?;

        let repo = fixture.repo()?;
        let main_tip = repo
            .find_branch("main", BranchType::Local)?
            .get()
            .target()
            .unwrap();

        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Refs,
            "feature",
            main_tip.to_string(),
        ));

        Ok(())
    }

    #[test]
    fn ghost_entry_has_no_branch_ref_sqlite() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .metadata_format(MetadataFormat::Sqlite)
            .ghost_branch_metadata("deleted", "main")
            .build()?;

        let repo = fixture.repo()?;
        assert!(
            repo.find_branch("deleted", BranchType::Local).is_err(),
            "ghost entries must not create a branch ref"
        );
        repo.assert(predicate::repo::has_sqlite_branch_metadata(
            "deleted", "main",
        ));

        Ok(())
    }

    #[test]
    fn ghost_entry_has_no_branch_ref_refs() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .ghost_branch_metadata("deleted", "main")
            .build()?;

        let repo = fixture.repo()?;
        assert!(
            repo.find_branch("deleted", BranchType::Local).is_err(),
            "ghost entries must not create a branch ref"
        );
        repo.assert(predicate::repo::has_branch_metadata("deleted", "main"));

        Ok(())
    }

    #[test]
    fn refs_blob_carries_parent_branch_revision() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .branch("feature")
            .branch_metadata("feature", "main")
            .build()?;

        let repo = fixture.repo()?;
        let reference = repo.find_reference("refs/branch-metadata/feature")?;
        let blob = reference.peel(git2::ObjectType::Blob)?.into_blob().unwrap();
        let json: serde_json::Value = serde_json::from_slice(blob.content())?;

        assert!(
            json.get("parentBranchRevision").is_some(),
            "refs blob should carry parentBranchRevision: {json}"
        );
        assert_eq!(
            json.get("branchName").and_then(|v| v.as_str()),
            Some("feature")
        );
        assert_eq!(
            json.get("parentBranchName").and_then(|v| v.as_str()),
            Some("main")
        );

        Ok(())
    }
}
