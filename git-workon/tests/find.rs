use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::cargo_bin_cmd;
use expectrl::{
    session::{OsProcess, OsStream},
    Expect, Session,
};
use git_workon_fixture::prelude::*;

#[test]
fn find_exact_match() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .build()?;

    cargo_bin_cmd!("git-workon")
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

    cargo_bin_cmd!("git-workon")
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

    cargo_bin_cmd!("git-workon")
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

    cargo_bin_cmd!("git-workon")
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
    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .arg("find")
        .arg("dirty")
        .arg("--dirty")
        .assert()
        .success();

    // Should NOT find clean with --dirty filter
    cargo_bin_cmd!("git-workon")
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

    cargo_bin_cmd!("git-workon")
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
    cargo_bin_cmd!("git-workon")
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
    cargo_bin_cmd!("git-workon")
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
const TAB: &[u8] = b"\t";

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

// ── stack-member fallback ─────────────────────────────────────────────────────

#[test]
fn find_falls_back_to_stack_branch_when_no_worktree_match() -> Result<(), Box<dyn std::error::Error>>
{
    // "feat-1" is a worktree. "step-2" is only a branch in feat-1's stack.
    // Searching for "step-2" should fall back to returning the "feat-1" worktree.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("step-2", "feat-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stdout = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("find")
            .arg("--no-interactive")
            .arg("step-2")
            .output()?
            .stdout,
    )?;

    assert!(
        stdout.contains("feat-1"),
        "expected feat-1 worktree path in output: {stdout}"
    );

    Ok(())
}

#[test]
fn find_stack_fallback_zero_matches_returns_no_matching_error(
) -> Result<(), Box<dyn std::error::Error>> {
    // Neither "ghost" worktree nor any stack branch matches → original error.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .build()?;

    let main_path = fixture.root()?.join("main");
    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("find")
        .arg("--no-interactive")
        .arg("ghost")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No matching worktree"));

    Ok(())
}

#[test]
fn find_stack_fallback_multiple_matches_bail_under_no_interactive(
) -> Result<(), Box<dyn std::error::Error>> {
    // "common" appears in both feat-a's stack and feat-b's stack.
    // With --no-interactive this must fail, not prompt.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-a")
        .worktree("feat-b")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-a", "main")
        .branch_metadata("common-a", "feat-a")
        .branch_metadata("feat-b", "main")
        .branch_metadata("common-b", "feat-b")
        .build()?;

    let main_path = fixture.root()?.join("main");
    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("find")
        .arg("--no-interactive")
        .arg("common")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Multiple stacks"));

    Ok(())
}

#[test]
fn find_no_stack_flag_disables_stack_fallback() -> Result<(), Box<dyn std::error::Error>> {
    // "step-2" would be found via the stack fallback, but --no-stack disables it.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("step-2", "feat-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("--no-stack")
        .arg("step-2")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No matching worktree"));

    Ok(())
}

#[test]
fn find_stack_fallback_is_case_insensitive() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("UPPER-step", "feat-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let stdout = String::from_utf8(
        cargo_bin_cmd!("git-workon")
            .current_dir(&main_path)
            .arg("find")
            .arg("--no-interactive")
            .arg("upper")
            .output()?
            .stdout,
    )?;

    assert!(
        stdout.contains("feat-1"),
        "case-insensitive stack fallback should return feat-1: {stdout}"
    );

    Ok(())
}

// ── New hybrid picker behavior ─────────────────────────────────────────────────

/// Typing a query jumps the cursor to the best-matching item even when a
/// different item was the initial default (is_active).
#[test]
fn find_picker_typing_jumps_cursor_to_best_match() -> Result<(), Box<dyn std::error::Error>> {
    // "docs" is the last worktree added, so it's current_dir (is_active).
    // Typing "feat" matches only "feature"; cursor should jump there.
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("feature")
        .worktree("docs")
        .build()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["find"]);

    session.expect("Select a worktree")?;
    // Type "feat" — only "feature" matches; cursor jumps to it.
    session.send(b"feat")?;
    session.send(ENTER)?;

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    assert!(
        selected.contains("feature") && !selected.contains("docs"),
        "Expected typing 'feat' to jump cursor to 'feature', got: {selected}"
    );

    Ok(())
}

/// Pressing Esc cancels the picker without selecting any worktree.
#[test]
fn find_picker_esc_cancels_selection() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("alpha")
        .worktree("bravo")
        .build()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["find"]);

    session.expect("Select a worktree")?;
    // ESC: lone \x1b with no follow-up sequence → Key::Escape.
    session.send(b"\x1b")?;

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    // After Esc the picker exits without printing a worktree path.
    // The last non-empty output is a display line (starts with space/→),
    // not an absolute path (which would start with '/').
    assert!(
        !selected.starts_with('/'),
        "Expected no path after Esc, got: {selected}"
    );

    Ok(())
}

/// `--new` / `-n` bypasses the stack-aware resolver and creates a fresh worktree.
///
/// Without `--new`, the resolver applies Rule 3: `step-2` is in `feat-1`'s stack
/// and `feat-1` has a worktree, so it picks `Checkout { host: "feat-1" }`.
/// With `--new`, we skip the resolver entirely and materialize a new worktree.
#[test]
fn find_new_flag_forces_fresh_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .branch("step-2")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("step-2", "feat-1")
        .build()?;

    let main_path = fixture.root()?.join("main");
    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&main_path)
        .arg("step-2")
        .arg("--new")
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        output.status.success(),
        "Expected success, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("step-2"),
        "Expected step-2 worktree path in output: {stdout}"
    );

    let repo = fixture.repo()?;
    repo.assert(predicate::repo::has_worktree("step-2"));

    Ok(())
}

/// `Tab` in the stack tree picker force-materializes a fresh worktree instead of
/// navigating to the containing stack-home.
///
/// Without Tab (Enter), selecting `step-2` (a ◯ diff in `feat-1`'s stack) returns
/// the `feat-1` worktree (granularity=Stack). With Tab, a new worktree for `step-2`
/// is created and its path is returned.
#[test]
fn find_tab_forces_materialize_in_tree_picker() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("feat-1")
        .branch("step-2")
        .config("workon.stackModel", "graphite")
        .branch_metadata("feat-1", "main")
        .branch_metadata("step-2", "feat-1")
        .build()?;

    // Run from feat-1 so it is the active (◉) worktree; cursor starts there.
    // step-2 is the next tree item (one ArrowDown away).
    let feat1_path = fixture.root()?.join("feat-1");
    let mut session = spawn_interactive(&feat1_path, &["find"]);

    session.expect("Select a worktree")?;
    session.send(ARROW_DOWN)?; // move cursor from feat-1 to step-2
    session.send(TAB)?; // force-materialize step-2

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    assert!(
        selected.contains("step-2"),
        "Expected step-2 worktree path after Tab, got: {selected}"
    );
    assert!(
        !selected.contains("feat-1"),
        "Tab should create step-2 worktree, not return feat-1 path: {selected}"
    );

    Ok(())
}

// ── DeletedNode routing ───────────────────────────────────────────────────────

/// `git workon ghost-branch` where `ghost-branch` exists in graphite stack metadata
/// but has no local branch ref should exit non-zero with a `DeletedBranchNode` error
/// that names the branch and points at `gt`.
///
/// Run from `other-work` (not in ghost-branch's stack) so rules 2/3 don't fire first.
#[test]
fn route_deleted_stack_node_errors_with_gt_guidance() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("other-work")
        .graphite_config(&["main"])
        .branch_metadata("ghost-branch", "main")
        // Deliberately no .branch("ghost-branch") — simulates a deleted local ref.
        // No branch_metadata for other-work, so other-work is not in ghost-branch's stack.
        .config("workon.stackModel", "graphite")
        .build()?;

    // Run from other-work: its branch is not in ghost-branch's stack, so
    // rules 2/3 don't fire and resolve_action reaches the DeletedNode check.
    let other_work_path = fixture.root()?.join("other-work");

    cargo_bin_cmd!("git-workon")
        .current_dir(&other_work_path)
        .args(["ghost-branch", "--no-interactive"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("stack metadata"));

    Ok(())
}

/// `git workon --json ghost-branch` for a deleted stack node must emit the
/// structured `{"error": ...}` envelope on stdout — routing errors honor the
/// JSON contract the same way run errors do.
#[test]
fn route_deleted_stack_node_json_emits_structured_error() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .worktree("other-work")
        .graphite_config(&["main"])
        .branch_metadata("ghost-branch", "main")
        .config("workon.stackModel", "graphite")
        .build()?;

    let other_work_path = fixture.root()?.join("other-work");

    let output = cargo_bin_cmd!("git-workon")
        .current_dir(&other_work_path)
        .args(["--json", "ghost-branch"])
        .output()?;

    assert!(!output.status.success(), "should fail on deleted node");

    let stdout = String::from_utf8(output.stdout)?;
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json output should be valid JSON");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("ghost-branch"),
        "error.message should name the branch, got: {json}"
    );

    Ok(())
}

/// The explicit subcommand form `git workon find <name> --new` must honor the
/// flag the same way bare-name routing does: force a fresh worktree.
#[test]
fn find_new_flag_forces_materialize() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .branch("feature")
        .build()?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["find", "feature", "--new", "--no-interactive"])
        .assert()
        .success();

    fixture.assert(predicate::repo::has_worktree("feature"));

    Ok(())
}

/// `git workon find --new` without a name is an error, not a silent no-op.
#[test]
fn find_new_flag_without_name_fails() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .build()?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["find", "--new", "--no-interactive"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--new requires"));

    Ok(())
}

/// `git workon <name> --no-interactive` routed to `New` must not open the
/// ambiguous-remote prompt — the flag has to follow the constructed command.
#[test]
fn route_to_new_propagates_no_interactive() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("main")
        .branch("feature")
        .remote("teamA", "https://example.com/teamA.git")
        .remote("teamB", "https://example.com/teamB.git")
        .upstream("feature", "teamA/feature")
        .upstream("feature", "teamB/feature")
        .build()?;

    // Remote-only branch on two equal-tier remotes: interactive mode would
    // prompt; --no-interactive must pick the first candidate instead.
    fixture
        .repo()?
        .find_branch("feature", git2::BranchType::Local)?
        .delete()?;

    cargo_bin_cmd!("git-workon")
        .current_dir(&fixture)
        .args(["feature", "--no-interactive"])
        .assert()
        .success();

    fixture.assert(predicate::repo::has_worktree("feature"));

    Ok(())
}

/// `Tab` in the flat (non-stack) picker behaves identically to `Enter` — it
/// navigates to the selected worktree without any force-materialize side-effect.
#[test]
fn find_flat_picker_tab_acts_as_enter() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .worktree("alpha")
        .worktree("bravo")
        .build()?;

    let mut session = spawn_interactive(fixture.as_ref(), &["find"]);

    session.expect("Select a worktree")?;
    session.send(TAB)?; // confirm with Tab; should resolve like Enter

    let output = session.expect(expectrl::Eof)?;
    let selected = last_line(output.get(0).unwrap());

    assert!(
        selected.contains("alpha") || selected.contains("bravo"),
        "Expected a worktree path after Tab in flat picker, got: {selected}"
    );

    Ok(())
}
