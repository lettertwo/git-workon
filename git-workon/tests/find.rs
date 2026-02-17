use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use git_workon_fixture::prelude::*;

#[test]
fn find_exact_match() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("feature")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature"));

    Ok(())
}

#[test]
fn find_no_match_errors() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("nonexistent")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No matching worktree"));

    Ok(())
}

#[test]
fn find_multiple_fuzzy_matches_errors_with_no_interactive() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-1")
        .worktree("feature-2")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("feature")
        .arg("--no-interactive")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Multiple worktrees match"));

    Ok(())
}

#[test]
fn find_no_name_errors_with_no_interactive() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("--no-interactive")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No worktree name provided"));

    Ok(())
}

#[test]
fn find_with_dirty_filter() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean")
        .worktree("dirty")
        .build()?;

    // Make dirty worktree dirty
    std::fs::write(
        fixture.root()?.child("dirty").join("test.txt"),
        "uncommitted",
    )?;

    // Should find dirty when searching with --dirty
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("dirty")
        .arg("--dirty")
        .assert()
        .success();

    // Should NOT find clean with --dirty filter
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("clean")
        .arg("--dirty")
        .assert()
        .failure();

    Ok(())
}

#[test]
fn find_all_filtered_out_errors() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("clean")
        .build()?;

    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("--dirty")
        .arg("--no-interactive")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No worktrees match"));

    Ok(())
}

#[test]
fn find_single_fuzzy_match_returns_directly() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature-branch")
        .worktree("other")
        .build()?;

    // "feature" matches only "feature-branch" → return without interaction
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("feature")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature-branch"));

    Ok(())
}

#[test]
fn find_case_insensitive_fuzzy_match() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("Feature-Branch")
        .build()?;

    // "feature" should match "Feature-Branch" (case-insensitive)
    Command::cargo_bin("git-workon")?
        .current_dir(&fixture)
        .arg("find")
        .arg("feature")
        .assert()
        .success();

    Ok(())
}

// --- Interactive PTY tests ---

const ARROW_DOWN: &[u8] = b"\x1b[B";
const ENTER: &[u8] = b"\r";

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
///
/// The PTY stream mixes stderr (picker UI with all items) and stdout (selected path).
/// The final line is the worktree path printed to stdout — the actual selection result.
fn last_line(output: &[u8]) -> String {
    let text = String::from_utf8_lossy(output);
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    // Strip ANSI escape sequences (e.g., \x1b[?25h cursor show)
    let mut result = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip escape sequence: ESC followed by '[' then params and a final letter
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
fn find_multiple_fuzzy_matches_interactive_select_default() -> Result<(), Box<dyn std::error::Error>>
{
    // Use names with no shared substring so we can distinguish them in PTY output.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("apple")
        .worktree("avocado")
        .worktree("blueberry") // Add a non-matching worktree as default
        .build()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["find", "a"]);

    session.expect("Select a worktree")?;
    session.send(ENTER)?;

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    assert!(
        selected.contains("avocado") || selected.contains("apple"),
        "Expected default Enter to select 'avocado', got: {selected}"
    );

    Ok(())
}

#[test]
fn find_no_name_interactive_select_default() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("alpha")
        .worktree("bravo")
        .build()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["find"]);

    session.expect("Select a worktree")?;
    session.send(ENTER)?;

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    // Pressing Enter selects the default worktree (last worktree we added, which is "bravo")
    assert!(
        selected.contains("bravo"),
        "Expected output to contain a worktree path, got: {selected}"
    );

    Ok(())
}

#[test]
fn find_interactive_arrow_key_navigation() -> Result<(), Box<dyn std::error::Error>> {
    // Use names with no shared substring so we can distinguish them in PTY output.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("apple")
        .worktree("avocado")
        .build()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["find", "a"]);

    session.expect("Select a worktree")?;
    session.send(ARROW_DOWN)?;
    session.send(ENTER)?;

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    // Arrow-down moves past the default (avocado) to select apple
    assert!(
        selected.contains("apple") && !selected.contains("avocado"),
        "Expected arrow-down to select 'apple' (not the default 'avocado'), got: {selected}"
    );

    Ok(())
}
