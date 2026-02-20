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

        // Configuration section — informational only, not included in fixable issues
        output::status("\nChecking configuration...");
        let config_entries = read_config_entries(&repo, &config)?;
        for (key, value, source) in &config_entries {
            match source {
                Some(src) => output::check_pass(&format!("{key} = {value} ({src})")),
                None => output::check_pass(&format!("{key} = {value}")),
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

            let config_json: serde_json::Map<String, serde_json::Value> = config_entries
                .into_iter()
                .map(|(k, v, s)| (k, json!({ "value": v, "source": s })))
                .collect();

            let result = json!({
                "issues": issues_json,
                "fixed": fixed_names,
                "dry_run": self.dry_run,
                "configuration": config_json,
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

/// Abbreviate the home directory as `~` in a path string.
fn abbreviate_home(path: &std::path::Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

/// Returns the abbreviated file path for a given config level.
fn config_level_path(repo: &git2::Repository, level: git2::ConfigLevel) -> Option<String> {
    let path = match level {
        git2::ConfigLevel::Local => Some(repo.path().join("config")),
        git2::ConfigLevel::Worktree => Some(repo.path().join("config.worktree")),
        git2::ConfigLevel::Global => git2::Config::find_global().ok(),
        git2::ConfigLevel::XDG => git2::Config::find_xdg().ok(),
        git2::ConfigLevel::System => git2::Config::find_system().ok(),
        _ => None,
    }?;
    Some(abbreviate_home(&path))
}

/// Return the config file path for a scalar key, or None if not set anywhere.
fn scalar_source(repo: &git2::Repository, config: &git2::Config, key: &str) -> Option<String> {
    for level in [
        git2::ConfigLevel::Local,
        git2::ConfigLevel::Worktree,
        git2::ConfigLevel::Global,
        git2::ConfigLevel::XDG,
        git2::ConfigLevel::System,
    ] {
        if config
            .open_level(level)
            .ok()
            .and_then(|c| c.get_string(key).ok())
            .is_some()
        {
            return config_level_path(repo, level);
        }
    }
    None
}

/// Return distinct config file paths for a multi-value key, or None if not set.
fn multivar_source(repo: &git2::Repository, config: &git2::Config, key: &str) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    if let Ok(mut entries) = config.multivar(key, None) {
        while let Some(Ok(entry)) = entries.next() {
            if let Some(path) = config_level_path(repo, entry.level()) {
                if !seen.contains(&path) {
                    seen.push(path);
                }
            }
        }
    }
    if seen.is_empty() {
        None
    } else {
        Some(seen.join(", "))
    }
}

/// Read all workon config values for display, returning (key, value, source) triples.
/// `source` is None for values that are just defaults (not set in any config file).
fn read_config_entries(
    repo: &git2::Repository,
    config: &WorkonConfig,
) -> Result<Vec<(String, String, Option<String>)>> {
    let git_config = repo.config().into_diagnostic()?;
    let mut entries = Vec::new();

    let (val, src) = match config.default_branch(None)? {
        Some(val) => (
            val,
            scalar_source(repo, &git_config, "workon.defaultBranch"),
        ),
        None => ("(not set)".to_string(), None),
    };
    entries.push(("workon.defaultBranch".to_string(), val, src));

    let auto_copy = config.auto_copy_untracked(None)?;
    let src = scalar_source(repo, &git_config, "workon.autoCopyUntracked");
    entries.push((
        "workon.autoCopyUntracked".to_string(),
        auto_copy.to_string(),
        src,
    ));

    let (val, src) = match config.pr_format(None) {
        Ok(val) => (val, scalar_source(repo, &git_config, "workon.prFormat")),
        Err(_) => (
            "(invalid)".to_string(),
            scalar_source(repo, &git_config, "workon.prFormat"),
        ),
    };
    entries.push(("workon.prFormat".to_string(), val, src));

    let timeout = config.hook_timeout()?;
    let src = scalar_source(repo, &git_config, "workon.hookTimeout");
    entries.push((
        "workon.hookTimeout".to_string(),
        format!("{}s", timeout.as_secs()),
        src,
    ));

    let patterns = config.copy_patterns()?;
    let src = multivar_source(repo, &git_config, "workon.copyPattern");
    let val = if patterns.is_empty() {
        "(not set)".to_string()
    } else {
        patterns.join(", ")
    };
    entries.push(("workon.copyPattern".to_string(), val, src));

    let excludes = config.copy_excludes()?;
    let src = multivar_source(repo, &git_config, "workon.copyExclude");
    let val = if excludes.is_empty() {
        "(not set)".to_string()
    } else {
        excludes.join(", ")
    };
    entries.push(("workon.copyExclude".to_string(), val, src));

    let protected = config.prune_protected_branches()?;
    let src = multivar_source(repo, &git_config, "workon.pruneProtectedBranches");
    let val = if protected.is_empty() {
        "(not set)".to_string()
    } else {
        protected.join(", ")
    };
    entries.push(("workon.pruneProtectedBranches".to_string(), val, src));

    let hooks = config.post_create_hooks()?;
    let src = multivar_source(repo, &git_config, "workon.postCreateHook");
    let val = if hooks.is_empty() {
        "(not set)".to_string()
    } else {
        hooks.join(", ")
    };
    entries.push(("workon.postCreateHook".to_string(), val, src));

    Ok(entries)
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
