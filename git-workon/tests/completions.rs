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
