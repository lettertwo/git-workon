#[cfg(test)]
mod fixture_builder {
    use assert_fs::prelude::*;
    use predicates::prelude::*;

    use git_workon_fixture::FixtureBuilder;

    #[test]
    fn default() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().build()?;

        fixture.with(|repo, path| {
            path.assert(predicate::path::is_dir());
            path.child(".git").assert(predicate::path::is_dir());
            path.child(".git/config")
                .assert(predicate::str::contains("bare = false"));

            // TODO: assert that the main branch exists, has no parents, and has the initial commit message.
            // The reason the below code won't work is because it's sort of using
            // the fixture assertions, but we want to test that the fixtures themselves
            // are correct before testing the assertions.

            repo.head().assert(predicate::str::contains("main"));

            // repo.branch("main").assert(repo_predicate::exists());

            // repo.head().assert(
            //     repo_predicate::is_branch("main")
            //         .and(
            //             repo_predicate::commit_message_contains("Initial commit")
            //                 .contains("Initial commit"),
            //         )
            //         .and(repo_predicate::commit_parent_count(0)),
            // );

            // let head = repo.head().unwrap();
            // assert!(has_main_branch.eval(repo));
            //
            // let commit = head.peel_to_commit().unwrap();
            // assert_eq!(commit.parent_count(), 0);
            // assert_eq!(commit.message(), Some("Initial commit"));
        });

        Ok(())
    }

    #[test]
    fn bare() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = FixtureBuilder::new().bare(true).build()?;

        fixture.with(|_, path| {
            path.assert(predicate::path::is_dir());
            path.child(".git").assert(predicate::path::is_dir());
            path.child(".git/config")
                .assert(predicate::str::contains("bare = true"));
        });

        Ok(())
    }
}
