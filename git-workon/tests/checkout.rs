use std::path::PathBuf;
use std::time::Duration;

use assert_cmd::cargo_bin_cmd;
use expectrl::{
    session::{OsProcess, OsStream},
    Expect, Session,
};
use git_workon_fixture::prelude::*;

// ── helpers ───────────────────────────────────────────────────────────────────

const YES_ENTER: &[u8] = b"y\r";

fn cargo_bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_git-workon"))
}

fn spawn_interactive(
    cwd: impl AsRef<std::path::Path>,
    args: &[&str],
) -> Session<OsProcess, OsStream> {
    let cwd = cwd.as_ref();
    let mut cmd = std::process::Command::new(cargo_bin_path());
    cmd.current_dir(cwd);
    for arg in args {
        cmd.arg(arg);
    }
    let mut session = expectrl::Session::spawn(cmd).expect("Failed to spawn in PTY");
    session.set_expect_timeout(Some(Duration::from_secs(10)));
    session
}

/// Commit a single file directly to `branch` in the bare repository.
///
/// Used to set up branches that don't have their own worktree.
fn add_file_to_branch(
    bare_path: impl AsRef<std::path::Path>,
    branch: &str,
    filename: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = git2::Repository::open_bare(bare_path)?;
    let parent_commit = repo
        .find_branch(branch, git2::BranchType::Local)?
        .get()
        .peel_to_commit()?;
    let blob = repo.blob(content.as_bytes())?;
    let mut builder = repo.treebuilder(Some(&parent_commit.tree()?))?;
    builder.insert(filename, blob, 0o100644)?;
    let tree = repo.find_tree(builder.write()?)?;
    let sig = git2::Signature::now("test", "test@test.com")?;
    repo.commit(
        Some(&format!("refs/heads/{}", branch)),
        &sig,
        &sig,
        &format!("add {}", filename),
        &tree,
        &[&parent_commit],
    )?;
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Clean checkout: a local untracked file that doesn't conflict with the target
/// branch is carried along silently. No stash is created.
#[test]
fn checkout_clean_nonconflicting_file_carried_along() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("stack-home")
        .branch("feat-t")
        .build()?;

    // feat-t gets a file; stack-home's working tree has a different local file
    let bare_path = fixture.root()?.join(".bare");
    add_file_to_branch(&*bare_path, "feat-t", "feat-only.txt", "feat")?;

    let stack_home_path = fixture.root()?.join("stack-home");
    std::fs::write(stack_home_path.join("local.txt"), "local work")?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["checkout", "feat-t", "stack-home"])
        .assert()
        .success();

    // Untracked file was carried along
    assert!(
        stack_home_path.join("local.txt").exists(),
        "local.txt should survive a clean carry-along checkout"
    );

    // No stash was created (nothing to shelve)
    let wt_repo = git2::Repository::open(&*stack_home_path)?;
    wt_repo.assert(predicate::repo::has_no_stash());

    Ok(())
}

/// Conflict + `--no-interactive`: exits with a non-zero code and reports a
/// conflict error without prompting.
#[test]
fn checkout_conflict_no_interactive_fails() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("stack-home")
        .branch("feat-t")
        .build()?;

    // feat-t adds conflict.txt; stack-home's working tree has it too (different content)
    let bare_path = fixture.root()?.join(".bare");
    add_file_to_branch(&*bare_path, "feat-t", "conflict.txt", "feat-content")?;

    let stack_home_path = fixture.root()?.join("stack-home");
    std::fs::write(stack_home_path.join("conflict.txt"), "local-edit")?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["checkout", "feat-t", "stack-home", "--no-interactive"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflict"));

    Ok(())
}

/// Conflict + `--json`: exits non-zero and emits a machine-readable error object,
/// never prompting. Verifies that `--json` propagates `no_interactive` to the
/// hidden checkout command so scripted callers get structured output.
#[test]
fn checkout_conflict_json_emits_structured_error() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("stack-home")
        .branch("feat-t")
        .build()?;

    let bare_path = fixture.root()?.join(".bare");
    add_file_to_branch(&*bare_path, "feat-t", "conflict.txt", "feat-content")?;

    let stack_home_path = fixture.root()?.join("stack-home");
    std::fs::write(stack_home_path.join("conflict.txt"), "local-edit")?;

    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["checkout", "feat-t", "stack-home", "--json"])
        .output()?;

    assert!(!output.status.success(), "should fail on conflict");

    let stdout = String::from_utf8(output.stdout)?;
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json output should be valid JSON");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("conflict"),
        "error.message should mention conflict, got: {json}"
    );

    Ok(())
}

/// Interactive "leave": user confirms shelve at the conflict prompt.
/// A labeled stash is created; the working tree ends up at the target branch's content.
#[test]
fn checkout_conflict_interactive_leave_creates_stash() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("stack-home")
        .branch("feat-t")
        .build()?;

    let bare_path = fixture.root()?.join(".bare");
    add_file_to_branch(&*bare_path, "feat-t", "conflict.txt", "feat-content")?;

    let stack_home_path = fixture.root()?.join("stack-home");
    std::fs::write(stack_home_path.join("conflict.txt"), "local-edit")?;

    // Spawn interactive checkout; press 'y' at the "Leave changes behind?" prompt
    let mut session = spawn_interactive(&fixture, &["checkout", "feat-t", "stack-home"]);
    session.expect("Leave changes behind")?;
    session.send(YES_ENTER)?;
    session.expect(expectrl::Eof)?;

    // A labeled stash was created for the shelved changes
    let wt_repo = git2::Repository::open(&*stack_home_path)?;
    wt_repo.assert(predicate::repo::has_stash(
        "workon-autostash: stack-home @ stack-home",
    ));

    // Working tree is now at feat-t's version
    let content = std::fs::read_to_string(stack_home_path.join("conflict.txt"))?;
    assert_eq!(
        content, "feat-content",
        "working tree should show feat-t content after shelve"
    );

    Ok(())
}

/// Restore-on-return: checking out a branch that has a labeled stash restores it.
/// The stash entry is dropped after the clean apply so it is not re-applied on
/// a later visit.
#[test]
fn checkout_restore_on_return() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("stack-home")
        .build()?;

    // Give stack-home a committed tracked file
    fixture
        .commit("stack-home")
        .file("content.txt", "v1")
        .create("add content")?;

    let stack_home_path = fixture.root()?.join("stack-home");

    // Dirty the file, then stash it with the workon restore label
    std::fs::write(stack_home_path.join("content.txt"), "local-edit")?;
    {
        let mut wt_repo = git2::Repository::open(&*stack_home_path)?;
        let sig = git2::Signature::now("test", "test@test.com")?;
        wt_repo.stash_save2(
            &sig,
            Some("workon-autostash: stack-home @ stack-home"),
            Some(git2::StashFlags::INCLUDE_UNTRACKED),
        )?;
    }
    // Working tree is now clean (stash saved the "local-edit")
    assert_eq!(
        std::fs::read_to_string(stack_home_path.join("content.txt"))?,
        "v1",
        "stash_save should reset content.txt to the committed state"
    );

    // Check out stack-home in stack-home: same branch, but restore-on-return runs
    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["checkout", "stack-home", "stack-home"])
        .assert()
        .success();

    // Stash was applied: content.txt is back to "local-edit"
    let restored = std::fs::read_to_string(stack_home_path.join("content.txt"))?;
    assert_eq!(
        restored, "local-edit",
        "restore-on-return should apply the labeled stash"
    );

    // The entry was dropped — a second visit must not re-apply it.
    let wt_repo = git2::Repository::open(&*stack_home_path)?;
    wt_repo.assert(predicate::repo::has_no_stash());

    Ok(())
}
