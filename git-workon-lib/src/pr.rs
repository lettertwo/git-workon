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

/// A parsed pull request reference from user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// The PR number extracted from the reference string.
    pub number: u32,
    /// Optional remote name if the reference included one (e.g. `origin/pull/123/head`).
    pub remote: Option<String>,
}

/// PR metadata fetched from the `gh` CLI.
#[derive(Debug, Clone)]
pub struct PrMetadata {
    /// PR number.
    pub number: u32,
    /// PR title.
    pub title: String,
    /// GitHub login of the PR author.
    pub author: String,
    /// Name of the branch that the PR was created from.
    pub head_ref: String,
    /// Name of the branch the PR targets.
    pub base_ref: String,
    /// True if the PR comes from a forked repository.
    pub is_fork: bool,
    /// GitHub login of the fork owner, if this is a fork PR.
    pub fork_owner: Option<String>,
    /// Clone URL of the fork repository, if this is a fork PR.
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

/// Return `Ok(())` if the `gh` CLI is installed and reachable in `PATH`.
///
/// Returns [`PrError::GhNotInstalled`] if `gh` cannot be executed.
pub fn check_gh_available() -> Result<()> {
    std::process::Command::new("gh")
        .arg("--version")
        .output()
        .map_err(|_| PrError::GhNotInstalled)?;
    Ok(())
}

/// Fetch PR metadata for `pr_number` using the `gh` CLI.
///
/// Runs `gh pr view <pr_number> --json ...` and parses the JSON output.
/// Requires `gh` to be authenticated (`gh auth login`).
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

/// Expand all placeholders in `format` using `metadata`.
///
/// Supported placeholders: `{number}`, `{title}`, `{author}`, `{branch}`.
/// Title, author, and branch values are sanitized for use in branch/directory names.
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

/// Priority tier for the shared `upstream → origin → others` remote precedence.
///
/// The single encoding of the precedence ADR-024 prescribes for every remote
/// decision: [`preferred_remote_order`] sorts by it, and
/// `resolve_remote_tracking` (worktree.rs) uses equal tiers to detect
/// ambiguity. Lower is more preferred; all non-special remotes share a tier.
pub fn remote_priority(remote: &str) -> usize {
    match remote {
        "upstream" => 0,
        "origin" => 1,
        _ => 2,
    }
}

/// Returns remotes in preferred order: upstream first, then origin, then all
/// others in configuration order (the sort is stable).
pub fn preferred_remote_order(repo: &Repository) -> Vec<String> {
    let Ok(remotes) = repo.remotes() else {
        return vec![];
    };
    let mut all: Vec<String> = remotes
        .iter()
        .flatten()
        .flatten()
        .map(str::to_string)
        .collect();
    all.sort_by_key(|r| remote_priority(r));
    all
}

/// Select which remote to use for fetching PR refs.
///
/// Priority: `upstream` → `origin` → first available remote.
/// Returns [`PrError::NoRemoteConfigured`] if the repository has no remotes.
pub fn detect_pr_remote(repo: &Repository) -> Result<String> {
    preferred_remote_order(repo)
        .into_iter()
        .next()
        .ok_or_else(|| PrError::NoRemoteConfigured.into())
}

/// Ensure a remote for a fork PR exists, then return its name.
///
/// For non-fork PRs this is equivalent to [`detect_pr_remote`].
/// For fork PRs, a remote named `pr-{number}-fork` is added if it doesn't
/// already exist, pointing at the fork's clone URL.
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

/// Fetch `branch` from `remote_name`, making it available as
/// `refs/remotes/{remote_name}/{branch}`, *unless* the tracking ref already exists.
///
/// Use this for the worktree-creation flow, where a PR branch is fetched exactly once — if
/// `refs/remotes/{remote_name}/{branch}` is already there, there's nothing to gain by
/// re-fetching it. **Do not use this where freshness matters** (e.g. reviewing a PR that may
/// have moved since a prior fetch): a previously-fetched branch is silently left stale, and a
/// long-lived ref like a base branch's is virtually always already present, so it would never
/// be refreshed at all. Use [`fetch_branch_fresh`] there instead.
pub fn fetch_branch(repo: &Repository, remote_name: &str, branch: &str) -> Result<()> {
    // Check if branch already exists locally
    let branch_ref = format!("refs/remotes/{}/{}", remote_name, branch);
    if repo.find_reference(&branch_ref).is_ok() {
        debug!("Branch ref {} already exists", branch_ref);
        return Ok(());
    }

    fetch_branch_fresh(repo, remote_name, branch)
}

/// Fetch `branch` from `remote_name`, making it available as
/// `refs/remotes/{remote_name}/{branch}`, always — force-updating the tracking ref to the
/// remote's current tip even if it already exists locally.
///
/// Use this whenever a stale tracking ref would be wrong to review against (e.g. resolving a
/// PR's head and base for `git workon review`): the refspec is already force (`+`), so this
/// never fails on a diverged tracking ref, it just moves it. For the one-time
/// worktree-creation fetch where an existing ref is fine to leave alone, use [`fetch_branch`].
pub fn fetch_branch_fresh(repo: &Repository, remote_name: &str, branch: &str) -> Result<()> {
    debug!("Fetching branch {} from remote {}", branch, remote_name);

    let refspec = format!(
        "+refs/heads/{}:refs/remotes/{}/{}",
        branch, remote_name, branch
    );

    let remote_url = repo
        .find_remote(remote_name)
        .ok()
        .and_then(|r| r.url().ok().map(str::to_string));
    let auth = get_remote_callbacks(repo, remote_url.as_deref())?;
    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(auth.callbacks());

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

/// Prepare everything needed to create a worktree for PR `pr_number`.
///
/// Orchestrates the complete PR workflow:
/// 1. Checks that `gh` CLI is available
/// 2. Fetches PR metadata via `gh`
/// 3. Sets up a fork remote if the PR is cross-repository
/// 4. Fetches the PR's head branch
/// 5. Formats the worktree name using `pr_format`
///
/// Returns `(worktree_name, remote_ref, base_branch)` ready for `add_worktree`.
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

    /// `fetch_branch` skips a re-fetch once the tracking ref exists, even when the remote has
    /// since moved — right for the one-time worktree-creation fetch, wrong for review, which is
    /// why [`fetch_branch_fresh`] exists. Pins both: the existence short-circuit staying put,
    /// and `fetch_branch_fresh` force-updating past it via the refspec's `+`.
    #[test]
    fn fetch_branch_fresh_updates_stale_tracking_ref_but_fetch_branch_does_not(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use git_workon_fixture::prelude::*;

        let upstream = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;
        let upstream_repo = upstream.repo()?;
        let old_oid = upstream_repo.head()?.peel_to_commit()?.id();

        let local = FixtureBuilder::new().remote("origin", &upstream).build()?;
        let repo = local.repo()?;

        // First fetch: creates the tracking ref at the remote's current tip.
        fetch_branch(repo, "origin", "main")?;
        let tracking_ref = "refs/remotes/origin/main";
        assert_eq!(repo.find_reference(tracking_ref)?.target(), Some(old_oid));

        // The remote moves.
        let sig = git2::Signature::now("Test User", "test@example.com")?;
        let old_commit = upstream_repo.find_commit(old_oid)?;
        let tree = old_commit.tree()?;
        let new_oid = upstream_repo.commit(
            Some("refs/heads/main"),
            &sig,
            &sig,
            "moved on main",
            &tree,
            &[&old_commit],
        )?;
        assert_ne!(new_oid, old_oid);

        // `fetch_branch` sees the ref already exists and leaves it stale.
        fetch_branch(repo, "origin", "main")?;
        assert_eq!(repo.find_reference(tracking_ref)?.target(), Some(old_oid));

        // `fetch_branch_fresh` force-updates it to the remote's new tip.
        fetch_branch_fresh(repo, "origin", "main")?;
        assert_eq!(repo.find_reference(tracking_ref)?.target(), Some(new_oid));

        Ok(())
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
