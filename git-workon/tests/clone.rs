use assert_cmd::Command;
use assert_fs::TempDir;
use git2::Repository;
use git_workon_fixture::prelude::*;

#[test]
fn clone_default() -> Result<(), Box<dyn std::error::Error>> {
    // Create a bare repository to act as the remote
    let remote = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a temp directory for the clone destination
    let clone_dest = TempDir::new()?;

    // Clone the remote repository
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&clone_dest)
        .arg("clone")
        .arg(remote.cwd()?.to_str().unwrap())
        .assert()
        .success();

    // Verify file system structure
    clone_dest
        .child(".bare/index")
        .assert(predicate::path::missing());
    clone_dest.child(".git").assert(predicate::path::is_file());
    clone_dest
        .child(".git")
        .assert(predicate::str::contains("gitdir: ./.bare"));
    clone_dest.child("main").assert(predicate::path::is_dir());

    // Open the repository and verify git state
    let repo = Repository::open(clone_dest.path().join(".bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("main"));
    repo.assert(predicate::repo::has_remote("origin"));
    repo.assert(predicate::repo::has_remote_branch("origin/main"));
    repo.assert(predicate::repo::has_config(
        "remote.origin.fetch",
        Some("+refs/heads/*:refs/remotes/origin/*"),
    ));

    clone_dest.close()?;
    Ok(())
}

#[test]
fn clone_with_name() -> Result<(), Box<dyn std::error::Error>> {
    // Create a bare repository to act as the remote
    let remote = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a temp directory for the clone destination
    let temp = TempDir::new()?;

    // Clone the remote repository with a custom directory name
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&temp)
        .arg("clone")
        .arg(remote.cwd()?.to_str().unwrap())
        .arg("myrepo")
        .assert()
        .success();

    // Verify nothing was created in the root temp directory
    temp.child(".bare").assert(predicate::path::missing());
    temp.child(".git").assert(predicate::path::missing());
    temp.child("main").assert(predicate::path::missing());

    // Verify file system structure under the custom directory name
    temp.child("myrepo/.bare/index")
        .assert(predicate::path::missing());
    temp.child("myrepo/.git").assert(predicate::path::is_file());
    temp.child("myrepo/.git")
        .assert(predicate::str::contains("gitdir: ./.bare"));
    temp.child("myrepo/main").assert(predicate::path::is_dir());

    // Open the repository and verify git state
    let repo = Repository::open(temp.path().join("myrepo/.bare"))?;
    repo.assert(predicate::repo::is_bare());
    repo.assert(predicate::repo::has_branch("main"));
    repo.assert(predicate::repo::has_remote("origin"));
    repo.assert(predicate::repo::has_remote_branch("origin/main"));
    repo.assert(predicate::repo::has_config(
        "remote.origin.fetch",
        Some("+refs/heads/*:refs/remotes/origin/*"),
    ));

    temp.close()?;
    Ok(())
}
