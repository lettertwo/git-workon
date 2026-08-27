use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::cargo_bin_cmd;
use expectrl::{
    session::{OsProcess, OsStream},
    Expect, Session,
};
use git_workon_fixture::prelude::*;

#[test]
fn new_creates_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a new worktree for a new branch
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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

/// Auto-attaching a branch that exists only on a remote must set upstream
/// tracking on the created local branch.
#[test]
fn new_remote_branch_attach_sets_upstream() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .branch("feature")
        .remote("origin", "https://example.com/origin.git")
        .upstream("feature", "origin/feature")
        .build()?;

    // Make "feature" remote-only so `new` takes the attach-from-remote path.
    fixture
        .repo()?
        .find_branch("feature", git2::BranchType::Local)?
        .delete()?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["new", "feature", "--no-interactive"])
        .assert()
        .success();

    fixture.assert(predicate::repo::has_worktree("feature"));
    fixture.assert(predicate::repo::has_branch("feature"));

    // Fresh handle: the cached fixture repo's config snapshot predates the
    // subprocess's upstream write.
    let repo = git2::Repository::open(fixture.repo()?.path())?;
    repo.assert(predicate::repo::has_upstream(
        "feature",
        Some("origin/feature"),
    ));

    Ok(())
}

/// Ambiguous remotes with --no-interactive: the first candidate is used and
/// upstream tracking is still set.
#[test]
fn new_ambiguous_remote_no_interactive_sets_upstream() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .branch("feature")
        .remote("teamA", "https://example.com/teamA.git")
        .remote("teamB", "https://example.com/teamB.git")
        .upstream("feature", "teamA/feature")
        .upstream("feature", "teamB/feature")
        .build()?;

    fixture
        .repo()?
        .find_branch("feature", git2::BranchType::Local)?
        .delete()?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["new", "feature", "--no-interactive"])
        .assert()
        .success();

    fixture.assert(predicate::repo::has_worktree("feature"));

    // Fresh handle: the cached fixture repo's config snapshot predates the
    // subprocess's upstream write.
    let repo = git2::Repository::open(fixture.repo()?.path())?;
    repo.assert(predicate::repo::has_upstream(
        "feature",
        Some("teamA/feature"),
    ));

    Ok(())
}

#[test]
fn new_with_slashes_in_name() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a new worktree with slashes in the branch name
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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
    assert_eq!(head.name(), Ok("refs/heads/docs"));

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
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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

    // Regression (ADR-027): the temporary branch libgit2/we create to check out the
    // detached worktree must not survive as a stray local branch.
    fixture.assert(predicate::repo::has_branch("detached").not());

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
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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
        .config("workon.autoCopy", "true")
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
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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
        .config("workon.autoCopy", "true")
        .config("workon.copyPattern", "**/*")
        .config("workon.copyExclude", "*.log")
        .build()?;

    let main_worktree = fixture.root()?.join("main");

    // Create files
    fs::write(main_worktree.join("data.txt"), "data")?;
    fs::write(main_worktree.join("debug.log"), "debug")?;

    // Create new worktree
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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
fn new_auto_copy_bare_dir_exclude_prunes_subtree() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.autoCopy", "true")
        .config("workon.copyExclude", "target")
        .build()?;

    let main_worktree = fixture.root()?.join("main");

    fs::write(main_worktree.join("data.txt"), "data")?;
    fs::create_dir_all(main_worktree.join("target/debug"))?;
    fs::write(main_worktree.join("target/debug/build.o"), "object")?;

    let mut new_cmd = cargo_bin_cmd!("git-workon");
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    let feature_worktree = fixture.root()?.join("feature");

    assert!(
        feature_worktree.join("data.txt").exists(),
        "Should copy data.txt"
    );
    assert!(
        !feature_worktree.join("target").exists(),
        "Should not copy anything under target/ (bare dir name excludes the subtree)"
    );

    Ok(())
}

#[test]
fn new_copy_flag_overrides_config() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        // Config disabled, but flag should enable it
        .config("workon.autoCopy", "false")
        .config("workon.copyPattern", "*.txt")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    fs::write(main_worktree.join("test.txt"), "content")?;

    // Create new worktree with --copy flag
    let mut new_cmd = cargo_bin_cmd!("git-workon");
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--copy")
        .arg("feature")
        .assert()
        .success();

    let feature_worktree = fixture.root()?.join("feature");

    // Verify file was copied despite config being false
    assert!(
        feature_worktree.join("test.txt").exists(),
        "Should copy file when --copy flag is used"
    );

    Ok(())
}

#[test]
fn new_no_copy_flag_overrides_config() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        // Config enabled, but flag should disable it
        .config("workon.autoCopy", "true")
        .config("workon.copyPattern", "*.txt")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    fs::write(main_worktree.join("test.txt"), "content")?;

    // Create new worktree with --no-copy flag
    let mut new_cmd = cargo_bin_cmd!("git-workon");
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--no-copy")
        .arg("feature")
        .assert()
        .success();

    let feature_worktree = fixture.root()?.join("feature");

    // Verify file was NOT copied despite config being true
    assert!(
        !feature_worktree.join("test.txt").exists(),
        "Should not copy file when --no-copy flag is used"
    );

    Ok(())
}

#[test]
fn new_auto_copy_skips_when_base_worktree_missing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        // Note: no main worktree created
        .config("workon.autoCopy", "true")
        .config("workon.copyPattern", "*.txt")
        .build()?;

    // Create new worktree (should succeed even though base worktree doesn't exist)
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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
        .config("workon.autoCopy", "true")
        // Note: no copyPattern configured - should default to copying everything
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    fs::write(main_worktree.join("test.txt"), "content")?;
    fs::write(main_worktree.join("readme.md"), "readme")?;
    fs::create_dir_all(main_worktree.join("src"))?;
    fs::write(main_worktree.join("src/main.rs"), "code")?;

    // Create new worktree (should copy all files using default pattern)
    let mut new_cmd = cargo_bin_cmd!("git-workon");
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

    cargo_bin_cmd!("git-workon")
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

    cargo_bin_cmd!("git-workon")
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

fn spawn_interactive(cwd: &Path, args: &[&str]) -> Session<OsProcess, OsStream> {
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

// ── stack-aware base selection and gt track ───────────────────────────────────

fn path_without_gt_new() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|dir| !std::path::Path::new(dir).join("gt").exists())
        .collect::<Vec<_>>()
        .join(":")
}

#[test]
fn new_defaults_base_to_cwd_head_when_in_stacked_worktree() -> Result<(), Box<dyn std::error::Error>>
{
    // feat-1 is last worktree so CWD = feat-1; it has an extra commit to distinguish from main.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .config("workon.gtAutoTrack", "false")
        .branch_metadata("feat-1", "main")
        .build()?;

    let feat1_oid = fixture
        .commit("feat-1")
        .file("stack.txt", "stack content")
        .create("commit on feat-1")?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .arg("new")
        .arg("feat-2")
        .assert()
        .success();

    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    bare_repo.assert(predicate::repo::branch_points_to("feat-2", feat1_oid));

    Ok(())
}

#[test]
fn new_explicit_base_overrides_smart_default() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .config("workon.gtAutoTrack", "false")
        .branch_metadata("feat-1", "main")
        .build()?;

    fixture
        .commit("feat-1")
        .file("stack.txt", "content")
        .create("commit on feat-1")?;

    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    let main_oid = bare_repo
        .find_branch("main", git2::BranchType::Local)?
        .get()
        .target()
        .unwrap();

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .arg("new")
        .arg("feat-2")
        .arg("--base")
        .arg("main")
        .assert()
        .success();

    bare_repo.assert(predicate::repo::branch_points_to("feat-2", main_oid));

    Ok(())
}

#[test]
fn new_no_stack_flag_disables_smart_base() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .config("workon.gtAutoTrack", "false")
        .branch_metadata("feat-1", "main")
        .build()?;

    fixture
        .commit("feat-1")
        .file("stack.txt", "content")
        .create("commit on feat-1")?;

    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    let main_oid = bare_repo
        .find_branch("main", git2::BranchType::Local)?
        .get()
        .target()
        .unwrap();

    let feat1_path = fixture.root()?.join("feat-1");
    cargo_bin_cmd!("git-workon")
        .current_dir(&feat1_path)
        .arg("new")
        .arg("--no-stack")
        .arg("feat-2")
        .assert()
        .success();

    bare_repo.assert(predicate::repo::branch_points_to("feat-2", main_oid));

    Ok(())
}

#[test]
fn new_gt_track_failure_is_non_fatal() -> Result<(), Box<dyn std::error::Error>> {
    // When stackModel=graphite but gt is absent, worktree is still created + warning printed.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .build()?;

    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .env("PATH", path_without_gt_new())
        .arg("new")
        .arg("feat-2")
        .output()?;

    assert!(
        output.status.success(),
        "new must succeed even when gt track fails; stderr: {}",
        std::str::from_utf8(&output.stderr).unwrap_or("(invalid utf8)")
    );

    let bare_path = fixture.root()?.join(".bare");
    git2::Repository::open_bare(&bare_path)?.assert(predicate::repo::has_branch("feat-2"));

    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(
        stderr.contains("Warning:") && stderr.contains("gt track"),
        "expected gt track warning in stderr: {stderr}"
    );

    Ok(())
}

#[test]
fn new_skips_gt_track_when_gt_auto_track_false() -> Result<(), Box<dyn std::error::Error>> {
    // gtAutoTrack=false suppresses gt track entirely — no warning emitted.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .config("workon.gtAutoTrack", "false")
        .branch_metadata("feat-1", "main")
        .build()?;

    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .env("PATH", path_without_gt_new())
        .arg("new")
        .arg("feat-2")
        .output()?;

    assert!(output.status.success());
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(
        !stderr.contains("gt track"),
        "gt track must not be invoked when gtAutoTrack=false: {stderr}"
    );

    Ok(())
}

#[test]
fn new_attaching_existing_tracked_branch_skips_gt_track() -> Result<(), Box<dyn std::error::Error>>
{
    // feat-2 already has branch-metadata (tracked in the stack) but no worktree yet.
    // Running `workon new feat-2` should create the worktree without calling `gt track --parent`,
    // which would otherwise re-parent the branch and trigger an unwanted restack/rebase.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("feat-2", "feat-1") // already tracked, no worktree
        .build()?;

    // Give feat-1 an extra commit so feat-2 has something to be based on
    fixture
        .commit("feat-1")
        .file("stack.txt", "content")
        .create("commit on feat-1")?;

    // Advance feat-2 to feat-1's current HEAD (fixture builder created it at the initial
    // commit; now we want it at feat-1's tip after the extra commit above).
    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    let feat1_oid = bare_repo
        .find_branch("feat-1", git2::BranchType::Local)?
        .get()
        .target()
        .unwrap();
    let feat1_commit = bare_repo.find_commit(feat1_oid)?;
    bare_repo.branch("feat-2", &feat1_commit, true)?; // force=true: update existing branch

    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .env("PATH", path_without_gt_new())
        .arg("new")
        .arg("feat-2")
        .output()?;

    assert!(
        output.status.success(),
        "new must succeed for already-tracked branch; stderr: {}",
        std::str::from_utf8(&output.stderr).unwrap_or("(invalid utf8)")
    );

    // Worktree was created
    bare_repo.assert(predicate::repo::has_worktree("feat-2"));

    // No gt track invocation (would show as a warning since gt is absent from PATH)
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(
        !stderr.contains("gt track"),
        "gt track must not be invoked for an already-tracked branch: {stderr}"
    );

    Ok(())
}

#[test]
fn new_attaching_existing_branch_with_slashes_skips_gt_track(
) -> Result<(), Box<dyn std::error::Error>> {
    // Branch names with slashes (e.g. "ee/zip-cli-aws") have refs like
    // refs/branch-metadata/ee/zip-cli-aws. The old current_stack guard failed to
    // detect these because fnmatch "*" doesn't cross "/". The branch_pre_existed
    // guard must work correctly regardless of slashes in the branch name.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1") // CWD for the command; no slash so fixture creates it fine
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("ee/feat-2", "feat-1") // slashed target branch, no worktree
        .build()?;

    fixture
        .commit("feat-1")
        .file("stack.txt", "content")
        .create("commit on feat-1")?;

    // Advance ee/feat-2 to feat-1's current HEAD (fixture builder created it at the initial
    // commit; now we want it at feat-1's tip after the extra commit above).
    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    let feat1_oid = bare_repo
        .find_branch("feat-1", git2::BranchType::Local)?
        .get()
        .target()
        .unwrap();
    let feat1_commit = bare_repo.find_commit(feat1_oid)?;
    bare_repo.branch("ee/feat-2", &feat1_commit, true)?; // force=true: update existing branch

    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .env("PATH", path_without_gt_new())
        .arg("new")
        .arg("ee/feat-2")
        .output()?;

    assert!(
        output.status.success(),
        "new must succeed for slashed branch; stderr: {}",
        std::str::from_utf8(&output.stderr).unwrap_or("(invalid utf8)")
    );

    // git-workon encodes the root-relative path into the worktree admin name (ADR-027)
    bare_repo.assert(predicate::repo::has_worktree("ee~feat-2"));

    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(
        !stderr.contains("gt track"),
        "gt track must not be invoked for a pre-existing slashed branch: {stderr}"
    );

    Ok(())
}

// ── gh-stack link hook ────────────────────────────────────────────────────────

#[test]
fn new_links_gh_stack_worktree_after_creation() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "gh-stack")
        .gh_stack(None, 1, "main", &[])
        .build()?;

    let main_path = fixture.root()?.join("main");
    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("new")
        .arg("feat-1")
        .assert()
        .success();

    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    bare_repo.assert(predicate::repo::gh_stack_is_linked("feat-1"));

    Ok(())
}

#[test]
fn new_skips_gh_stack_link_with_no_stack_flag() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .config("workon.stackModel", "gh-stack")
        .gh_stack(None, 1, "main", &[])
        .build()?;

    let main_path = fixture.root()?.join("main");
    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("new")
        .arg("--no-stack")
        .arg("feat-1")
        .assert()
        .success();

    let bare_path = fixture.root()?.join(".bare");
    let bare_repo = git2::Repository::open_bare(&bare_path)?;
    bare_repo.assert(predicate::repo::gh_stack_is_linked("feat-1").not());

    Ok(())
}
