use assert_cmd::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn no_args_shows_usage_and_fails() {
    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn help_shows_usage_and_succeeds() {
    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("git-workon-review"));
}
