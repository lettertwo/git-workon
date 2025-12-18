use assert_cmd::Command;
use assert_fs::TempDir;
use git2::Repository;
use git_workon_fixture::prelude::*;

#[test]
fn init_default() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&temp).arg("init").assert().success();

    // Verify file system structure
    temp.child(".bare/index").assert(predicate::path::missing());
    temp.child(".git").assert(predicate::path::is_file());
    temp.child(".git")
        .assert(predicate::str::contains("gitdir: ./.bare"));
    temp.child("main").assert(predicate::path::is_dir());

    // Open the repository and verify git state
    let repo = Repository::open(temp.path().join(".bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("main"));

    temp.close()?;
    Ok(())
}

#[test]
fn init_with_name() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&temp)
        .arg("init")
        .arg("test")
        .assert()
        .success();

    // Verify nothing was created in the root temp directory
    temp.child(".bare").assert(predicate::path::missing());
    temp.child(".git").assert(predicate::path::missing());
    temp.child("main").assert(predicate::path::missing());

    // Verify file system structure under the custom directory name
    temp.child("test/.bare/index")
        .assert(predicate::path::missing());
    temp.child("test/.git").assert(predicate::path::is_file());
    temp.child("test/.git")
        .assert(predicate::str::contains("gitdir: ./.bare"));
    temp.child("test/main").assert(predicate::path::is_dir());

    // Open the repository and verify git state
    let repo = Repository::open(temp.path().join("test/.bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("main"));

    temp.close()?;
    Ok(())
}
