use assert_cmd::{prelude::*, Command};
use assert_fs::{prelude::*, TempDir};
use predicates::prelude::*;

#[test]
fn init() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let mut cmd = Command::cargo_bin("git-workon")?;
    cmd.current_dir(&temp).arg("init").assert().success();

    temp.child(".git").assert(predicate::path::is_dir());
    temp.close()?;
    Ok(())
}
