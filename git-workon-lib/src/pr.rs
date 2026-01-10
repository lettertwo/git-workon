//! Pull request support for creating worktrees from PR references.
//!
//! This module enables creating worktrees directly from pull request references,
//! making it easy to review PRs in isolated worktrees.
//!
//! ## PR Reference Parsing
//!
//! Supports multiple PR reference formats:
//! - `#123` - GitHub shorthand (most common)
//! - `pr#123` or `pr-123` - Explicit PR references
//! - `https://github.com/owner/repo/pull/123` - Full GitHub PR URL
//! - `origin/pull/123/head` - Direct remote ref (less common)
//!
//! Parsing is lenient - if it looks like a PR reference, we'll try to extract the number.
//!
//! ## Smart Routing
//!
//! The CLI's smart routing (in main.rs) automatically detects PR references:
//! ```bash
//! git workon #123        # Routes to `new` command with PR reference
//! git workon pr#123      # Same - creates PR worktree
//! git workon feature     # Routes to `find` command (not a PR)
//! ```
//!
//! ## Remote Detection Algorithm
//!
//! To fetch PRs, we need to determine which remote to use. The detection strategy:
//! 1. Check for `upstream` remote (common in fork workflows)
//! 2. Fall back to `origin` remote (most common)
//! 3. Use first available remote (rare, but handles edge cases)
//!
//! This handles both direct repository workflows and fork-based workflows.
//!
//! ## Auto-Fetch Strategy
//!
//! PR branches are fetched automatically using gh CLI metadata:
//! ```text
//! git fetch <remote> +refs/heads/{branch}:refs/remotes/<remote>/{branch}
//! ```
//!
//! Where `{branch}` is the actual branch name from the PR (obtained via gh CLI).
//! The `+` forces the fetch even if not fast-forward, ensuring we always get the latest PR state.
//!
//! For fork PRs, a fork remote is automatically added and the branch is fetched from it.
//! For non-fork PRs, the branch is fetched from the detected remote (origin/upstream).
//!
//! ## Worktree Naming
//!
//! Worktree names are generated from `workon.prFormat` config (default: `pr-{number}`):
//! - `pr-123` (default format)
//! - `#123` (if configured with `#{number}`)
//! - `pull-123` (if configured with `pull-{number}`)
//!
//! The format must contain `{number}` placeholder.
//!
//! ## Example Usage
//!
//! ```bash
//! # Create worktree for PR #123 (auto-detects remote, auto-fetches)
//! git workon #123
//!
//! # Explicit PR reference
//! git workon new pr#456
//!
//! # From GitHub URL
//! git workon new https://github.com/user/repo/pull/789
//!
//! # Configure custom naming
//! git config workon.prFormat "review-{number}"
//! git workon #123  # Creates worktree named "review-123"
//! ```
//!
//! ## gh CLI Integration
//!
//! PR support integrates with gh CLI for rich metadata:
//! - **Format placeholders**: {number}, {title}, {author}, {branch}
//! - **Fork support**: Auto-adds fork remotes and fetches fork branches
//! - **Metadata**: Fetches PR title, author, branch names, and state
//! - **Validation**: Checks PR exists before creating worktree

use git2::{FetchOptions, Repository};
use log::debug;

use crate::{
    error::{PrError, Result},
    get_remote_callbacks,
};

/// Represents a pull request reference
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u32,
    pub remote: Option<String>,
}

/// PR metadata fetched from gh CLI
#[derive(Debug, Clone)]
pub struct PrMetadata {
    pub number: u32,
    pub title: String,
    pub author: String,
    pub head_ref: String,
    pub base_ref: String,
    pub is_fork: bool,
    pub fork_owner: Option<String>,
    pub fork_url: Option<String>,
}

/// Parse a PR reference from user input
///
/// Supported formats:
/// - `#123` - GitHub shorthand
/// - `pr#123` or `pr-123` - Explicit PR references
/// - `https://github.com/owner/repo/pull/123` - GitHub PR URL
/// - `origin/pull/123/head` - Direct remote ref
///
/// Returns `Ok(None)` if the input is not a PR reference.
/// Returns `Ok(Some(PullRequest))` if successfully parsed.
/// Returns `Err` if the input looks like a PR reference but is malformed.
pub fn parse_pr_reference(input: &str) -> Result<Option<PullRequest>> {
    // Try #123 format
    if let Some(num_str) = input.strip_prefix('#') {
        return parse_number(num_str, input).map(|num| {
            Some(PullRequest {
                number: num,
                remote: None,
            })
        });
    }

    // Try pr#123 format
    if let Some(num_str) = input.strip_prefix("pr#") {
        return parse_number(num_str, input).map(|num| {
            Some(PullRequest {
                number: num,
                remote: None,
            })
        });
    }

    // Try pr-123 format
    if let Some(num_str) = input.strip_prefix("pr-") {
        return parse_number(num_str, input).map(|num| {
            Some(PullRequest {
                number: num,
                remote: None,
            })
        });
    }

    // Try GitHub URL: https://github.com/owner/repo/pull/123
    if input.contains("github.com") && input.contains("/pull/") {
        return parse_github_url(input);
    }

    // Try remote ref format: origin/pull/123/head
    if input.contains("/pull/") && input.ends_with("/head") {
        return parse_remote_ref(input);
    }

    // Not a PR reference
    Ok(None)
}

/// Helper to parse a number string
fn parse_number(num_str: &str, original_input: &str) -> Result<u32> {
    num_str.parse::<u32>().map_err(|_| {
        PrError::InvalidReference {
            input: original_input.to_string(),
        }
        .into()
    })
}

/// Parse GitHub PR URL
fn parse_github_url(url: &str) -> Result<Option<PullRequest>> {
    // Extract the PR number from URL like: https://github.com/owner/repo/pull/123
    let parts: Vec<&str> = url.split('/').collect();

    // Find "pull" in the path and get the number after it
    for (i, &part) in parts.iter().enumerate() {
        if part == "pull" && i + 1 < parts.len() {
            let num_str = parts[i + 1];
            let number = parse_number(num_str, url)?;
            return Ok(Some(PullRequest {
                number,
                remote: None,
            }));
        }
    }

    Err(PrError::InvalidReference {
        input: url.to_string(),
    }
    .into())
}

/// Parse remote ref format: origin/pull/123/head
fn parse_remote_ref(ref_str: &str) -> Result<Option<PullRequest>> {
    // Format: remote/pull/number/head
    let parts: Vec<&str> = ref_str.split('/').collect();

    if parts.len() >= 4 && parts[parts.len() - 3] == "pull" && parts[parts.len() - 1] == "head" {
        let num_str = parts[parts.len() - 2];
        let number = parse_number(num_str, ref_str)?;
        return Ok(Some(PullRequest {
            number,
            remote: None,
        }));
    }

    Err(PrError::InvalidReference {
        input: ref_str.to_string(),
    }
    .into())
}

/// Check if gh CLI is available
pub fn check_gh_available() -> Result<()> {
    std::process::Command::new("gh")
        .arg("--version")
        .output()
        .map_err(|_| PrError::GhNotInstalled)?;
    Ok(())
}

/// Fetch PR metadata using gh CLI
pub fn fetch_pr_metadata(pr_number: u32) -> Result<PrMetadata> {
    // Ensure gh is available
    check_gh_available()?;

    // Fetch PR metadata with single gh command
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "number,title,author,headRefName,baseRefName,isCrossRepository,headRepository",
        ])
        .output()
        .map_err(|e| PrError::GhFetchFailed {
            message: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PrError::GhFetchFailed {
            message: stderr.to_string(),
        }
        .into());
    }

    // Parse JSON response
    let json_str = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&json_str).map_err(|e| PrError::GhJsonParseFailed {
            message: e.to_string(),
        })?;

    // Extract fields
    let number = json["number"]
        .as_u64()
        .ok_or_else(|| PrError::GhJsonParseFailed {
            message: "Missing 'number' field".to_string(),
        })? as u32;

    let title = json["title"]
        .as_str()
        .ok_or_else(|| PrError::GhJsonParseFailed {
            message: "Missing 'title' field".to_string(),
        })?
        .to_string();

    let author = json["author"]["login"]
        .as_str()
        .ok_or_else(|| PrError::GhJsonParseFailed {
            message: "Missing 'author.login' field".to_string(),
        })?
        .to_string();

    let head_ref = json["headRefName"]
        .as_str()
        .ok_or_else(|| PrError::GhJsonParseFailed {
            message: "Missing 'headRefName' field".to_string(),
        })?
        .to_string();

    let base_ref = json["baseRefName"]
        .as_str()
        .ok_or_else(|| PrError::GhJsonParseFailed {
            message: "Missing 'baseRefName' field".to_string(),
        })?
        .to_string();

    let is_fork = json["isCrossRepository"].as_bool().unwrap_or(false);

    let (fork_owner, fork_url) = if is_fork {
        let owner = json["headRepository"]["owner"]["login"]
            .as_str()
            .ok_or(PrError::MissingForkOwner)?
            .to_string();
        let url = json["headRepository"]["url"]
            .as_str()
            .map(|s| s.to_string());
        (Some(owner), url)
    } else {
        (None, None)
    };

    Ok(PrMetadata {
        number,
        title,
        author,
        head_ref,
        base_ref,
        is_fork,
        fork_owner,
        fork_url,
    })
}

/// Sanitize a string for use in branch/worktree names
fn sanitize_for_branch_name(s: &str) -> String {
    let sanitized = s
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            ' ' | '/' => '-',
            _ => '-',
        })
        .collect::<String>()
        .to_lowercase();

    // Collapse multiple dashes into single dash
    let mut result = String::new();
    let mut last_was_dash = false;
    for c in sanitized.chars() {
        if c == '-' {
            if !last_was_dash {
                result.push(c);
            }
            last_was_dash = true;
        } else {
            result.push(c);
            last_was_dash = false;
        }
    }

    result.trim_matches(|c| c == '-' || c == '_').to_string()
}

/// Format PR name with metadata placeholders
pub fn format_pr_name_with_metadata(format: &str, metadata: &PrMetadata) -> String {
    format
        .replace("{number}", &metadata.number.to_string())
        .replace("{title}", &sanitize_for_branch_name(&metadata.title))
        .replace("{author}", &sanitize_for_branch_name(&metadata.author))
        .replace("{branch}", &sanitize_for_branch_name(&metadata.head_ref))
}

/// Check if a string looks like a PR reference
///
/// This is a quick check used for routing decisions.
pub fn is_pr_reference(input: &str) -> bool {
    parse_pr_reference(input).ok().flatten().is_some()
}

/// Detect which remote to use for fetching PR refs
///
/// Priority: upstream > origin > first remote
pub fn detect_pr_remote(repo: &Repository) -> Result<String> {
    let remotes = repo.remotes()?;

    // Priority: upstream > origin
    for name in &["upstream", "origin"] {
        if remotes.iter().flatten().any(|r| r == *name) {
            debug!("Using remote: {}", name);
            return Ok(name.to_string());
        }
    }

    // Fall back to first remote
    if let Some(first_remote) = remotes.get(0) {
        Ok(first_remote.to_string())
    } else {
        Err(PrError::NoRemoteConfigured.into())
    }
}

/// Add fork remote if needed and return remote name to fetch from
pub fn setup_fork_remote(repo: &Repository, metadata: &PrMetadata) -> Result<String> {
    if !metadata.is_fork {
        // Not a fork - use regular remote
        return detect_pr_remote(repo);
    }

    // Fork PR - need to add fork remote
    let _fork_owner = metadata
        .fork_owner
        .as_ref()
        .ok_or(PrError::MissingForkOwner)?;

    let fork_url = metadata
        .fork_url
        .as_ref()
        .ok_or(PrError::MissingForkOwner)?;

    // Check if fork remote already exists
    let fork_remote_name = format!("pr-{}-fork", metadata.number);

    if repo.find_remote(&fork_remote_name).is_ok() {
        debug!("Fork remote {} already exists", fork_remote_name);
        return Ok(fork_remote_name);
    }

    // Add fork as remote
    debug!("Adding fork remote: {} -> {}", fork_remote_name, fork_url);
    repo.remote(&fork_remote_name, fork_url)
        .map_err(|e| PrError::FetchFailed {
            remote: fork_remote_name.clone(),
            message: format!("Failed to add fork remote: {}", e),
        })?;

    Ok(fork_remote_name)
}

/// Fetch a branch from a remote
///
/// This fetches the specified branch from the remote, making it available
/// as `refs/remotes/{remote}/{branch}` locally.
///
/// This is used for both fork and non-fork PRs to fetch the actual branch
/// that was used to create the PR (using gh CLI metadata).
pub fn fetch_branch(repo: &Repository, remote_name: &str, branch: &str) -> Result<()> {
    // Check if branch already exists locally
    let branch_ref = format!("refs/remotes/{}/{}", remote_name, branch);
    if repo.find_reference(&branch_ref).is_ok() {
        debug!("Branch ref {} already exists", branch_ref);
        return Ok(());
    }

    debug!("Fetching branch {} from remote {}", branch, remote_name);

    let refspec = format!(
        "+refs/heads/{}:refs/remotes/{}/{}",
        branch, remote_name, branch
    );

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(get_remote_callbacks()?);

    repo.find_remote(remote_name)?
        .fetch(
            &[refspec.as_str()],
            Some(&mut fetch_options),
            Some("Fetching PR branch"),
        )
        .map_err(|e| PrError::FetchFailed {
            remote: remote_name.to_string(),
            message: e.message().to_string(),
        })?;

    debug!("Successfully fetched branch {}", branch);
    Ok(())
}

/// Format a PR worktree name using the format string
///
/// Replaces `{number}` placeholder with the PR number.
pub fn format_pr_name(format: &str, pr_number: u32) -> String {
    format.replace("{number}", &pr_number.to_string())
}

/// Prepare a PR worktree using gh CLI metadata
///
/// This handles the complete PR workflow:
/// 1. Check gh CLI is available
/// 2. Fetch PR metadata from gh
/// 3. Setup fork remote if needed
/// 4. Fetch PR branch
/// 5. Format worktree name using metadata
///
/// Returns (worktree_name, remote_ref, base_branch) for use with add_worktree
pub fn prepare_pr_worktree(
    repo: &Repository,
    pr_number: u32,
    pr_format: &str,
) -> Result<(String, String, String)> {
    debug!("Preparing PR worktree for PR #{}", pr_number);

    // Fetch PR metadata from gh CLI
    let metadata = fetch_pr_metadata(pr_number)?;
    debug!(
        "Fetched metadata: title='{}', author='{}', is_fork={}",
        metadata.title, metadata.author, metadata.is_fork
    );

    // Setup remote and fetch branch
    // For fork PRs: setup fork remote and fetch from it
    // For non-fork PRs: use existing remote (origin/upstream)
    let remote_name = if metadata.is_fork {
        setup_fork_remote(repo, &metadata)?
    } else {
        detect_pr_remote(repo)?
    };

    // Fetch the actual branch from gh CLI metadata (works for both fork and non-fork)
    fetch_branch(repo, &remote_name, &metadata.head_ref)?;

    // Format worktree name using metadata
    let worktree_name = format_pr_name_with_metadata(pr_format, &metadata);
    debug!("Worktree name: {}", worktree_name);

    // Build remote ref using the actual branch from metadata
    let remote_ref = format!("{}/{}", remote_name, metadata.head_ref);
    debug!("Remote ref: {}", remote_ref);

    Ok((worktree_name, remote_ref, metadata.base_ref))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hash_number() {
        let pr = parse_pr_reference("#123").unwrap().unwrap();
        assert_eq!(pr.number, 123);
        assert_eq!(pr.remote, None);
    }

    #[test]
    fn test_parse_pr_hash_number() {
        let pr = parse_pr_reference("pr#456").unwrap().unwrap();
        assert_eq!(pr.number, 456);
        assert_eq!(pr.remote, None);
    }

    #[test]
    fn test_parse_pr_dash_number() {
        let pr = parse_pr_reference("pr-789").unwrap().unwrap();
        assert_eq!(pr.number, 789);
        assert_eq!(pr.remote, None);
    }

    #[test]
    fn test_parse_github_url() {
        let pr = parse_pr_reference("https://github.com/owner/repo/pull/999")
            .unwrap()
            .unwrap();
        assert_eq!(pr.number, 999);
        assert_eq!(pr.remote, None);
    }

    #[test]
    fn test_parse_remote_ref() {
        let pr = parse_pr_reference("origin/pull/111/head").unwrap().unwrap();
        assert_eq!(pr.number, 111);
        assert_eq!(pr.remote, None);
    }

    #[test]
    fn test_parse_regular_branch_name() {
        let result = parse_pr_reference("my-feature-branch").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_invalid_number() {
        let result = parse_pr_reference("#abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_is_pr_reference_true() {
        assert!(is_pr_reference("#123"));
        assert!(is_pr_reference("pr#456"));
        assert!(is_pr_reference("pr-789"));
        assert!(is_pr_reference("https://github.com/owner/repo/pull/999"));
    }

    #[test]
    fn test_is_pr_reference_false() {
        assert!(!is_pr_reference("my-branch"));
        assert!(!is_pr_reference("feature"));
    }

    #[test]
    fn test_format_pr_name() {
        assert_eq!(format_pr_name("pr-{number}", 123), "pr-123");
        assert_eq!(format_pr_name("review-{number}", 456), "review-456");
        assert_eq!(format_pr_name("{number}-test", 789), "789-test");
    }

    #[test]
    fn test_sanitize_branch_name() {
        assert_eq!(sanitize_for_branch_name("Fix Bug #123"), "fix-bug-123");
        assert_eq!(
            sanitize_for_branch_name("Add Feature (v2)"),
            "add-feature-v2"
        );
        assert_eq!(sanitize_for_branch_name("john-smith"), "john-smith");
        assert_eq!(
            sanitize_for_branch_name("Fix: Authentication Issue"),
            "fix-authentication-issue"
        );
        assert_eq!(sanitize_for_branch_name("Test@#$%"), "test");
    }

    #[test]
    fn test_format_with_metadata() {
        let metadata = PrMetadata {
            number: 123,
            title: "Fix Authentication Bug".to_string(),
            author: "john-smith".to_string(),
            head_ref: "feature/fix-auth".to_string(),
            base_ref: "main".to_string(),
            is_fork: false,
            fork_owner: None,
            fork_url: None,
        };

        assert_eq!(
            format_pr_name_with_metadata("pr-{number}", &metadata),
            "pr-123"
        );
        assert_eq!(
            format_pr_name_with_metadata("{number}-{title}", &metadata),
            "123-fix-authentication-bug"
        );
        assert_eq!(
            format_pr_name_with_metadata("{author}/pr-{number}", &metadata),
            "john-smith/pr-123"
        );
        assert_eq!(
            format_pr_name_with_metadata("{branch}-{number}", &metadata),
            "feature-fix-auth-123"
        );
    }

    // Integration tests requiring gh CLI (marked with #[ignore])
    #[test]
    #[ignore]
    fn test_gh_cli_available() {
        check_gh_available().expect("gh CLI should be installed");
    }

    #[test]
    #[ignore]
    fn test_fetch_real_pr_metadata() {
        // Requires gh CLI and auth
        // This test uses a real PR from a public repo (git-workon itself if available)
        // Replace with actual PR number from your repository for testing
        let metadata = fetch_pr_metadata(1).expect("Failed to fetch PR metadata");
        assert_eq!(metadata.number, 1);
        assert!(!metadata.title.is_empty());
        assert!(!metadata.author.is_empty());
    }
}
