use std::ffi::{OsStr, OsString};
use std::path::Path;

use clap::builder::StyledStr;
use clap::{Command, CommandFactory};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use workon::WorktreeDescriptor;

use crate::display::worktree_display_row;

fn worktree_help(wt: &WorktreeDescriptor, root: &Path, current_dir: &Path) -> Option<StyledStr> {
    let Ok(row) = worktree_display_row(wt, root, current_dir) else {
        return None;
    };

    let mut parts = Vec::new();

    if row.is_active {
        parts.push("→".to_string());
    }
    if !row.indicators.is_empty() {
        parts.push(row.indicators.join(" "));
    }
    if !row.last_activity.is_empty() {
        parts.push(row.last_activity);
    }
    if let Some(ann) = row.branch_annotation {
        parts.push(ann);
    }

    Some(StyledStr::from(parts.join("  ")))
}

pub fn complete_worktree_names(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(repo) = workon::get_repo(None) else {
        return vec![];
    };
    let Ok(worktrees) = workon::get_worktrees(&repo) else {
        return vec![];
    };
    let Ok(root) = workon::workon_root(&repo) else {
        return vec![];
    };
    let current_dir = std::env::current_dir().unwrap_or_default();
    let prefix = current.to_string_lossy();
    // Offer the root-relative path rather than the (possibly `~`-encoded) admin name —
    // it's what a user typed to create the worktree, and `find_worktree` resolves it.
    worktrees
        .iter()
        .filter_map(|wt| {
            let relative = workon::relative_worktree_path(&repo, wt.path())?;
            if !relative.starts_with(prefix.as_ref()) {
                return None;
            }
            Some(CompletionCandidate::new(relative).help(worktree_help(wt, root, &current_dir)))
        })
        .collect()
}

pub fn complete_branch_names(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(repo) = workon::get_repo(None) else {
        return vec![];
    };
    let Ok(branches) = repo.branches(None) else {
        return vec![];
    };
    let prefix = current.to_string_lossy();
    branches
        .filter_map(|b| b.ok())
        .filter_map(|(branch, _)| branch.name().ok().flatten().map(str::to_owned))
        .filter(|name| name.starts_with(prefix.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

/// Surface `git-workon-*` executables on `$PATH` as top-level subcommands, so `git workon <TAB>`
/// offers them alongside the built-ins (mirroring `git`'s / `cargo`'s plugin completion). A name
/// already claimed by a built-in is skipped — the built-in owns it (dispatch enforces the same
/// precedence). Each is a bare stub `Command`; per-external argument completion (delegating into the
/// external's own `COMPLETE=` responder) is a separate concern, not wired here.
fn augment_external_subcommands(cmd: Command) -> Command {
    let known = crate::dispatch::known_subcommands(&cmd);
    crate::dispatch::external_subcommand_names()
        .into_iter()
        .filter(|name| !known.contains(name))
        .fold(cmd, |cmd, name| {
            // `Command::new` needs `&'static str` and clap's `Str` won't take an owned `String`.
            // Leaking is fine here: `augment` only runs inside the ephemeral `COMPLETE=` completer
            // process, which emits candidates and exits immediately — the handful of leaked names
            // live microseconds, never accumulating across invocations.
            let name: &'static str = String::leak(name);
            cmd.subcommand(Command::new(name).about("External git-workon subcommand"))
        })
}

/// First-party externals known to speak the `clap_complete` `COMPLETE=` responder protocol.
///
/// Delegation (below) has to *execute* the external to get its completions — there is no way to
/// probe "does this binary support `COMPLETE=`" without running it, and a plain user script (the
/// external-subcommand surface explicitly supports those; see `dispatch.rs` and the
/// `PathStub::command` tests) ignores `COMPLETE` entirely and just runs, turning a TAB press into
/// an arbitrary side-effecting execution with its normal stdout misread as completion candidates.
/// ADR-036 only promises this delegation for the review binary, so the allowlist starts there.
/// A future git-config allowlist (`workon.*`, ADR-006) can let users opt other externals in
/// deliberately — extend this list (or make it configurable) when that lands.
const DELEGATED_EXTERNALS: &[&str] = &["review"];

/// Sub-delegate `git-workon <ext> <words...><TAB>` completion to `git-workon-<ext>`'s own
/// `COMPLETE=<shell>` responder — the M6 CS3-deferred seam, wired here per ADR-036 CS5.
///
/// `augment_external_subcommands` (above) only adds a bare stub `Command` for each PATH-discovered
/// external, with no argument definitions of its own — clap_complete's engine has no notion that
/// the stub actually stands in for a whole other program, so anything typed after the external's
/// name would otherwise complete against nothing (verified manually: `git workon review <TAB>`
/// offered only global flags before this). This function runs *before* `CompleteEnv`'s own
/// dispatch in `main`, so it can intercept that case and hand off instead.
///
/// It re-derives the shell's word list and completion index the same way `clap_complete`'s own
/// bash/elvish adapters do (`_CLAP_COMPLETE_INDEX`; zsh/fish don't set that var and always mean
/// "the last word", so that's the fallback). If the first non-flag word after the program name
/// names a subcommand in `DELEGATED_EXTERNALS` (see its doc comment for why delegation is gated at
/// all) that also resolves to a `git-workon-<name>` executable on `$PATH` (and isn't a known
/// built-in — a built-in's own `Cli` already completes itself) *and* the word actually being
/// completed sits after it, this re-invokes that executable under the identical protocol: the
/// leading `<program-name> <ext>` words collapse into one placeholder word (`git-workon-<ext>`,
/// mirroring how the external is invoked for real by `dispatch::try_dispatch`) and the completion
/// index shifts down by however many leading words were collapsed away. Stdin is nulled for the
/// delegated process — an external that prompts on stdin must not be able to block the user's
/// shell on a TAB press. The external's stdout — already shell-formatted by its own `CompleteEnv`
/// responder — is copied through verbatim, and this process exits with the external's exit code.
///
/// A no-op (returns without printing or exiting) whenever `COMPLETE` isn't set, there's no word
/// after the program name, the completing index lands on the subcommand slot itself (that's
/// still a top-level candidate list, not a delegation target), the leading word is a known
/// built-in, the leading word isn't in `DELEGATED_EXTERNALS`, or nothing matching is found on
/// `$PATH` — every one of those falls through to `CompleteEnv`'s normal dispatch in `main`.
pub fn try_delegate_external_completion() {
    let Some(shell) = std::env::var_os("COMPLETE") else {
        return;
    };
    if shell.is_empty() || shell == "0" {
        return;
    }

    let args: Vec<OsString> = std::env::args_os().collect();
    let Some(dash_dash) = args.iter().position(|a| a == "--") else {
        return;
    };
    let words = &args[dash_dash + 1..];
    if words.len() < 2 {
        return; // nothing after the program-name word to delegate
    }

    // Mirrors clap_complete's own env adapters: bash/elvish read `_CLAP_COMPLETE_INDEX`; zsh/fish
    // always treat the last word as the one being completed.
    let index = std::env::var("_CLAP_COMPLETE_INDEX")
        .ok()
        .and_then(|i| i.parse::<usize>().ok())
        .unwrap_or(words.len() - 1);

    // The first non-flag word after the program name (index 0) is the subcommand candidate.
    let Some(subcmd_pos) = words[1..]
        .iter()
        .position(|w| !w.to_str().is_some_and(|s| s.starts_with('-')))
        .map(|i| i + 1)
    else {
        return;
    };
    if index <= subcmd_pos {
        return; // completing the subcommand slot itself, not a word after it
    }

    let Some(name) = words[subcmd_pos].to_str() else {
        return;
    };
    let known = crate::dispatch::known_subcommands(&crate::cli::Cli::command());
    if known.contains(name) {
        return; // a built-in owns this name; its own Cli completes it
    }
    if !DELEGATED_EXTERNALS.contains(&name) {
        return; // protocol support isn't known/promised for this external; don't execute it
    }
    let Some(exe) = crate::dispatch::find_external(name) else {
        return; // no matching external on PATH
    };

    let mut delegated_words: Vec<OsString> = vec![OsString::from(format!("git-workon-{name}"))];
    delegated_words.extend(words[subcmd_pos + 1..].iter().cloned());
    let delegated_index = index - subcmd_pos;

    let output = std::process::Command::new(&exe)
        .env("COMPLETE", &shell)
        .env("_CLAP_COMPLETE_INDEX", delegated_index.to_string())
        .stdin(std::process::Stdio::null())
        .arg("--")
        .args(&delegated_words)
        .output();

    let Ok(output) = output else {
        std::process::exit(0); // fail closed: no candidates rather than a broken TAB
    };
    use std::io::Write as _;
    let _ = std::io::stdout().write_all(&output.stdout);
    std::process::exit(output.status.code().unwrap_or(0));
}

pub fn augment(cmd: Command) -> Command {
    let cmd = augment_external_subcommands(cmd);
    cmd.mut_arg("name", |a| {
        a.add(ArgValueCompleter::new(complete_worktree_names))
    })
    .mut_subcommand("find", |sub| {
        sub.mut_arg("name", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
    })
    .mut_subcommand("prune", |sub| {
        sub.mut_arg("names", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
    })
    .mut_subcommand("move", |sub| {
        sub.mut_arg("names", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
    })
    .mut_subcommand("copy", |sub| {
        sub.mut_arg("from", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
        .mut_arg("to", |a| {
            a.add(ArgValueCompleter::new(complete_worktree_names))
        })
    })
    .mut_subcommand("new", |sub| {
        sub.mut_arg("base", |a| {
            a.add(ArgValueCompleter::new(complete_branch_names))
        })
    })
}
