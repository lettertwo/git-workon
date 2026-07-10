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

    /// Commit a file directly onto `parent`, at the object-database level — no worktree checkout
    /// needed, so it works regardless of which branch is currently checked out.
    fn commit_onto(repo: &git2::Repository, parent_oid: git2::Oid, path: &str) -> git2::Oid {
        let parent = repo.find_commit(parent_oid).unwrap();
        let mut treebuilder = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
        let blob_oid = repo.blob(b"content\n").unwrap();
        treebuilder
            .insert(path, blob_oid, git2::FileMode::Blob.into())
            .unwrap();
        let tree = repo.find_tree(treebuilder.write().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(None, &sig, &sig, "test commit", &tree, &[&parent])
            .unwrap()
    }

    #[test]
    fn update_branch_cascades_parent_revision_to_children_refs(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // `branch_metadata`'s revisions resolve once at `build()` time (see its doc comment) —
        // "b"'s recorded parentBranchRevision is "a"'s tip AT BUILD TIME. Moving "a" afterward
        // must cascade into "b"'s recorded revision, or `resolve_graphite_base` would compute
        // "b"'s base against a commit "a" no longer occupies.
        let fixture = FixtureBuilder::new()
            .branch("a")
            .branch_metadata("a", "main")
            .branch_metadata("b", "a")
            .build()?;
        let repo = fixture.repo()?;
        let a_old_tip = repo
            .find_branch("a", BranchType::Local)?
            .get()
            .target()
            .unwrap();

        let a_new_tip = commit_onto(repo, a_old_tip, "advance.txt");
        fixture.update_branch("a", a_new_tip)?;

        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Refs,
            "b",
            a_new_tip.to_string(),
        ));

        Ok(())
    }

    #[test]
    fn update_branch_cascades_parent_revision_to_children_sqlite(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .metadata_format(MetadataFormat::Sqlite)
            .branch("a")
            .branch_metadata("a", "main")
            .branch_metadata("b", "a")
            .build()?;
        let repo = fixture.repo()?;
        let a_old_tip = repo
            .find_branch("a", BranchType::Local)?
            .get()
            .target()
            .unwrap();

        let a_new_tip = commit_onto(repo, a_old_tip, "advance.txt");
        fixture.update_branch("a", a_new_tip)?;

        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Sqlite,
            "b",
            a_new_tip.to_string(),
        ));

        Ok(())
    }

    #[test]
    fn update_branch_does_not_cascade_a_deliberately_stale_verbatim_revision(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // `branch_metadata_at` pins a verbatim (possibly bogus) revision — a fixture built that
        // way is deliberately testing the stale-revision case itself, so `update_branch` must
        // NOT overwrite it just because the parent branch name matches.
        let fixture = FixtureBuilder::new()
            .branch("a")
            .branch_metadata_at("b", "a", "", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
            .build()?;
        let repo = fixture.repo()?;
        let a_tip = repo
            .find_branch("a", BranchType::Local)?
            .get()
            .target()
            .unwrap();

        let a_new_tip = commit_onto(repo, a_tip, "advance.txt");
        fixture.update_branch("a", a_new_tip)?;

        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Refs,
            "b",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
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

    #[test]
    fn branch_metadata_at_persists_verbatim_strings_sqlite(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new()
            .metadata_format(MetadataFormat::Sqlite)
            .branch_metadata_at(
                "feature",
                "main",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "cafebabecafebabecafebabecafebabecafebabe",
            )
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Sqlite,
            "feature",
            "cafebabecafebabecafebabecafebabecafebabe",
        ));

        Ok(())
    }

    #[test]
    fn branch_metadata_at_persists_verbatim_strings_refs() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = FixtureBuilder::new()
            .branch_metadata_at(
                "feature",
                "main",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "cafebabecafebabecafebabecafebabecafebabe",
            )
            .build()?;

        let repo = fixture.repo()?;
        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Refs,
            "feature",
            "cafebabecafebabecafebabecafebabecafebabe",
        ));

        Ok(())
    }

    #[test]
    fn branch_metadata_at_allows_non_resolving_revision_sqlite(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // A verbatim 40-hex string need not resolve to a real commit; the fixture is not
        // responsible for validating it — that's the lib's job (InvalidParentRevision).
        let fixture = FixtureBuilder::new()
            .metadata_format(MetadataFormat::Sqlite)
            .branch_metadata_at(
                "feature",
                "main",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            )
            .build()?;

        let repo = fixture.repo()?;
        assert!(
            repo.find_commit(git2::Oid::from_str(
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            )?)
            .is_err(),
            "the verbatim revision should not resolve to a real commit"
        );
        repo.assert(predicate::repo::has_metadata_parent_revision(
            MetadataFormat::Sqlite,
            "feature",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ));

        Ok(())
    }
}
