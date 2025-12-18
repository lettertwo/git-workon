#[cfg(test)]
mod tests {
    use assert_fs::TempDir;
    use git_workon_fixture::prelude::*;
    use workon::init;

    #[test]
    fn test_init_basic() -> Result<(), Box<dyn std::error::Error>> {
        // Create a new directory and initialize it
        let dir = TempDir::new()?;
        let repo = init(dir.to_path_buf())?;

        repo.assert(predicate::repo::is_bare());
        repo.assert(predicate::repo::has_branch("main"));

        dir.assert(predicate::path::is_dir());
        dir.child(".bare").assert(predicate::path::is_dir());
        dir.child(".bare/config").assert(predicate::path::is_file());

        dir.child(".git").assert(predicate::path::is_file());
        dir.child(".git")
            .assert(predicate::str::contains("gitdir: ./.bare"));

        Ok(())
    }
}
