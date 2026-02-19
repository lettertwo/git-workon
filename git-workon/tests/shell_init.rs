use assert_cmd::Command;
use git_workon_fixture::prelude::*;

#[test]
fn complete_lists_worktree_names() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    // Complete the name arg in the "find" subcommand (index 1 = after "find")
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("_complete")
        .arg("--index")
        .arg("1")
        .arg("--")
        .arg("find")
        .arg("")
        .assert()
        .success()
        .stdout(predicate::str::contains("main"))
        .stdout(predicate::str::contains("feature"));

    Ok(())
}

#[test]
fn shell_init_bash_outputs_function() -> Result<(), Box<dyn std::error::Error>> {
    Command::cargo_bin("git-workon")?
        .arg("shell-init")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("workon()"))
        .stdout(predicate::str::contains("complete -o nospace"));

    Ok(())
}

#[test]
fn shell_init_zsh_outputs_compdef() -> Result<(), Box<dyn std::error::Error>> {
    Command::cargo_bin("git-workon")?
        .arg("shell-init")
        .arg("zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("workon()"))
        .stdout(predicate::str::contains("compdef"));

    Ok(())
}

#[test]
fn shell_init_fish_outputs_function() -> Result<(), Box<dyn std::error::Error>> {
    Command::cargo_bin("git-workon")?
        .arg("shell-init")
        .arg("fish")
        .assert()
        .success()
        .stdout(predicate::str::contains("function workon"))
        .stdout(predicate::str::contains("complete --keep-order"));

    Ok(())
}

#[test]
fn shell_init_custom_cmd() -> Result<(), Box<dyn std::error::Error>> {
    Command::cargo_bin("git-workon")?
        .arg("shell-init")
        .arg("bash")
        .arg("--cmd")
        .arg("gw")
        .assert()
        .success()
        .stdout(predicate::str::contains("gw()"))
        .stdout(predicate::str::contains("complete -o nospace"));

    Ok(())
}
