use assert_cmd::cargo_bin_cmd;
use git_workon_fixture::prelude::*;
use std::fs;
use std::path::Path;

#[test]
fn copy_basic() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "**/*")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    // Create some untracked files in main worktree
    fs::create_dir_all(main_worktree.join("node_modules"))?;
    fs::write(main_worktree.join("node_modules/package.json"), "{}")?;
    fs::create_dir_all(main_worktree.join("build"))?;
    fs::write(
        main_worktree.join("build/output.js"),
        "console.log('test');",
    )?;

    // Copy files from main to feature (uses config patterns)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    // Verify files were copied
    assert!(
        feature_worktree.join("node_modules/package.json").exists(),
        "Should copy node_modules/package.json"
    );
    assert!(
        feature_worktree.join("build/output.js").exists(),
        "Should copy build/output.js"
    );

    // Verify content is correct
    let content = fs::read_to_string(feature_worktree.join("build/output.js"))?;
    assert_eq!(content, "console.log('test');");

    Ok(())
}

#[test]
fn copy_with_config_patterns() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "*.txt")
        .config("workon.copyPattern", "build/*")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    // Create various untracked files
    fs::write(main_worktree.join("test.txt"), "text file")?;
    fs::write(main_worktree.join("ignore.log"), "log file")?;
    fs::create_dir_all(main_worktree.join("build"))?;
    fs::write(main_worktree.join("build/output.js"), "build output")?;

    // Copy using config patterns (default behavior)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    // Verify only pattern-matched files were copied
    assert!(
        feature_worktree.join("test.txt").exists(),
        "Should copy test.txt (matches *.txt)"
    );
    assert!(
        feature_worktree.join("build/output.js").exists(),
        "Should copy build/output.js (matches build/*)"
    );
    assert!(
        !feature_worktree.join("ignore.log").exists(),
        "Should not copy ignore.log (no matching pattern)"
    );

    Ok(())
}

#[test]
fn copy_with_excludes() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "**/*")
        .config("workon.copyExclude", "*.log")
        .config("workon.copyExclude", "temp/*")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    // Create various files
    fs::write(main_worktree.join("app.js"), "app code")?;
    fs::write(main_worktree.join("debug.log"), "debug info")?;
    fs::create_dir_all(main_worktree.join("temp"))?;
    fs::write(main_worktree.join("temp/cache.dat"), "cached data")?;

    // Copy all files using config pattern
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    // Verify files were copied except excluded ones
    assert!(
        feature_worktree.join("app.js").exists(),
        "Should copy app.js (not excluded)"
    );
    assert!(
        !feature_worktree.join("debug.log").exists(),
        "Should not copy debug.log (excluded by *.log)"
    );
    assert!(
        !feature_worktree.join("temp/cache.dat").exists(),
        "Should not copy temp/cache.dat (excluded by temp/*)"
    );

    Ok(())
}

#[test]
fn copy_exclude_bare_dir_name_prunes_subtree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyExclude", "logs")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    fs::write(main_worktree.join("app.js"), "app code")?;
    fs::create_dir_all(main_worktree.join("logs/nested"))?;
    fs::write(main_worktree.join("logs/a.log"), "log")?;
    fs::write(main_worktree.join("logs/nested/deep.txt"), "deep")?;

    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    assert!(
        feature_worktree.join("app.js").exists(),
        "Should copy app.js (not excluded)"
    );
    assert!(
        !feature_worktree.join("logs").exists(),
        "Should not copy anything under logs/ (bare dir name excludes the subtree)"
    );

    Ok(())
}

#[test]
fn copy_exclude_dir_glob_prunes_subtree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyExclude", "target/**")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    fs::write(main_worktree.join("app.js"), "app code")?;
    fs::create_dir_all(main_worktree.join("target/debug"))?;
    fs::write(main_worktree.join("target/debug/app.o"), "object")?;

    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    assert!(
        feature_worktree.join("app.js").exists(),
        "Should copy app.js (not excluded)"
    );
    assert!(
        !feature_worktree.join("target").exists(),
        "Should not copy anything under target/ (dir/** excludes the subtree)"
    );

    Ok(())
}

#[test]
fn copy_with_pattern_override() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "*.txt")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    // Create various files
    fs::write(main_worktree.join("readme.txt"), "readme")?;
    fs::write(main_worktree.join("app.js"), "app code")?;

    // Copy with --pattern flag (should override config)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("--pattern")
        .arg("*.js")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    // Verify only JS file was copied (pattern override)
    assert!(
        feature_worktree.join("app.js").exists(),
        "Should copy app.js (matches --pattern *.js)"
    );
    assert!(
        !feature_worktree.join("readme.txt").exists(),
        "Should not copy readme.txt (config pattern ignored)"
    );

    Ok(())
}

#[test]
fn copy_respects_force_flag() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "**/*")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    // Create files in both worktrees
    fs::write(main_worktree.join("data.txt"), "main version")?;
    fs::write(feature_worktree.join("data.txt"), "feature version")?;

    // Copy without --force (should skip existing file)
    let mut cmd = cargo_bin_cmd!("git-workon");
    let output = cmd
        .current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .output()?;

    assert!(output.status.success());

    // Verify file was not overwritten
    let content = fs::read_to_string(feature_worktree.join("data.txt"))?;
    assert_eq!(
        content, "feature version",
        "Should not overwrite without --force"
    );

    // Now copy with --force (should overwrite)
    let mut cmd2 = cargo_bin_cmd!("git-workon");
    cmd2.current_dir(&fixture)
        .arg("copy")
        .arg("--force")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    // Verify file was overwritten
    let content = fs::read_to_string(feature_worktree.join("data.txt"))?;
    assert_eq!(content, "main version", "Should overwrite with --force");

    Ok(())
}

#[test]
fn copy_creates_directories() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "**/*")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    // Create nested directory structure
    fs::create_dir_all(main_worktree.join("src/components/ui"))?;
    fs::write(
        main_worktree.join("src/components/ui/Button.js"),
        "button code",
    )?;

    // Copy files
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    // Verify directory structure was created
    assert!(
        feature_worktree.join("src/components/ui").is_dir(),
        "Should create nested directories"
    );
    assert!(
        feature_worktree
            .join("src/components/ui/Button.js")
            .exists(),
        "Should copy file in nested directory"
    );

    let content = fs::read_to_string(feature_worktree.join("src/components/ui/Button.js"))?;
    assert_eq!(content, "button code");

    Ok(())
}

#[test]
fn copy_with_no_matching_files() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    let main_worktree = fixture.root()?.join("main");

    // Create a file that won't match the pattern
    fs::write(main_worktree.join("readme.txt"), "readme")?;

    // Try to copy with non-matching pattern
    let mut cmd = cargo_bin_cmd!("git-workon");
    let output = cmd
        .current_dir(&fixture)
        .arg("copy")
        .arg("--pattern")
        .arg("*.js")
        .arg("main")
        .arg("feature")
        .output()?;

    // Should succeed (copying 0 files is not an error)
    assert!(output.status.success());

    // Verify output shows 0 files copied
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Copied 0 file(s)"),
        "Should report 0 files copied"
    );

    Ok(())
}

#[test]
fn copy_with_cli_exclude() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "**/*")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    fs::write(main_worktree.join("app.js"), "app code")?;
    fs::write(main_worktree.join("debug.log"), "log data")?;

    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("--exclude")
        .arg("*.log")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    assert!(
        feature_worktree.join("app.js").exists(),
        "Should copy app.js (not excluded)"
    );
    assert!(
        !feature_worktree.join("debug.log").exists(),
        "Should not copy debug.log (excluded by --exclude *.log)"
    );

    Ok(())
}

#[test]
fn copy_with_cli_exclude_additive_with_config() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "**/*")
        .config("workon.copyExclude", "*.log")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    fs::write(main_worktree.join("app.js"), "app code")?;
    fs::write(main_worktree.join("debug.log"), "log data")?;
    fs::write(main_worktree.join("secret.tmp"), "temp data")?;

    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("--exclude")
        .arg("*.tmp")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    assert!(
        feature_worktree.join("app.js").exists(),
        "Should copy app.js (not excluded)"
    );
    assert!(
        !feature_worktree.join("debug.log").exists(),
        "Should not copy debug.log (excluded by config)"
    );
    assert!(
        !feature_worktree.join("secret.tmp").exists(),
        "Should not copy secret.tmp (excluded by --exclude)"
    );

    Ok(())
}

#[test]
fn copy_with_multiple_cli_excludes() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .config("workon.copyPattern", "**/*")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    fs::write(main_worktree.join("app.js"), "app code")?;
    fs::write(main_worktree.join("debug.log"), "log data")?;
    fs::create_dir_all(main_worktree.join("temp"))?;
    fs::write(main_worktree.join("temp/cache.dat"), "cached data")?;

    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("--exclude")
        .arg("*.log")
        .arg("--exclude")
        .arg("temp/*")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    assert!(
        feature_worktree.join("app.js").exists(),
        "Should copy app.js (not excluded)"
    );
    assert!(
        !feature_worktree.join("debug.log").exists(),
        "Should not copy debug.log (excluded by first --exclude)"
    );
    assert!(
        !feature_worktree.join("temp/cache.dat").exists(),
        "Should not copy temp/cache.dat (excluded by second --exclude)"
    );

    Ok(())
}

#[test]
fn copy_without_config_defaults_to_all() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    // Create various files
    fs::write(main_worktree.join("test.txt"), "test")?;
    fs::write(main_worktree.join("readme.md"), "readme")?;
    fs::create_dir_all(main_worktree.join("src"))?;
    fs::write(main_worktree.join("src/main.rs"), "fn main() {}")?;

    // Copy without config or pattern (should copy everything)
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    // Verify all files were copied
    assert!(
        feature_worktree.join("test.txt").exists(),
        "Should copy test.txt"
    );
    assert!(
        feature_worktree.join("readme.md").exists(),
        "Should copy readme.md"
    );
    assert!(
        feature_worktree.join("src/main.rs").exists(),
        "Should copy src/main.rs"
    );

    Ok(())
}

#[test]
fn copy_does_not_copy_dot_git() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    // In a worktree, .git is a file (not directory) containing a gitdir pointer
    assert!(
        main_worktree.join(".git").exists(),
        ".git must exist in source worktree"
    );
    let original_git = fs::read_to_string(feature_worktree.join(".git"))?;

    // Create an untracked file so the copy actually runs
    fs::write(main_worktree.join("test.txt"), "test")?;

    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(&fixture)
        .arg("copy")
        .arg("main")
        .arg("feature")
        .assert()
        .success();

    // .git must not have been overwritten
    let after_git = fs::read_to_string(feature_worktree.join(".git"))?;
    assert_eq!(
        original_git, after_git,
        ".git file must not be modified by copy"
    );

    // Normal untracked file was still copied
    assert!(
        feature_worktree.join("test.txt").exists(),
        "test.txt should be copied"
    );

    Ok(())
}

#[test]
fn copy_omit_to_defaults_to_current_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feature")
        .build()?;

    let main_worktree = fixture.root()?.join("main");
    let feature_worktree = fixture.root()?.join("feature");

    fs::write(main_worktree.join("data.txt"), "from main")?;

    // Run from inside the feature worktree, omitting the `to` argument
    let mut cmd = cargo_bin_cmd!("git-workon");
    cmd.current_dir(Path::new(&feature_worktree))
        .arg("copy")
        .arg("main")
        .assert()
        .success();

    assert!(
        feature_worktree.join("data.txt").exists(),
        "Should copy to current worktree when 'to' is omitted"
    );
    let content = fs::read_to_string(feature_worktree.join("data.txt"))?;
    assert_eq!(content, "from main");

    Ok(())
}
