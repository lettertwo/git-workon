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
//! PR branches are fetched automatically if not present locally:
//! ```text
//! git fetch <remote> +refs/pull/{number}/head:refs/remotes/<remote>/pr/{number}
//! ```
//!
//! The `+` forces the fetch even if not fast-forward, ensuring we always get the latest PR state.
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
//! ## Future Enhancements
//!
//! TODO: Support PR format variables {title}, {author} (requires gh CLI integration)
//! TODO: Handle fork-based PRs (fetch from fork remote)
//! TODO: Integration with gh CLI for PR metadata
//! TODO: Support GitLab merge requests

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

/// Fetch PR refs from the remote
///
/// This fetches the specific PR ref if it doesn't already exist locally.
pub fn fetch_pr_refs(repo: &Repository, remote: &str, pr_number: u32) -> Result<()> {
    // Check if PR ref already exists
    let pr_ref = format!("refs/remotes/{}/pull/{}/head", remote, pr_number);
    if repo.find_reference(&pr_ref).is_ok() {
        debug!("PR ref {} already exists", pr_ref);
        return Ok(());
    }

    debug!("Fetching PR #{} from remote {}", pr_number, remote);

    // Build the refspec for fetching this specific PR
    let refspec = format!(
        "+refs/pull/{}/head:refs/remotes/{}/pull/{}/head",
        pr_number, remote, pr_number
    );

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(get_remote_callbacks()?);

    repo.find_remote(remote)?
        .fetch(
            &[refspec.as_str()],
            Some(&mut fetch_options),
            Some("Fetching PR"),
        )
        .map_err(|e| PrError::FetchFailed {
            remote: remote.to_string(),
            message: e.message().to_string(),
        })?;

    debug!("Successfully fetched PR #{}", pr_number);
    Ok(())
}

/// Format a PR worktree name using the format string
///
/// Replaces `{number}` placeholder with the PR number.
pub fn format_pr_name(format: &str, pr_number: u32) -> String {
    format.replace("{number}", &pr_number.to_string())
}

/// Prepare a PR worktree by fetching refs and determining the branch name
///
/// This handles the complete PR workflow:
/// 1. Detect which remote to use
/// 2. Fetch PR refs if needed
/// 3. Format the worktree name using config
///
/// Returns (worktree_name, remote_ref) for use with add_worktree
pub fn prepare_pr_worktree(
    repo: &Repository,
    pr_number: u32,
    pr_format: &str,
) -> Result<(String, String)> {
    debug!("Preparing PR worktree for PR #{}", pr_number);

    // Detect which remote to use
    let remote = detect_pr_remote(repo)?;
    debug!("Using remote: {}", remote);

    // Fetch the PR refs if needed
    fetch_pr_refs(repo, &remote, pr_number)?;

    // Format the worktree name using the PR format
    let worktree_name = format_pr_name(pr_format, pr_number);
    debug!("Worktree name: {}", worktree_name);

    // Build the remote ref for branching
    let remote_ref = format!("{}/pull/{}/head", remote, pr_number);

    Ok((worktree_name, remote_ref))
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
}
