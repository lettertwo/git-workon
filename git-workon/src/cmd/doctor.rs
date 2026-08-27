//! Doctor command for detecting and repairing workspace issues.
//!
//! Detect and repair workspace issues using git's native `git worktree repair` plus
//! additional workon-specific checks.
//!
//! ### Worktree Checks (per-worktree):
//! - Missing worktree directories (in git list but directory deleted) — fixable with --fix
//! - Broken git links (.git file pointing to non-existent location) — manual fix needed
//! - Worktrees whose upstream branch is gone — informational
//! - Worktrees whose admin (metadata) directory name has gone stale — fixable with --fix
//!
//! ### Dependency Checks (once):
//! - Hook commands not found in PATH (from workon.postCreateHook config)
//! - gh CLI not available (required for PR workflow features)
//! - gh CLI not authenticated (PR features require `gh auth login`)
//! - No git remote configured (PR features require at least one remote)
//!
//! ### Configuration Checks (once):
//! - Renamed config keys (e.g. workon.autoCopyUntracked → workon.autoCopy) — fixable with --fix
//! - Invalid workon.stackModel or workon.stackWorktreeGranularity values
//! - Invalid workon.prFormat (missing required `{number}` placeholder)
//! - workon.defaultBranch set to a branch that does not exist locally
//! - stackModel=graphite but repo not initialized with `gt init`
//! - stackModel=gh-stack but no gh-stack file anywhere, or the `gh-stack` extension missing
//! - Worktrees whose gh-stack admin-dir file isn't linked to the canonical store — fixable
//!   with --fix (plants the link, or merges a pre-existing real file first)
//! - gh-stack files that fail to parse, or whose stacks disagree across worktrees
//! - Both Graphite and gh-stack artifacts present with the model left on `auto`
//!
//! ## Flags:
//! - `--fix` - Automatically repair fixable issues (missing directory entries, renamed keys)
//! - `--dry-run` - Preview fixes without applying

use std::path::{Path, PathBuf};

use log::debug;
use miette::{IntoDiagnostic, Result};
use serde_json::json;
use workon::{
    encode_worktree_name, get_repo, get_worktrees, gh_stack_divergent_stack_numbers,
    gh_stack_readability_errors, gh_stack_worktree_link_status, is_gh_stack_repo,
    is_graphite_active, is_graphite_repo, link_worktree, migrate_worktree, preferred_remote_order,
    relative_worktree_path, rename_worktree_metadata, GhStackLinkStatus, Granularity, StackModel,
    WorkonConfig, WorktreeDescriptor,
};

use crate::cli::Doctor;
use crate::output;

use super::Run;

/// Config keys that were renamed in a breaking change and are no longer read.
/// Each (old_key, new_key) pair is scanned at every config scope.
const RENAMED_SCALAR_KEYS: &[(&str, &str)] = &[("workon.autoCopyUntracked", "workon.autoCopy")];

#[derive(Debug)]
enum IssueKind {
    MissingDirectory,
    BrokenGitLink,
    GoneUpstream,
    HookNotFound {
        hook: String,
        command: String,
    },
    GhNotFound,
    RenamedConfigKey {
        old_key: String,
        new_key: String,
        level: git2::ConfigLevel,
        source: String,
        value: String,
        new_already_set: bool,
    },
    GtNotFound,
    GhNotAuthenticated,
    NoRemote,
    InvalidStackConfig {
        key: String,
        value: String,
        reason: String,
    },
    InvalidConfig {
        key: String,
        value: String,
        reason: String,
    },
    DefaultBranchMissing {
        branch: String,
    },
    GraphiteNotInitialized,
    StaleWorktreeName {
        expected: String,
    },
    GhStackWorktreeNotLinked {
        worktree: String,
        holds_file: bool,
    },
    GhStackExtensionNotFound,
    GhStackNotInitialized,
    GhStackFileUnreadable {
        path: PathBuf,
        reason: String,
    },
    GhStackDivergentStacks {
        number: u64,
    },
    BothStackToolsDetected,
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

    fn config(kind: IssueKind) -> Self {
        Self {
            kind,
            name: None,
            path: None,
        }
    }

    fn fixable(&self) -> bool {
        matches!(
            self.kind,
            IssueKind::MissingDirectory
                | IssueKind::RenamedConfigKey { .. }
                | IssueKind::StaleWorktreeName { .. }
                | IssueKind::GhStackWorktreeNotLinked { .. }
        )
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
            IssueKind::GtNotFound => "gt CLI not found (stack features unavailable)".to_string(),
            IssueKind::GhNotAuthenticated => {
                "gh is not authenticated — run: gh auth login".to_string()
            }
            IssueKind::NoRemote => "no git remote configured (PR features unavailable)".to_string(),
            IssueKind::InvalidStackConfig { key, value, reason } => {
                format!("'{key} = {value}': {reason}")
            }
            IssueKind::InvalidConfig { key, value, reason } => {
                format!("'{key} = {value}': {reason}")
            }
            IssueKind::DefaultBranchMissing { branch } => {
                format!("configured branch '{branch}' not found in repo")
            }
            IssueKind::GraphiteNotInitialized => {
                "stackModel=graphite but repo not gt-initialized — run: gt init".to_string()
            }
            IssueKind::StaleWorktreeName { expected } => {
                format!("admin directory name is stale (expected '{expected}')")
            }
            IssueKind::RenamedConfigKey {
                old_key,
                new_key,
                source,
                value,
                new_already_set,
                ..
            } => {
                if *new_already_set {
                    format!(
                        "'{old_key}' at {source} — renamed to '{new_key}' (already set); run --fix to remove old key"
                    )
                } else {
                    format!(
                        "'{old_key} = {value}' at {source} — renamed to '{new_key}', no longer read; run --fix to migrate"
                    )
                }
            }
            IssueKind::GhStackWorktreeNotLinked {
                worktree,
                holds_file,
            } => {
                if *holds_file {
                    format!(
                        "'{worktree}' has its own gh-stack file, not linked to canonical — run --fix to merge it in"
                    )
                } else {
                    format!("'{worktree}' is not linked to the canonical gh-stack file — run --fix to link it")
                }
            }
            IssueKind::GhStackExtensionNotFound => {
                "gh-stack extension not found (run: gh extension install github/gh-stack)"
                    .to_string()
            }
            IssueKind::GhStackNotInitialized => {
                "stackModel=gh-stack but no gh-stack file found — run: gh stack init".to_string()
            }
            IssueKind::GhStackFileUnreadable { path, reason } => {
                format!("gh-stack file '{}' is unreadable: {reason}", path.display())
            }
            IssueKind::GhStackDivergentStacks { number } => {
                format!(
                    "stack #{number} is defined differently in more than one worktree's gh-stack file"
                )
            }
            IssueKind::BothStackToolsDetected => {
                "both Graphite and gh-stack artifacts are present; set workon.stackModel explicitly to avoid ambiguity".to_string()
            }
        }
    }

    fn kind_str(&self) -> &'static str {
        match self.kind {
            IssueKind::MissingDirectory => "missing_directory",
            IssueKind::BrokenGitLink => "broken_git_link",
            IssueKind::GoneUpstream => "gone_upstream",
            IssueKind::HookNotFound { .. } => "hook_not_found",
            IssueKind::GhNotFound => "gh_not_found",
            IssueKind::RenamedConfigKey { .. } => "renamed_config_key",
            IssueKind::GtNotFound => "gt_not_found",
            IssueKind::GhNotAuthenticated => "gh_not_authenticated",
            IssueKind::NoRemote => "no_remote",
            IssueKind::InvalidStackConfig { .. } => "invalid_stack_config",
            IssueKind::InvalidConfig { .. } => "invalid_config",
            IssueKind::DefaultBranchMissing { .. } => "default_branch_missing",
            IssueKind::GraphiteNotInitialized => "graphite_not_initialized",
            IssueKind::StaleWorktreeName { .. } => "stale_worktree_name",
            IssueKind::GhStackWorktreeNotLinked { .. } => "gh_stack_worktree_not_linked",
            IssueKind::GhStackExtensionNotFound => "gh_stack_extension_not_found",
            IssueKind::GhStackNotInitialized => "gh_stack_not_initialized",
            IssueKind::GhStackFileUnreadable { .. } => "gh_stack_file_unreadable",
            IssueKind::GhStackDivergentStacks { .. } => "gh_stack_divergent_stacks",
            IssueKind::BothStackToolsDetected => "both_stack_tools_detected",
        }
    }
}

impl Run for Doctor {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;
        let worktrees = get_worktrees(&repo)?;
        let config = WorkonConfig::new(&repo)?;
        // Computed once up front: gates the per-worktree link check below and the
        // dependency-section gh-stack checks further down, mirroring the GraphiteNotInitialized
        // gate later in this function.
        let gh_stack_active = matches!(config.stack_model(None), Ok(StackModel::GhStack));

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
                    let mut clean = true;

                    if wt.has_gone_upstream().unwrap_or(false) {
                        debug!("'{}': upstream is gone", name);
                        let issue = Issue::worktree(IssueKind::GoneUpstream, name, path.clone());
                        output::check_warn(name, &issue.message());
                        issues.push(issue);
                        clean = false;
                    }

                    if let Some(expected) = expected_worktree_name(&repo, &path) {
                        if expected != name {
                            debug!("'{}': admin name is stale, expected '{}'", name, expected);
                            let issue = Issue::worktree(
                                IssueKind::StaleWorktreeName {
                                    expected: expected.clone(),
                                },
                                name,
                                path.clone(),
                            );
                            output::check_warn(name, &issue.message());
                            issues.push(issue);
                            clean = false;
                        }
                    }

                    if gh_stack_active {
                        if let GhStackLinkStatus::NotLinked { holds_file } =
                            gh_stack_worktree_link_status(&repo, name)
                        {
                            debug!(
                                "'{}': gh-stack not linked (holds_file={})",
                                name, holds_file
                            );
                            let issue = Issue::worktree(
                                IssueKind::GhStackWorktreeNotLinked {
                                    worktree: name.to_string(),
                                    holds_file,
                                },
                                name,
                                path.clone(),
                            );
                            output::check_warn(name, &issue.message());
                            issues.push(issue);
                            clean = false;
                        }
                    }

                    if clean {
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
            debug!("checking gh auth status");
            if gh_authenticated() {
                debug!("gh auth: ok");
                output::check_pass("gh auth");
            } else {
                debug!("gh not authenticated");
                let issue = Issue::dependency(IssueKind::GhNotAuthenticated);
                output::check_warn("gh auth", &issue.message());
                issues.push(issue);
            }
        } else {
            debug!("gh CLI not found in PATH");
            let issue = Issue::dependency(IssueKind::GhNotFound);
            output::check_fail("gh", "not found in PATH");
            issues.push(issue);
        }

        debug!("checking remote configuration");
        if preferred_remote_order(&repo).is_empty() {
            debug!("no remotes configured");
            let issue = Issue::dependency(IssueKind::NoRemote);
            output::check_warn("remote", &issue.message());
            issues.push(issue);
        } else {
            debug!("remote: ok");
            output::check_pass("remote");
        }

        debug!("checking gt CLI availability");
        if gt_available() {
            debug!("gt CLI: ok");
            output::check_pass("gt");
        } else {
            debug!("gt CLI not found in PATH");
            let issue = Issue::dependency(IssueKind::GtNotFound);
            output::check_warn("gt", "not found in PATH (stack features unavailable)");
            issues.push(issue);
        }

        // gh-stack checks — gated on the effective model (unlike GtNotFound above) so
        // Graphite users, who never touch the gh-stack extension, aren't nagged about it.
        if gh_stack_active {
            debug!("checking gh-stack extension availability");
            if gh_stack_extension_available() {
                debug!("gh-stack extension: ok");
                output::check_pass("gh-stack");
            } else {
                debug!("gh-stack extension not found");
                let issue = Issue::dependency(IssueKind::GhStackExtensionNotFound);
                output::check_warn("gh-stack", &issue.message());
                issues.push(issue);
            }

            if !is_gh_stack_repo(&repo) {
                debug!("stackModel=gh-stack but no gh-stack file found anywhere");
                let issue = Issue::config(IssueKind::GhStackNotInitialized);
                output::check_warn("gh-stack", &issue.message());
                issues.push(issue);
            }

            for (path, reason) in gh_stack_readability_errors(&repo) {
                debug!("gh-stack file unreadable: {}", path.display());
                let issue = Issue {
                    kind: IssueKind::GhStackFileUnreadable {
                        path: path.clone(),
                        reason,
                    },
                    name: None,
                    path: Some(path),
                };
                output::check_fail("gh-stack", &issue.message());
                issues.push(issue);
            }

            for number in gh_stack_divergent_stack_numbers(&repo) {
                debug!("gh-stack #{} is divergent across worktrees", number);
                let issue = Issue::config(IssueKind::GhStackDivergentStacks { number });
                output::check_warn("gh-stack", &issue.message());
                issues.push(issue);
            }
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

        // Configuration section — informational display plus renamed-key detection
        output::status("\nChecking configuration...");
        let config_entries = read_config_entries(&repo, &config)?;
        for (key, value, source) in &config_entries {
            match source {
                Some(src) => output::check_pass(&format!("{key} = {value} ({src})")),
                None => output::check_pass(&format!("{key} = {value}")),
            }
        }

        let git_config = repo.config().into_diagnostic()?;
        for (old_key, new_key) in RENAMED_SCALAR_KEYS {
            for level in [
                git2::ConfigLevel::Local,
                git2::ConfigLevel::Worktree,
                git2::ConfigLevel::Global,
                git2::ConfigLevel::XDG,
                git2::ConfigLevel::System,
            ] {
                if let Some(value) = git_config
                    .open_level(level)
                    .ok()
                    .and_then(|c| c.get_string(old_key).ok())
                {
                    let new_already_set = git_config
                        .open_level(level)
                        .ok()
                        .and_then(|c| c.get_string(new_key).ok())
                        .is_some();
                    let source =
                        config_level_path(&repo, level).unwrap_or_else(|| "(unknown)".to_string());
                    let issue = Issue::config(IssueKind::RenamedConfigKey {
                        old_key: old_key.to_string(),
                        new_key: new_key.to_string(),
                        level,
                        source,
                        value,
                        new_already_set,
                    });
                    output::check_warn(old_key, &issue.message());
                    issues.push(issue);
                }
            }
        }

        // Stack config validation
        if let Err(e) = config.stack_model(None) {
            let value = repo
                .config()
                .ok()
                .and_then(|c| c.get_string("workon.stackModel").ok())
                .unwrap_or_default();
            let issue = Issue::config(IssueKind::InvalidStackConfig {
                key: "workon.stackModel".to_string(),
                value,
                reason: e.to_string(),
            });
            output::check_fail("workon.stackModel", &issue.message());
            issues.push(issue);
        }
        if let Err(e) = config.stack_worktree_granularity(None) {
            let value = repo
                .config()
                .ok()
                .and_then(|c| c.get_string("workon.stackWorktreeGranularity").ok())
                .unwrap_or_default();
            let issue = Issue::config(IssueKind::InvalidStackConfig {
                key: "workon.stackWorktreeGranularity".to_string(),
                value,
                reason: e.to_string(),
            });
            output::check_fail("workon.stackWorktreeGranularity", &issue.message());
            issues.push(issue);
        }

        // prFormat validation — flag invalid value as an issue (parity with stackModel)
        if let Err(e) = config.pr_format(None) {
            let value = repo
                .config()
                .ok()
                .and_then(|c| c.get_string("workon.prFormat").ok())
                .unwrap_or_default();
            let issue = Issue::config(IssueKind::InvalidConfig {
                key: "workon.prFormat".to_string(),
                value,
                reason: e.to_string(),
            });
            output::check_fail("workon.prFormat", &issue.message());
            issues.push(issue);
        }

        // defaultBranch existence check
        if let Some(branch) = config.default_branch(None)? {
            debug!("checking defaultBranch '{branch}' exists");
            let exists = repo.find_branch(&branch, git2::BranchType::Local).is_ok();
            if !exists {
                debug!("defaultBranch '{branch}' not found in repo");
                let issue = Issue::config(IssueKind::DefaultBranchMissing {
                    branch: branch.clone(),
                });
                output::check_fail("workon.defaultBranch", &issue.message());
                issues.push(issue);
            }
        }

        // Graphite-init mismatch: stackModel=graphite but repo not gt-initialized.
        // Only emit when gt is on PATH to avoid double-warning with GtNotFound.
        // is_graphite_active = detect_gt() && is_graphite_repo(), so when gt is available
        // it reduces to is_graphite_repo() — false means repo hasn't been `gt init`-ed.
        if gt_available() {
            if let Ok(StackModel::Graphite) = config.stack_model(None) {
                if !is_graphite_active(&repo) {
                    debug!("stackModel=graphite but repo not gt-initialized");
                    let issue = Issue::config(IssueKind::GraphiteNotInitialized);
                    output::check_warn("graphite", &issue.message());
                    issues.push(issue);
                }
            }
        }

        // Both tools' artifacts present, model left on auto/unset: `StackModel::detect` picks
        // Graphite deterministically (see its docs), but the user tried both tools and might
        // not know which one workon is actually reading. An explicit `workon.stackModel =
        // graphite` silences this — it's read as "I know, and I mean it."
        let stack_model_is_explicit = !matches!(
            repo.config()
                .ok()
                .and_then(|c| c.get_string("workon.stackModel").ok())
                .as_deref(),
            None | Some("auto")
        );
        if !stack_model_is_explicit && is_graphite_repo(&repo) && is_gh_stack_repo(&repo) {
            debug!("both graphite and gh-stack artifacts present, model left on auto");
            let issue = Issue::config(IssueKind::BothStackToolsDetected);
            output::check_warn("stack", &issue.message());
            issues.push(issue);
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
                    if let IssueKind::InvalidStackConfig { key, value, reason }
                    | IssueKind::InvalidConfig { key, value, reason } = &issue.kind
                    {
                        obj["key"] = json!(key);
                        obj["value"] = json!(value);
                        obj["reason"] = json!(reason);
                    }
                    if let IssueKind::DefaultBranchMissing { branch } = &issue.kind {
                        obj["branch"] = json!(branch);
                    }
                    if let IssueKind::StaleWorktreeName { expected } = &issue.kind {
                        obj["expected"] = json!(expected);
                    }
                    if let IssueKind::GhStackWorktreeNotLinked { holds_file, .. } = &issue.kind {
                        obj["holds_file"] = json!(holds_file);
                    }
                    if let IssueKind::GhStackFileUnreadable { reason, .. } = &issue.kind {
                        obj["reason"] = json!(reason);
                    }
                    if let IssueKind::GhStackDivergentStacks { number } = &issue.kind {
                        obj["number"] = json!(number);
                    }
                    if let IssueKind::RenamedConfigKey {
                        old_key,
                        new_key,
                        source,
                        value,
                        new_already_set,
                        ..
                    } = &issue.kind
                    {
                        obj["old_key"] = json!(old_key);
                        obj["new_key"] = json!(new_key);
                        obj["source"] = json!(source);
                        obj["value"] = json!(value);
                        obj["new_already_set"] = json!(new_already_set);
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
                for desc in &fixed {
                    output::success(&format!("  ✓ {desc}"));
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

/// Compute the admin name a worktree at `path` should have, or `None` if `path` isn't
/// inside the workon root.
fn expected_worktree_name(repo: &git2::Repository, path: &Path) -> Option<String> {
    let relative = relative_worktree_path(repo, path)?;
    Some(encode_worktree_name(&relative))
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

/// Returns the filesystem path for a given config level, or None for unsupported levels.
fn level_to_path(repo: &git2::Repository, level: git2::ConfigLevel) -> Option<PathBuf> {
    match level {
        git2::ConfigLevel::Local => Some(repo.path().join("config")),
        git2::ConfigLevel::Worktree => Some(repo.path().join("config.worktree")),
        git2::ConfigLevel::Global => git2::Config::find_global().ok(),
        git2::ConfigLevel::XDG => git2::Config::find_xdg().ok(),
        git2::ConfigLevel::System => git2::Config::find_system().ok(),
        _ => None,
    }
}

/// Returns the abbreviated file path for a given config level.
fn config_level_path(repo: &git2::Repository, level: git2::ConfigLevel) -> Option<String> {
    Some(abbreviate_home(&level_to_path(repo, level)?))
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

    let auto_copy = config.auto_copy(None)?;
    let src = scalar_source(repo, &git_config, "workon.autoCopy");
    entries.push(("workon.autoCopy".to_string(), auto_copy.to_string(), src));

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

    let copy_include_ignored = config.copy_include_ignored(None)?;
    let src = scalar_source(repo, &git_config, "workon.copyIncludeIgnored");
    entries.push((
        "workon.copyIncludeIgnored".to_string(),
        copy_include_ignored.to_string(),
        src,
    ));

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

    let (stack_model_val, src) = match config.stack_model(None) {
        Ok(StackModel::None) => (
            "none".to_string(),
            scalar_source(repo, &git_config, "workon.stackModel"),
        ),
        Ok(StackModel::Graphite) => (
            "graphite".to_string(),
            scalar_source(repo, &git_config, "workon.stackModel"),
        ),
        Ok(StackModel::GhStack) => (
            "gh-stack".to_string(),
            scalar_source(repo, &git_config, "workon.stackModel"),
        ),
        Ok(StackModel::Git) => (
            "git".to_string(),
            scalar_source(repo, &git_config, "workon.stackModel"),
        ),
        Err(_) => (
            "(invalid)".to_string(),
            scalar_source(repo, &git_config, "workon.stackModel"),
        ),
    };
    entries.push(("workon.stackModel".to_string(), stack_model_val, src));

    let (granularity_val, src) = match config.stack_worktree_granularity(None) {
        Ok(Granularity::Stack) => (
            "stack".to_string(),
            scalar_source(repo, &git_config, "workon.stackWorktreeGranularity"),
        ),
        Err(_) => (
            "(invalid)".to_string(),
            scalar_source(repo, &git_config, "workon.stackWorktreeGranularity"),
        ),
    };
    entries.push((
        "workon.stackWorktreeGranularity".to_string(),
        granularity_val,
        src,
    ));

    let stack_auto_track = config.stack_auto_track(None)?;
    let src = scalar_source(repo, &git_config, "workon.stackAutoTrack");
    entries.push((
        "workon.stackAutoTrack".to_string(),
        stack_auto_track.to_string(),
        src,
    ));

    let gt_auto_track = config.gt_auto_track(None)?;
    let src = scalar_source(repo, &git_config, "workon.gtAutoTrack");
    entries.push((
        "workon.gtAutoTrack".to_string(),
        gt_auto_track.to_string(),
        src,
    ));

    let prune_gone = config.prune_gone(None)?;
    let src = scalar_source(repo, &git_config, "workon.pruneGone");
    entries.push(("workon.pruneGone".to_string(), prune_gone.to_string(), src));

    let prune_fetch = config.prune_fetch(None)?;
    let src = scalar_source(repo, &git_config, "workon.pruneFetch");
    entries.push((
        "workon.pruneFetch".to_string(),
        prune_fetch.to_string(),
        src,
    ));

    Ok(entries)
}

/// Fix all fixable issues. Returns a description string for each successfully fixed issue.
fn fix_issues(repo: &git2::Repository, issues: &[Issue]) -> Result<Vec<String>> {
    let mut fixed = Vec::new();
    for issue in issues.iter().filter(|i| i.fixable()) {
        match &issue.kind {
            IssueKind::MissingDirectory => {
                if let Some(name) = &issue.name {
                    debug!("pruning worktree '{}'", name);
                    let worktree = repo.find_worktree(name).into_diagnostic()?;
                    let mut opts = git2::WorktreePruneOptions::new();
                    opts.valid(true);
                    worktree.prune(Some(&mut opts)).into_diagnostic()?;
                    debug!("pruned worktree '{}'", name);
                    fixed.push(format!("Pruned: {name}"));
                }
            }
            IssueKind::StaleWorktreeName { expected } => {
                if let (Some(name), Some(path)) = (&issue.name, &issue.path) {
                    debug!("renaming worktree admin dir '{}' -> '{}'", name, expected);
                    rename_worktree_metadata(repo, name, expected, path).into_diagnostic()?;
                    fixed.push(format!("Renamed admin directory: {name} → {expected}"));
                }
            }
            IssueKind::GhStackWorktreeNotLinked { holds_file, .. } => {
                if let Some(name) = &issue.name {
                    if *holds_file {
                        debug!("migrating gh-stack file for worktree '{}'", name);
                        migrate_worktree(repo, name).into_diagnostic()?;
                        fixed.push(format!("Migrated gh-stack file into canonical: {name}"));
                    } else {
                        debug!("linking gh-stack for worktree '{}'", name);
                        link_worktree(repo, name).into_diagnostic()?;
                        fixed.push(format!("Linked to canonical gh-stack file: {name}"));
                    }
                }
            }
            IssueKind::RenamedConfigKey {
                old_key,
                new_key,
                level,
                source,
                value,
                new_already_set,
            } => {
                if let Some(path) = level_to_path(repo, *level) {
                    match git2::Config::open(&path) {
                        Ok(mut leveled) => {
                            if !new_already_set {
                                debug!("writing {new_key} = {value} to {}", path.display());
                                leveled.set_str(new_key, value).into_diagnostic()?;
                            }
                            debug!("removing {old_key} from {}", path.display());
                            leveled.remove(old_key).into_diagnostic()?;
                            fixed.push(format!("Migrated: {old_key} → {new_key} ({source})"));
                        }
                        Err(e) => {
                            debug!("failed to open config at {}: {}", path.display(), e);
                        }
                    }
                }
            }
            _ => {}
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

/// Check if the gh CLI is authenticated (exit 0 from `gh auth status`).
fn gh_authenticated() -> bool {
    std::process::Command::new("gh")
        .args(["auth", "status"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check if the gt CLI (Graphite) is available.
fn gt_available() -> bool {
    command_in_path("gt")
}

/// Check if the `gh-stack` `gh` extension is installed (`gh extension list` mentions it).
///
/// A subprocess, unlike gh-stack's own stack-state reads elsewhere in this crate — decision 2
/// of the gh-stack handoff forbids shelling out for *stack state*, not for a doctor probe, and
/// `gh_authenticated` above already probes `gh auth status` the same way.
fn gh_stack_extension_available() -> bool {
    std::process::Command::new("gh")
        .args(["extension", "list"])
        .output()
        .map(|out| {
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("gh-stack")
        })
        .unwrap_or(false)
}
