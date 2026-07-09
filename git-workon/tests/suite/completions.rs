// use assert_cmd::Command;
//
// #[test]
// fn completions_bash() {
//     let source = include_str!("../contrib/completions/git-workon.bash");
//     Command::new("bash")
//         .args(&["--noprofile", "--norc", "-c", source])
//         .assert()
//         .success()
//         .stdout("")
//         .stderr("");
// }
//
// #[test]
// fn completions_fish() {
//     let source = include_str!("../contrib/completions/git-workon.fish");
//     let tempdir = tempfile::tempdir().unwrap();
//     let tempdir = tempdir.path().to_str().unwrap();
//
//     Command::new("fish")
//         .env("HOME", tempdir)
//         .args(&["--command", source, "--private"])
//         .assert()
//         .success()
//         .stdout("")
//         .stderr("");
// }
//
// #[test]
// fn completions_powershell() {
//     let source = include_str!("../contrib/completions/_git-workon.ps1");
//     Command::new("pwsh")
//         .args(&[
//             "-NoLogo",
//             "-NonInteractive",
//             "-NoProfile",
//             "-Command",
//             source,
//         ])
//         .assert()
//         .success()
//         .stdout("")
//         .stderr("");
// }
//
// #[test]
// fn completions_zsh() {
//     let source = r#"
//     set -eu
//     completions='./contrib/completions'
//     test -d "$completions"
//     fpath=("$completions" $fpath)
//     autoload -Uz compinit
//     compinit -u
//     "#;
//
//     Command::new("zsh")
//         .args(&["-c", source, "--no-rcs"])
//         .assert()
//         .success()
//         .stdout("")
//         .stderr("");
// }

use assert_cmd::cargo_bin_cmd;
use git_workon_fixture::prelude::*;

/// Drive clap_complete's dynamic `COMPLETE=bash` protocol for `git workon <words>` and return the
/// emitted candidate *values*. The runtime protocol (see the generated bash registration) requires
/// `_CLAP_COMPLETE_INDEX` (the word position being completed); omitting it yields "no completion
/// generated". Output is one candidate per line, each formatted `value\013help` — so the value is
/// the text before the first `\013`.
fn bash_candidates(path: &str, words: &[&str], index: usize) -> Vec<String> {
    let output = cargo_bin_cmd!("git-workon")
        .env("PATH", path)
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", index.to_string())
        .arg("--")
        .args(words)
        .output()
        .expect("completion invocation");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            line.split('\u{000b}')
                .next()
                .unwrap_or(line)
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn tab_lists_external_subcommands_from_path() {
    let stub = PathStub::new().unwrap().command("review").unwrap();

    // Completing the subcommand slot (`git workon <TAB>`) offers the PATH-discovered external
    // alongside the built-ins.
    let candidates = bash_candidates(&stub.path(), &["git-workon", ""], 1);
    assert!(
        candidates.iter().any(|c| c == "review"),
        "expected `review` among candidates: {candidates:?}"
    );
    assert!(
        candidates.iter().any(|c| c == "find"),
        "built-ins must still complete too: {candidates:?}"
    );
}

/// The sibling `git-workon-review` binary, built alongside this test binary as part of the
/// workspace (`cargo test --workspace` / `-p git-workon` after a workspace build both produce
/// it). Located via the workspace's `target/<profile>/` (one level up from this crate's
/// `CARGO_MANIFEST_DIR`) rather than pulled in as a Cargo dev-dependency, which would drag
/// `git-workon-review`'s whole tree-sitter-heavy dependency tree into every `git-workon` test
/// build for one delegation test. NOT `current_exe()`-relative: this repo shares cargo's
/// intermediate build artifacts across worktrees (a `build-dir` override in `.cargo/config.toml`),
/// so test binaries themselves live under that shared dir while final binaries still land in
/// this worktree's own `target/<profile>/`.
fn review_binary_path() -> std::path::PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("target")
        .join(profile)
        .join("git-workon-review")
}

/// ADR-030 CS5 / M6 CS3's deferred seam: `git workon review <TAB>` shells out to the review
/// binary's own `COMPLETE=` responder rather than completing against review's argument-less stub
/// `Command` (`augment_external_subcommands`). Uses the *real* compiled `git-workon-review`
/// (via `PathStub::command_exe`, not the canned `arg:`/`cwd:` script `command` writes) so the
/// candidates asserted here — `stack`/`uncommitted` — are genuinely the review binary's own SOURCE
/// completer output, proving the index-shifted shell-out end to end.
#[test]
fn tab_after_review_subcommand_delegates_to_review_binary_completer() {
    let review_exe = review_binary_path();
    assert!(
        review_exe.is_file(),
        "expected a sibling git-workon-review binary at {review_exe:?} \
         (built as part of a workspace build/test run)"
    );

    let stub = PathStub::new()
        .unwrap()
        .command_exe("review", &review_exe)
        .unwrap();

    // `git workon review <TAB>` — index 2 is the (empty) word after `review`.
    let candidates = bash_candidates(&stub.path(), &["git-workon", "review", ""], 2);

    assert!(
        candidates.iter().any(|c| c == "stack"),
        "expected the review binary's own `stack` keyword candidate, delegated through: {candidates:?}"
    );
    assert!(
        candidates.iter().any(|c| c == "uncommitted"),
        "expected the review binary's own `uncommitted` keyword candidate, delegated through: {candidates:?}"
    );
}

#[test]
fn external_subcommand_does_not_shadow_a_builtin_in_completion() {
    // A `git-workon-list` stub must not produce a duplicate `list` candidate — the built-in owns
    // the name (same precedence dispatch enforces).
    let stub = PathStub::new().unwrap().command("list").unwrap();

    let candidates = bash_candidates(&stub.path(), &["git-workon", "list"], 1);
    let list_count = candidates.iter().filter(|c| *c == "list").count();
    assert_eq!(
        list_count, 1,
        "`list` should appear exactly once (built-in, not doubled by the external): {candidates:?}"
    );
}
