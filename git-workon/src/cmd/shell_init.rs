//! Shell integration: init script generation and worktree completion.
//!
//! Generates shell wrapper functions and completion scripts for bash, zsh, and fish.
//! The wrapper captures `git workon` stdout and `cd`s to the result when it's a directory.
//!
//! ## Usage
//!
//! ```bash
//! eval "$(git workon shell-init)"        # auto-detect from $SHELL
//! eval "$(git workon shell-init bash)"   # bash
//! eval "$(git workon shell-init zsh)"    # zsh
//! git workon shell-init fish | source    # fish
//! ```
//!
//! ## Future Work
//!
//! TODO: Frequency/recency tracking for smart defaults (zoxide-style)
//! TODO: Determine feasability of acheiving cd from git extension, e.g.:
//!  - `git workon jump <pattern>` — fast jump by fuzzy match using frequency data
//!  - `git workon switch <pattern>` — alternative name for jump
//!  - `git workon find <pattern> --jump` — alternative to augment `find` witha jump/switch behavior

use std::{
    io::{self, Write},
    path::Path,
};

use clap_complete::env::{Bash, EnvCompleter, Fish, Zsh};
use miette::{miette, IntoDiagnostic, Result};
use workon::WorktreeDescriptor;

use crate::cli::{Shell, ShellInit};

use super::Run;

fn shell_from_path(path: &str) -> Result<Shell> {
    let basename = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match basename {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        other => Err(miette!(
            "Unrecognized shell '{other}'; specify a shell explicitly (bash, zsh, or fish)"
        )),
    }
}

fn detect_shell() -> Result<Shell> {
    let shell_var = std::env::var("SHELL").map_err(|_| {
        miette!("$SHELL is not set; specify a shell explicitly (bash, zsh, or fish)")
    })?;
    shell_from_path(&shell_var)
}

fn generate_shell_integration(shell: Shell, bin_name: &str, buf: &mut dyn Write) -> Result<()> {
    let pkg_name = env!("CARGO_PKG_NAME");
    let env_shell: &dyn EnvCompleter = match shell {
        Shell::Bash => &Bash,
        Shell::Zsh => &Zsh,
        Shell::Fish => &Fish,
    };

    env_shell
        .write_registration("COMPLETE", pkg_name, pkg_name, pkg_name, buf)
        .into_diagnostic()?;

    if bin_name != pkg_name {
        env_shell
            .write_registration("COMPLETE", bin_name, bin_name, pkg_name, buf)
            .into_diagnostic()?;
    }

    write!(
        buf,
        "{}",
        match shell {
            Shell::Bash => BASH_TEMPLATE.replace("{cmd}", bin_name),
            Shell::Zsh => ZSH_TEMPLATE.replace("{cmd}", bin_name),
            Shell::Fish => FISH_TEMPLATE.replace("{cmd}", bin_name),
        }
    )
    .into_diagnostic()
}

const BASH_TEMPLATE: &str = r#"{cmd}() {
    local result
    result="$(command git workon "$@")" || { local code=$?; printf '%s\n' "$result"; return $code; }
    if [ -d "$result" ]; then
        cd "$result" || return $?
    elif [ -n "$result" ]; then
        printf '%s\n' "$result"
    fi
}
"#;

const ZSH_TEMPLATE: &str = r#"{cmd}() {
    local result
    result="$(command git workon "$@")" || { local code=$?; printf '%s\n' "$result"; return $code; }
    if [ -d "$result" ]; then
        cd "$result" || return $?
    elif [ -n "$result" ]; then
        printf '%s\n' "$result"
    fi
}
"#;

const FISH_TEMPLATE: &str = r#"function {cmd}
    set -l result (command git workon $argv)
    set -l code $status
    if test $code -ne 0; printf '%s\n' $result; return $code; end
    if test -d "$result"; cd $result
    else if test -n "$result"; printf '%s\n' $result; end
end
"#;

impl Run for ShellInit {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let cmd = &self.cmd;
        let shell = match self.shell {
            Some(s) => s,
            None => detect_shell()?,
        };

        generate_shell_integration(shell, cmd, &mut io::stdout()).map(|_| None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init(shell: Shell, cmd: &str) -> String {
        let cmd_str = cmd.to_string();
        let template = match shell {
            Shell::Bash => BASH_TEMPLATE,
            Shell::Zsh => ZSH_TEMPLATE,
            Shell::Fish => FISH_TEMPLATE,
        };
        template.replace("{cmd}", &cmd_str)
    }

    #[test]
    fn bash_contains_function_name() {
        let script = init(Shell::Bash, "workon");
        assert!(script.contains("workon()"));
    }

    #[test]
    fn bash_custom_cmd() {
        let script = init(Shell::Bash, "gw");
        assert!(script.contains("gw()"));
        assert!(!script.contains("workon()"));
    }

    #[test]
    fn zsh_custom_cmd() {
        let script = init(Shell::Zsh, "gw");
        assert!(script.contains("gw()"));
        assert!(!script.contains("workon()"));
    }

    #[test]
    fn fish_contains_function_keyword() {
        let script = init(Shell::Fish, "workon");
        assert!(script.contains("function workon"));
    }

    #[test]
    fn fish_custom_cmd() {
        let script = init(Shell::Fish, "gw");
        assert!(script.contains("function gw"));
        assert!(!script.contains("function workon"));
    }

    #[test]
    fn detect_shell_bash() {
        assert!(matches!(shell_from_path("/bin/bash").unwrap(), Shell::Bash));
    }

    #[test]
    fn detect_shell_zsh() {
        assert!(matches!(
            shell_from_path("/usr/bin/zsh").unwrap(),
            Shell::Zsh
        ));
    }

    #[test]
    fn detect_shell_fish() {
        assert!(matches!(
            shell_from_path("/usr/local/bin/fish").unwrap(),
            Shell::Fish
        ));
    }

    #[test]
    fn detect_shell_unrecognized() {
        assert!(shell_from_path("/bin/sh").is_err());
    }
}
