//! Doctor command for detecting and repairing workspace issues.
//!
//! Detect and repair workspace issues using git's native `git worktree repair` plus
//! additional workon-specific checks.
//!
//! ### Worktree Checks (per-worktree):
//! - Missing worktree directories (in git list but directory deleted) — fixable with --fix
//! - Broken git links (.git file pointing to non-existent location) — manual fix needed
//! - Worktrees whose upstream branch is gone — informational
//!
//! ### Dependency Checks (once):
//! - Hook commands not found in PATH (from workon.postCreateHook config)
//! - gh CLI not available (required for PR workflow features)
//!
//! ## Flags:
//! - `--fix` - Automatically repair fixable issues (missing directory entries)
//! - `--dry-run` - Preview fixes without applying

use std::path::{Path, PathBuf};

use log::debug;
use miette::{IntoDiagnostic, Result};
use serde_json::json;
use workon::{get_repo, get_worktrees, WorkonConfig, WorktreeDescriptor};

use crate::cli::Doctor;
use crate::output;

use super::Run;

#[derive(Debug)]
enum IssueKind {
    MissingDirectory,
    BrokenGitLink,
    GoneUpstream,
    HookNotFound { hook: String, command: String },
    GhNotFound,
}

struct Issue {
    kind: IssueKind,
    name: Option<String>,
    path: Option<PathBuf>,
}

impl Issue {
    fn worktree(kind: IssueKind, name: &str, path: PathBuf) -> Self {
        Self {
            kind,
            name: Some(name.to_string()),
            path: Some(path),
        }
    }

    fn dependency(kind: IssueKind) -> Self {
        Self {
            kind,
            name: None,
            path: None,
        }
    }

    fn fixable(&self) -> bool {
        matches!(self.kind, IssueKind::MissingDirectory)
    }

    fn message(&self) -> String {
        match &self.kind {
            IssueKind::MissingDirectory => "missing directory".to_string(),
            IssueKind::BrokenGitLink => {
                "broken git link (run 'git worktree repair' to fix)".to_string()
            }
            IssueKind::GoneUpstream => {
                "upstream branch is gone (suggest: git workon prune --gone)".to_string()
            }
            IssueKind::HookNotFound { hook, command } => {
                format!("hook command '{command}' not found in PATH (from hook \"{hook}\")")
            }
            IssueKind::GhNotFound => "gh CLI not found (PR features unavailable)".to_string(),
        }
    }

    fn kind_str(&self) -> &'static str {
        match self.kind {
            IssueKind::MissingDirectory => "missing_directory",
            IssueKind::BrokenGitLink => "broken_git_link",
            IssueKind::GoneUpstream => "gone_upstream",
            IssueKind::HookNotFound { .. } => "hook_not_found",
            IssueKind::GhNotFound => "gh_not_found",
        }
    }
}

impl Run for Doctor {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;
        let worktrees = get_worktrees(&repo)?;
        let config = WorkonConfig::new(&repo)?;

        debug!("found {} worktree(s)", worktrees.len());
        output::status(&format!("Checking {} worktree(s)...", worktrees.len()));

        // Worktree checks — collect issues and print inline as we go
        let mut issues: Vec<Issue> = Vec::new();
        for wt in &worktrees {
            if let Some(name) = wt.name() {
                let path = wt.path().to_path_buf();
                debug!("'{}': checking at {}", name, path.display());
                let raw = repo.find_worktree(name).into_diagnostic()?;
                if raw.validate().is_err() {
                    if !path.exists() {
                        debug!("'{}': validate failed, directory missing", name);
                        let issue = Issue::worktree(IssueKind::MissingDirectory, name, path);
                        output::check_fail(name, &issue.message());
                        issues.push(issue);
                    } else {
                        debug!("'{}': validate failed, broken git link", name);
                        let issue = Issue::worktree(IssueKind::BrokenGitLink, name, path);
                        output::check_fail(name, &issue.message());
                        issues.push(issue);
                    }
                } else {
                    debug!("'{}': validate ok, checking upstream", name);
                    if wt.has_gone_upstream().unwrap_or(false) {
                        debug!("'{}': upstream is gone", name);
                        let issue = Issue::worktree(IssueKind::GoneUpstream, name, path);
                        output::check_warn(name, &issue.message());
                        issues.push(issue);
                    } else {
                        debug!("'{}': ok", name);
                        output::check_pass(name);
                    }
                }
            }
        }

        // Dependency checks — print section header then check inline
        output::status("\nChecking dependencies...");

        debug!("checking gh CLI availability");
        if gh_available() {
            debug!("gh CLI: ok");
            output::check_pass("gh");
        } else {
            debug!("gh CLI not found in PATH");
            let issue = Issue::dependency(IssueKind::GhNotFound);
            output::check_fail("gh", "not found in PATH");
            issues.push(issue);
        }

        let hooks = config.post_create_hooks()?;
        debug!("checking {} configured hook(s)", hooks.len());
        for hook in &hooks {
            if let Some(command) = hook.split_whitespace().next() {
                debug!("checking hook command '{}' in PATH", command);
                if command_in_path(command) {
                    debug!("hook command '{}': ok", command);
                    output::check_pass(&format!("{command} (hook)"));
                } else {
                    debug!("hook command '{}' not found in PATH", command);
                    let issue = Issue::dependency(IssueKind::HookNotFound {
                        hook: hook.clone(),
                        command: command.to_string(),
                    });
                    output::check_fail(
                        command,
                        &format!("not found in PATH (from hook \"{hook}\")"),
                    );
                    issues.push(issue);
                }
            }
        }

        debug!("found {} issue(s) total", issues.len());

        // JSON output: serialize all collected issues
        if self.json {
            let fixed_names: Vec<String> = if self.fix && !self.dry_run {
                fix_issues(&repo, &issues)?
            } else {
                Vec::new()
            };

            let issues_json: Vec<_> = issues
                .iter()
                .map(|issue| {
                    let mut obj = json!({
                        "kind": issue.kind_str(),
                        "fixable": issue.fixable(),
                        "message": issue.message(),
                    });
                    if let Some(name) = &issue.name {
                        obj["name"] = json!(name);
                    }
                    if let Some(path) = &issue.path {
                        obj["path"] = json!(path.to_str());
                    }
                    if let IssueKind::HookNotFound { hook, command } = &issue.kind {
                        obj["hook"] = json!(hook);
                        obj["command"] = json!(command);
                    }
                    obj
                })
                .collect();

            let result = json!({
                "issues": issues_json,
                "fixed": fixed_names,
                "dry_run": self.dry_run,
            });
            let output = serde_json::to_string_pretty(&result).into_diagnostic()?;
            println!("{}", output);
            return Ok(None);
        }

        // Text output: summary / action
        output::status("");

        if issues.is_empty() {
            output::success("All checks passed.");
            return Ok(None);
        }

        let fixable_count = issues.iter().filter(|i| i.fixable()).count();

        if self.dry_run {
            if fixable_count == 0 {
                output::notice("No issues can be automatically fixed.");
            } else {
                output::notice(&format!(
                    "Would fix {} issue(s). Dry run — no changes made.",
                    fixable_count
                ));
            }
            return Ok(None);
        }

        if self.fix {
            if fixable_count == 0 {
                output::status("No issues can be automatically fixed.");
            } else {
                output::info(&format!("Fixing {} issue(s)...", fixable_count));
                let fixed = fix_issues(&repo, &issues)?;
                for name in &fixed {
                    output::success(&format!("  ✓ Pruned: {name}"));
                }
            }
        } else if fixable_count > 0 {
            output::status(&format!(
                "{} issue(s) can be automatically fixed. Run with --fix to apply.",
                fixable_count
            ));
        }

        Ok(None)
    }
}

/// Prune worktrees with missing directories. Returns the names of pruned worktrees.
fn fix_issues(repo: &git2::Repository, issues: &[Issue]) -> Result<Vec<String>> {
    let mut fixed = Vec::new();
    for issue in issues.iter().filter(|i| i.fixable()) {
        if let Some(name) = &issue.name {
            debug!("pruning worktree '{}'", name);
            let worktree = repo.find_worktree(name).into_diagnostic()?;
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(true);
            worktree.prune(Some(&mut opts)).into_diagnostic()?;
            debug!("pruned worktree '{}'", name);
            fixed.push(name.clone());
        }
    }
    Ok(fixed)
}

/// Check if a command is available in PATH (or as a path).
fn command_in_path(cmd: &str) -> bool {
    if cmd.starts_with('/') || cmd.starts_with("./") {
        return Path::new(cmd).exists();
    }
    if let Ok(path) = std::env::var("PATH") {
        return path.split(':').any(|dir| Path::new(dir).join(cmd).exists());
    }
    false
}

/// Check if the gh CLI is available.
fn gh_available() -> bool {
    command_in_path("gh")
}
