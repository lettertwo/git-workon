use assert_cmd::Command;
use git_workon_fixture::prelude::*;
use std::fs;

#[test]
fn hook_executes_successfully() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .config(
            "workon.postCreateHook",
            "echo 'Hook executed' > hook_output.txt",
        )
        .build()?;

    // Create a new worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify the worktree was created
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    // Verify the hook executed by checking for output file
    let hook_output_path = fixture.root()?.join("feature/hook_output.txt");
    assert!(
        hook_output_path.exists(),
        "Hook should have created output file"
    );

    let content = fs::read_to_string(hook_output_path)?;
    assert!(
        content.contains("Hook executed"),
        "Hook output should contain expected text"
    );

    Ok(())
}

#[test]
fn hook_failure_shows_warning() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .config("workon.postCreateHook", "exit 1")
        .build()?;

    // Create a new worktree with a failing hook
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    let output = new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .output()?;

    // Command should still succeed despite hook failure
    assert!(
        output.status.success(),
        "Command should succeed even when hook fails"
    );

    // Verify warning message in stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Warning: Post-create hook failed"),
        "Should show warning about hook failure"
    );

    // Verify the worktree was still created
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    fixture.assert(predicate::repo::has_branch("feature"));

    Ok(())
}

#[test]
fn no_hooks_flag_skips_execution() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .config(
            "workon.postCreateHook",
            "echo 'Hook executed' > hook_output.txt",
        )
        .build()?;

    // Create a new worktree with --no-hooks flag
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--no-hooks")
        .arg("feature")
        .assert()
        .success();

    // Verify the worktree was created
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    // Verify the hook did NOT execute
    let hook_output_path = fixture.root()?.join("feature/hook_output.txt");
    assert!(
        !hook_output_path.exists(),
        "Hook should not have executed with --no-hooks flag"
    );

    Ok(())
}

#[test]
fn multiple_hooks_execute_sequentially() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .config("workon.postCreateHook", "echo 'First hook' > hook1.txt")
        .config("workon.postCreateHook", "echo 'Second hook' > hook2.txt")
        .build()?;

    // Create a new worktree
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify both hooks executed
    let hook1_path = fixture.root()?.join("feature/hook1.txt");
    let hook2_path = fixture.root()?.join("feature/hook2.txt");

    assert!(hook1_path.exists(), "First hook should have executed");
    assert!(hook2_path.exists(), "Second hook should have executed");

    let content1 = fs::read_to_string(hook1_path)?;
    let content2 = fs::read_to_string(hook2_path)?;

    assert!(content1.contains("First hook"));
    assert!(content2.contains("Second hook"));

    Ok(())
}

#[test]
fn hook_environment_variables_set() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("develop")
        .config(
            "workon.postCreateHook",
            "env | grep WORKON_ | sort > env_vars.txt",
        )
        .build()?;

    // Create a new worktree with a base branch
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("--base")
        .arg("develop")
        .arg("feature")
        .assert()
        .success();

    // Read the environment variables captured by the hook
    let env_file_path = fixture.root()?.join("feature/env_vars.txt");
    assert!(
        env_file_path.exists(),
        "Hook should have captured environment variables"
    );

    let env_content = fs::read_to_string(env_file_path)?;

    // Verify WORKON_WORKTREE_PATH is set
    assert!(
        env_content.contains("WORKON_WORKTREE_PATH="),
        "Should set WORKON_WORKTREE_PATH"
    );
    assert!(
        env_content.contains("/feature"),
        "Path should end with worktree name"
    );

    // Verify WORKON_BRANCH_NAME is set
    assert!(
        env_content.contains("WORKON_BRANCH_NAME=feature"),
        "Should set WORKON_BRANCH_NAME to branch name"
    );

    // Verify WORKON_BASE_BRANCH is set
    assert!(
        env_content.contains("WORKON_BASE_BRANCH=develop"),
        "Should set WORKON_BASE_BRANCH to base branch"
    );

    Ok(())
}

#[test]
fn hook_executes_in_clone_command() -> Result<(), Box<dyn std::error::Error>> {
    // Create a source repository to clone
    let source_fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a temp directory for the clone destination
    let clone_temp = assert_fs::TempDir::new()?;

    // Clone the repository
    let mut clone_cmd = Command::cargo_bin("git-workon")?;
    clone_cmd
        .current_dir(&clone_temp)
        .arg("clone")
        .arg(source_fixture.cwd()?.to_str().unwrap())
        .arg("cloned")
        .assert()
        .success();

    let clone_path = clone_temp.path().join("cloned");

    // Set up a hook in the cloned repo for testing
    let cloned_repo = git2::Repository::open(clone_path.join(".bare"))?;
    let mut config = cloned_repo.config()?;
    config.set_str(
        "workon.postCreateHook",
        "echo 'Clone hook' > clone_hook.txt",
    )?;
    drop(config);
    drop(cloned_repo);

    // Now create a new worktree in the cloned repo to test hooks
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&clone_path)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify hook executed
    let hook_output = clone_path.join("feature/clone_hook.txt");
    assert!(
        hook_output.exists(),
        "Hook should execute in cloned repository"
    );

    clone_temp.close()?;
    Ok(())
}

#[test]
fn hook_executes_in_init_command() -> Result<(), Box<dyn std::error::Error>> {
    let test_dir = assert_fs::TempDir::new()?;
    let init_path = test_dir.join("initialized");

    // Initialize a new repository
    let mut init_cmd = Command::cargo_bin("git-workon")?;
    init_cmd
        .current_dir(&test_dir)
        .arg("init")
        .arg(&init_path)
        .assert()
        .success();

    // Set up a hook in the initialized repo
    let init_repo = git2::Repository::open(&init_path)?;
    let mut config = init_repo.config()?;
    config.set_str("workon.postCreateHook", "echo 'Init hook' > init_hook.txt")?;
    drop(config);
    drop(init_repo);

    // Create a new worktree to test hooks
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&init_path)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify hook executed
    let hook_output = init_path.join("feature/init_hook.txt");
    assert!(
        hook_output.exists(),
        "Hook should execute in initialized repository"
    );

    Ok(())
}

#[test]
fn no_hooks_configured_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    // Create a new worktree without any hooks configured
    let mut new_cmd = Command::cargo_bin("git-workon")?;
    new_cmd
        .current_dir(&fixture)
        .arg("new")
        .arg("feature")
        .assert()
        .success();

    // Verify the worktree was created successfully
    fixture
        .root()?
        .child("feature")
        .assert(predicate::path::is_dir());

    fixture.assert(predicate::repo::has_branch("feature"));

    Ok(())
}
