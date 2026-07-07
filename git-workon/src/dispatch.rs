//! Manual pre-parse intercept for external-subcommand dispatch.
//!
//! `git workon <name> [args...]` execs `git-workon-<name>` on `$PATH` when `<name>` isn't a
//! known built-in subcommand — mirroring `git-*` / `cargo-*` plugin dispatch. This is deliberately
//! **not** implemented with clap's `allow_external_subcommands` / `#[command(external_subcommand)]`:
//! `Cli`'s flattened `Find.name` (see `cli.rs`) already captures any unrecognized first token as a
//! worktree-name candidate for the ADR-004 default-command routing in `main.rs`. A native clap
//! external-subcommand mechanism would swallow *every* unknown token, which would break
//! `git workon my-branch` routing outright. Instead, [`try_dispatch`] runs as a manual intercept
//! before `Cli::parse()`, and falls through to normal parsing (unchanged) whenever it doesn't
//! recognize an external to dispatch to.
//!
//! Precedence: a known built-in subcommand always wins over a same-named external on `PATH` (a
//! stray `git-workon-list` can never shadow the built-in `list`). An external, if found, shadows
//! a same-named branch/worktree — `git workon find <name>` remains the explicit escape hatch.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;

/// Collect the set of known subcommand names for `cmd`: primary names, all aliases (visible and
/// hidden), plus clap's injected `help`. Derived from the `Command` rather than hardcoded, so it
/// can't drift as `Cmd` variants are added, renamed, or gain aliases.
pub fn known_subcommands(cmd: &clap::Command) -> HashSet<String> {
    let mut names: HashSet<String> = cmd
        .get_subcommands()
        .flat_map(|sub| {
            std::iter::once(sub.get_name().to_string())
                .chain(sub.get_all_aliases().map(|a| a.to_string()))
        })
        .collect();
    names.insert("help".to_string());
    names
}

/// Scan `$PATH` for an executable `git-workon-<name>`.
///
/// Unix-only exec-bit check for now; Windows `.exe` support is a future concern (not needed for
/// the current dispatch scope). Reused by the completer in a later changeset to enumerate
/// PATH-discovered externals.
pub fn find_external(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let bin_name = format!("git-workon-{name}");
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(&bin_name);
        is_executable(&candidate).then_some(candidate)
    })
}

/// Enumerate every `git-workon-<suffix>` executable reachable on `$PATH`, returning the sorted set
/// of `<suffix>` names. Used by the completer to surface external subcommands as top-level
/// completion candidates (the enumeration counterpart to [`find_external`]'s keyed lookup). Sorted
/// + deduped so a name earlier on `$PATH` shadowing a later one appears once, deterministically.
pub fn external_subcommand_names() -> BTreeSet<String> {
    let Some(path_var) = std::env::var_os("PATH") else {
        return BTreeSet::new();
    };
    let mut names = BTreeSet::new();
    for dir in std::env::split_paths(&path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let raw = entry.file_name();
            let Some(file) = raw.to_str() else { continue };
            let Some(suffix) = file.strip_prefix("git-workon-") else {
                continue;
            };
            if suffix.is_empty() || !is_executable(&entry.path()) {
                continue;
            }
            names.insert(suffix.to_string());
        }
    }
    names
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Run the dispatch intercept. Called between `CompleteEnv::…complete()` and `Cli::parse()` in
/// `main`. If an unrecognized first argument matches an external `git-workon-<name>` on `PATH`,
/// this execs it (spawn+wait, propagating the child's exit code) and terminates the process —
/// it never returns in that case. Otherwise it returns, and normal clap parsing proceeds
/// unaffected.
///
/// Steps (locked decision, see module docs): no args → return; leading-dash first arg (a global
/// flag like `--version`) → return; first arg is a known built-in → return; PATH miss → return;
/// PATH hit → exec and exit.
pub fn try_dispatch(known: &HashSet<String>) {
    let mut args = std::env::args_os();
    args.next(); // argv[0] (the git-workon binary path)

    let Some(first) = args.next() else {
        return; // no args at all → normal parse
    };

    let Some(first_str) = first.to_str() else {
        return; // non-UTF8 first arg: let clap's own parsing/error path handle it
    };

    if first_str.starts_with('-') {
        return; // global flag (--version, --help, -v, --json, ...) → normal parse
    }

    if known.contains(first_str) {
        return; // known built-in subcommand → normal parse (checked before PATH lookup)
    }

    let Some(exe) = find_external(first_str) else {
        return; // no matching external on PATH → normal parse, falls through to find.name
    };

    let rest: Vec<OsString> = args.collect();
    let code = match std::process::Command::new(&exe).args(&rest).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("git-workon: failed to run {}: {e}", exe.display());
            1
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn known_subcommands_includes_names_aliases_and_help() {
        let known = known_subcommands(&crate::cli::Cli::command());
        assert!(known.contains("find"));
        assert!(known.contains("list"));
        assert!(known.contains("ls")); // visible alias
        assert!(known.contains("check")); // visible alias for doctor
        assert!(known.contains("help"));
        assert!(known.contains("_complete")); // hidden but still a real subcommand
    }

    #[test]
    fn find_external_misses_when_not_on_path() {
        assert!(find_external("definitely-not-a-real-subcommand-xyz").is_none());
    }
}
