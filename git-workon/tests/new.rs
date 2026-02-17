use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use git_workon_fixture::prelude::*;

#[test]
fn new_creates_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a new worktree for a new branch
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature-branch")
        .assert()
        .success();

    // Verify the new worktree directory exists
    fixture
        .root()?
        .child("feature-branch")
        .assert(predicate::path::is_dir());

    fixture.assert(predicate::repo::is_bare());
    fixture.assert(predicate::repo::has_branch("main"));
    fixture.assert(predicate::repo::has_branch("feature-branch"));

    Ok(())
}

#[test]
fn new_with_slashes_in_name() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a new worktree with slashes in the branch name
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("user/feature-branch")
        .assert()
        .success();

    // Verify the new worktree directory exists with the full path
    fixture
        .root()?
        .child("user/feature-branch")
        .assert(predicate::path::is_dir());

    // Open the repository and verify git state
    fixture.assert(predicate::repo::is_bare());
    fixture.assert(predicate::repo::has_branch("user/feature-branch"));

    Ok(())
}

#[test]
#[ignore]
fn new_orphan_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Add a commit to main so we can verify the orphan has no parent
    fixture
        .commit("main")
        .file("test.txt", "test")
        .create("Test commit")?;

    // Create an orphan worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--orphan")
        .arg("docs")
        .assert()
        .success();

    // Verify the new worktree directory exists
    fixture
        .root()?
        .child("docs")
        .assert(predicate::path::is_dir());

    // Open the repository and verify the main branch exists
    fixture.assert(predicate::repo::is_bare());
    fixture.assert(predicate::repo::has_branch("main"));

    // Open the orphan worktree and verify it's truly orphaned
    // let orphan_repo = Repository::open(fixture.root()?.join("docs"))?;

    // Verify HEAD points to the docs branch
    let head = fixture.head()?;
    assert_eq!(head.name(), Some("refs/heads/docs"));

    // Verify the branch has exactly one commit (the initial empty commit)
    let head_commit = head.peel_to_commit()?;
    assert_eq!(
        head_commit.parent_count(),
        0,
        "Orphan branch should have no parent commits"
    );

    // Verify the index is empty
    let index = fixture.repo()?.index()?;
    assert_eq!(index.len(), 0, "Orphan worktree index should be empty");

    // Verify the working directory doesn't contain files from main
    assert!(
        !fixture.root()?.path().join("docs/test.txt").exists(),
        "Orphan worktree should not have files from parent branch"
    );

    Ok(())
}

#[test]
fn new_detached_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a detached worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--detach")
        .arg("detached")
        .assert()
        .success();

    // Verify the new worktree directory exists
    fixture
        .root()?
        .child("detached")
        .assert(predicate::path::is_dir());

    // Open the repository and verify the main branch exists
    fixture.assert(predicate::repo::is_bare());
    fixture.assert(predicate::repo::has_branch("main"));

    // Verify the detached worktree HEAD is in detached state
    let worktree_path = fixture.root()?.child("detached");
    let worktree_repo = git2::Repository::open(worktree_path.path())?;
    worktree_repo.assert(predicate::repo::is_head_detached());

    Ok(())
}

#[test]
fn new_uses_config_default_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .config("workon.defaultBranch", "develop")
        .build()?;

    // Add a commit to develop to differentiate it from main
    fixture
        .commit("develop")
        .file("develop.txt", "from develop")
        .create("Commit on develop")?;

    // Create a new worktree without specifying base - should use config default
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify the new worktree exists
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    fixture.assert(predicate::repo::has_branch("feature"));

    // Verify feature branch was created from develop by checking commit ancestry
    let repo = fixture.repo()?;
    let feature_branch = repo.find_branch("feature", git2::BranchType::Local)?;
    let feature_commit = feature_branch.get().peel_to_commit()?;

    let develop_branch = repo.find_branch("develop", git2::BranchType::Local)?;
    let develop_commit = develop_branch.get().peel_to_commit()?;

    // Feature's parent should be develop's HEAD
    assert_eq!(
        feature_commit.id(),
        develop_commit.id(),
        "Feature branch should be based on develop"
    );

    Ok(())
}

#[test]
fn new_cli_base_overrides_config() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .worktree("staging")
        .config("workon.defaultBranch", "develop")
        .build()?;

    // Add commits to differentiate branches
    fixture
        .commit("develop")
        .file("develop.txt", "from develop")
        .create("Commit on develop")?;

    fixture
        .commit("staging")
        .file("staging.txt", "from staging")
        .create("Commit on staging")?;

    // Create new worktree with --base flag (should override config)
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--base")
        .arg("staging")
        .arg("feature")
        .assert()
        .success();

    // Verify the new worktree exists
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    fixture.assert(predicate::repo::has_branch("feature"));

    // Verify feature branch was created from staging (not develop)
    let repo = fixture.repo()?;
    let feature_branch = repo.find_branch("feature", git2::BranchType::Local)?;
    let feature_commit = feature_branch.get().peel_to_commit()?;

    let staging_branch = repo.find_branch("staging", git2::BranchType::Local)?;
    let staging_commit = staging_branch.get().peel_to_commit()?;

    // Feature's parent should be staging's HEAD (not develop)
    assert_eq!(
        feature_commit.id(),
        staging_commit.id(),
        "Feature branch should be based on staging (CLI override)"
    );

    Ok(())
}

#[test]
fn new_without_config_uses_default_branch() -> Result<(), Box<dyn std::error::Error>> {
    // Create a bare repo with just the main worktree
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create new worktree without config (should branch from default branch)
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify the new worktree exists
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    fixture.assert(predicate::repo::has_branch("feature"));

    // Verify feature branch was created from main (the default branch)
    let repo = fixture.repo()?;
    let feature_branch = repo.find_branch("feature", git2::BranchType::Local)?;
    let feature_commit = feature_branch.get().peel_to_commit()?;

    let main_branch = repo.find_branch("main", git2::BranchType::Local)?;
    let main_commit = main_branch.get().peel_to_commit()?;

    // Feature should be based on main (the default branch)
    assert_eq!(
        feature_commit.id(),
        main_commit.id(),
        "Feature branch should be based on main when no config"
    );

    Ok(())
}

#[test]
fn new_with_auto_copy_enabled() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.autoCopyUntracked", "true")
        .config("workon.copyPattern", ".env*")
        .config("workon.copyPattern", "node_modules/**/*")
        .build()?;

    let main_worktree = fixture.root()?.join("main");

    // Create some untracked files in main worktree
    fs::write(main_worktree.join(".env.local"), "SECRET=value")?;
    fs::write(main_worktree.join("other.txt"), "not copied")?;
    fs::create_dir_all(main_worktree.join("node_modules/lib"))?;
    fs::write(main_worktree.join("node_modules/lib/index.js"), "module")?;

    // Create new worktree from main (should auto-copy matching files)
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    let feature_worktree = fixture.root()?.join("feature");

    // Verify pattern-matched files were copied
    assert!(
        feature_worktree.join(".env.local").exists(),
        "Should auto-copy .env.local (matches .env*)"
    );
    assert!(
        feature_worktree.join("node_modules/lib/index.js").exists(),
        "Should auto-copy node_modules (matches pattern)"
    );
    assert!(
        !feature_worktree.join("other.txt").exists(),
        "Should not copy other.txt (no matching pattern)"
    );

    Ok(())
}

#[test]
fn new_with_auto_copy_respects_excludes() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.autoCopyUntracked", "true")
        .config("workon.copyPattern", "**/*")
        .config("workon.copyExclude", "*.log")
        .build()?;

    let main_worktree = fixture.root()?.join("main");

    // Create files
    fs::write(main_worktree.join("data.txt"), "data")?;
    fs::write(main_worktree.join("debug.log"), "debug")?;

    // Create new worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    let feature_worktree = fixture.root()?.join("feature");

    // Verify exclusions were respected
    assert!(
        feature_worktree.join("data.txt").exists(),
        "Should copy data.txt"
    );
    assert!(
        !feature_worktree.join("debug.log").exists(),
        "Should not copy debug.log (excluded)"
    );

    Ok(())
}

#[test]
fn new_copy_untracked_flag_overrides_config() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        // Config disabled, but flag should enable it
        .config("workon.autoCopyUntracked", "false")
        .config("workon.copyPattern", "*.txt")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    fs::write(main_worktree.join("test.txt"), "content")?;

    // Create new worktree with --copy-untracked flag
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--copy-untracked")
        .arg("feature")
        .assert()
        .success();

    let feature_worktree = fixture.root()?.join("feature");

    // Verify file was copied despite config being false
    assert!(
        feature_worktree.join("test.txt").exists(),
        "Should copy file when --copy-untracked flag is used"
    );

    Ok(())
}

#[test]
fn new_no_copy_untracked_flag_overrides_config() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        // Config enabled, but flag should disable it
        .config("workon.autoCopyUntracked", "true")
        .config("workon.copyPattern", "*.txt")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    fs::write(main_worktree.join("test.txt"), "content")?;

    // Create new worktree with --no-copy-untracked flag
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--no-copy-untracked")
        .arg("feature")
        .assert()
        .success();

    let feature_worktree = fixture.root()?.join("feature");

    // Verify file was NOT copied despite config being true
    assert!(
        !feature_worktree.join("test.txt").exists(),
        "Should not copy file when --no-copy-untracked flag is used"
    );

    Ok(())
}

#[test]
fn new_auto_copy_skips_when_base_worktree_missing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        // Note: no main worktree created
        .config("workon.autoCopyUntracked", "true")
        .config("workon.copyPattern", "*.txt")
        .build()?;

    // Create new worktree (should succeed even though base worktree doesn't exist)
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify worktree was created successfully
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn new_auto_copy_without_patterns_copies_everything() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.autoCopyUntracked", "true")
        // Note: no copyPattern configured - should default to copying everything
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    fs::write(main_worktree.join("test.txt"), "content")?;
    fs::write(main_worktree.join("readme.md"), "readme")?;
    fs::create_dir_all(main_worktree.join("src"))?;
    fs::write(main_worktree.join("src/main.rs"), "code")?;

    // Create new worktree (should copy all files using default pattern)
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    let feature_worktree = fixture.root()?.join("feature");

    // Verify all files were copied (default pattern **/* used)
    assert!(
        feature_worktree.join("test.txt").exists(),
        "Should copy test.txt with default pattern"
    );
    assert!(
        feature_worktree.join("readme.md").exists(),
        "Should copy readme.md with default pattern"
    );
    assert!(
        feature_worktree.join("src/main.rs").exists(),
        "Should copy src/main.rs with default pattern"
    );

    Ok(())
}

#[test]
fn new_no_name_errors_with_no_interactive() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("new")
        .arg("--no-interactive")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No worktree name provided"));

    Ok(())
}

#[test]
fn new_with_explicit_name_works_non_interactively() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .arg("--no-interactive")
        .assert()
        .success();

    let repo = fixture.repo()?;
    repo.assert(predicate::repo::has_worktree("feature"));

    Ok(())
}

// --- Interactive PTY tests ---

const ENTER: &[u8] = b"\r";
const ARROW_DOWN: &[u8] = b"\x1b[B";

fn cargo_bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_git-workon"))
}

fn spawn_interactive(cwd: &Path, args: &[&str]) -> expectrl::Session {
    let mut cmd = std::process::Command::new(cargo_bin_path());
    cmd.current_dir(cwd);
    for arg in args {
        cmd.arg(arg);
    }
    let mut session = expectrl::Session::spawn(cmd).expect("Failed to spawn in PTY");
    session.set_expect_timeout(Some(Duration::from_secs(10)));
    session
}

/// Extract the last non-empty line from PTY output, stripping ANSI escape sequences.
fn last_line(output: &[u8]) -> String {
    let text = String::from_utf8_lossy(output);
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut result = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.next() == Some('[') {
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() || ch == 'h' || ch == 'l' {
                        break;
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[test]
fn new_interactive_prompts_for_name() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["new"]);

    session.expect("Branch name")?;
    session.send("my-feature\r")?;

    session.expect("Base branch")?;
    session.send(ENTER)?;

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    assert!(
        selected.contains("my-feature"),
        "Expected output to contain 'my-feature', got: {selected}"
    );

    fixture
        .root()?
        .child("my-feature")
        .assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn new_interactive_prompts_for_base_branch() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("develop")
        .build()?;

    // Add a distinct commit on develop so we can verify the base
    fixture
        .commit("develop")
        .file("develop.txt", "from develop")
        .create("Commit on develop")?;

    let mut session = spawn_interactive(fixture.as_ref(), &["new"]);

    session.expect("Branch name")?;
    session.send("my-feature\r")?;

    // Base branch prompt: default is item 0 "<default: main>",
    // then branches listed. Arrow down to skip default, then find "develop".
    session.expect("Base branch")?;
    // Arrow down past default to select a non-default branch.
    // The list is: 0: <default: main>, 1: develop, 2: main
    session.send(ARROW_DOWN)?;
    session.send(ENTER)?;

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    assert!(
        selected.contains("my-feature"),
        "Expected output to contain 'my-feature', got: {selected}"
    );

    // Verify the feature branch was based on develop (same commit)
    let repo = fixture.repo()?;
    let feature_branch = repo.find_branch("my-feature", git2::BranchType::Local)?;
    let feature_commit = feature_branch.get().peel_to_commit()?;

    let develop_branch = repo.find_branch("develop", git2::BranchType::Local)?;
    let develop_commit = develop_branch.get().peel_to_commit()?;

    assert_eq!(
        feature_commit.id(),
        develop_commit.id(),
        "Feature branch should be based on develop"
    );

    Ok(())
}
