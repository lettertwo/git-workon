#[cfg(test)]
mod tests {
    use assert_fs::TempDir;
    use git_workon_fixture::prelude::*;
    use workon::clone;

    #[test]
    fn test_clone_basic() -> Result<(), Box<dyn std::error::Error>> {
        // Create a bare remote fixture with a default branch "main"
        let remote = FixtureBuilder::new().bare(true).build()?;

        // Clone into a new dir using the clone logic
        let dir = TempDir::new()?;
        let repo = clone(
            dir.to_path_buf(),
            remote.path.as_ref().unwrap().to_str().unwrap(),
        )?;

        repo.assert(predicate::repo::is_bare());
        repo.assert(predicate::repo::has_branch("main"));
        repo.assert(predicate::repo::has_remote("origin"));
        repo.assert(predicate::repo::has_remote_branch("origin/main"));

        repo.assert(predicate::repo::has_config(
            "remote.origin.fetch",
            Some("+refs/heads/*:refs/remotes/origin/*"),
        ));

        dir.assert(predicate::path::is_dir());
        dir.child(".bare").assert(predicate::path::is_dir());
        dir.child(".bare/config").assert(predicate::path::is_file());

        dir.child(".git").assert(predicate::path::is_file());
        dir.child(".git")
            .assert(predicate::str::contains("gitdir: ./.bare"));

        Ok(())
    }
}
