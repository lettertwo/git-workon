use assert_cmd::Command;
use assert_fs::{prelude::*, TempDir};
use predicates::prelude::*;

// 1. git clone --bare --single-branch <atlassian-url>.git .bare
// 2. $ echo "gitdir: ./.bare" > .git
// 3. $ git config remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"
// 4. $ git fetch
// 5. $ git worktree add --track main origin/main
#[test]
fn clone_default() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: Create some fixtures, e.g., bare repo to clone from.
    let temp = TempDir::new()?;
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&temp).arg("clone").assert().success();

    // temp.child(".bare/index").assert(predicate::path::missing());
    // temp.child(".bare/config").assert(predicate::str::contains("bare = true"));
    // temp.child(".git").assert(predicate::path::is_file());
    // temp.child(".git").assert(predicate::str::contains("gitdir: ./.bare"));
    // temp.child("main").assert(predicate::path::is_dir());

    temp.close()?;
    Ok(())
}

#[test]
fn clone_with_name() -> Result<(), Box<dyn std::error::Error>> {
    // let temp = TempDir::new()?;
    // let mut cmd = Command::cargo_bin("git-workon")?;
    // cmd.current_dir(&temp).arg("init").arg("test").assert().success();
    //
    // temp.child(".bare").assert(predicate::path::missing());
    // temp.child(".git").assert(predicate::path::missing());
    // temp.child("main").assert(predicate::path::missing());
    //
    // temp.child("test/.bare/index").assert(predicate::path::missing());
    // temp.child("test/.bare/config").assert(predicate::str::contains("bare = true"));
    // temp.child("test/.git").assert(predicate::path::is_file());
    // temp.child("test/.git").assert(predicate::str::contains("gitdir: ./.bare"));
    // temp.child("test/main").assert(predicate::path::is_dir());
    //
    // temp.close()?;
    Ok(())
}
