//! Minimal `~/.ssh/config` parser for extracting `IdentityAgent`.
//!
//! libgit2 (and by extension `git2_credentials`) reads the SSH agent socket
//! from `SSH_AUTH_SOCK` only — it does not honour the `IdentityAgent`
//! directive in `~/.ssh/config`. This module bridges that gap by parsing the
//! config file and applying `IdentityAgent` to the process environment before
//! libgit2 opens SSH connections.

use std::path::PathBuf;

use log::{debug, warn};

/// Extract the hostname from a git remote URL.
///
/// Handles both SSH (`git@github.com:...` or `ssh://git@github.com/...`)
/// and HTTPS (`https://github.com/...`) URLs.
pub(crate) fn extract_host_from_url(url: &str) -> Option<String> {
    // SCP-style SSH: git@github.com:user/repo.git
    if let Some(rest) = url.strip_prefix("git@") {
        return rest.split(':').next().map(str::to_string);
    }

    // URL-style: ssh://..., https://..., http://...
    if let Some(rest) = url
        .strip_prefix("ssh://")
        .or_else(|| url.strip_prefix("https://"))
        .or_else(|| url.strip_prefix("http://"))
    {
        // Strip optional user@
        let rest = rest.split_once('@').map_or(rest, |(_, r)| r);
        // Strip path and port
        return rest
            .split('/')
            .next()
            .and_then(|h| h.split(':').next())
            .map(str::to_string);
    }

    None
}

/// Returns true if `pattern` matches `host`.
///
/// Supports exact matches and the `*` wildcard (which matches any sequence of
/// characters, including `.`). The comparison is case-insensitive.
fn host_matches(pattern: &str, host: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern.eq_ignore_ascii_case(host);
    }
    // Simple glob: split on '*' and check prefix/suffix/interior
    let parts: Vec<&str> = pattern.split('*').collect();
    let host_lower = host.to_ascii_lowercase();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        let part_lower = part.to_ascii_lowercase();
        if i == 0 {
            if !host_lower.starts_with(part_lower.as_str()) {
                return false;
            }
            pos = part.len();
        } else {
            match host_lower[pos..].find(part_lower.as_str()) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    // Last part must reach the end of the host string
    if parts.last().is_some_and(|p| !p.is_empty()) {
        pos == host_lower.len()
    } else {
        true
    }
}

/// Parse `~/.ssh/config` and return the `IdentityAgent` value for `host`.
///
/// Returns `None` if:
/// - The config file does not exist or cannot be read
/// - No matching `Host` block contains `IdentityAgent`
/// - The value is `none` or `SSH_AUTH_SOCK` (caller should use `SSH_AUTH_SOCK`)
///
/// The returned path has `~` expanded to the user's home directory.
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

pub(crate) fn resolve_identity_agent(host: &str) -> Option<PathBuf> {
    let config_path = home_dir()?.join(".ssh").join("config");
    let content = std::fs::read_to_string(&config_path).ok()?;
    find_identity_agent_in_config(host, &content)
}

fn find_identity_agent_in_config(host: &str, content: &str) -> Option<PathBuf> {
    let mut in_matching_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Split into key and value
        let (key, value) = split_key_value(trimmed)?;

        if key.eq_ignore_ascii_case("Host") {
            // Each pattern in `Host` is space-separated
            in_matching_block = value.split_whitespace().any(|p| host_matches(p, host));
            continue;
        }

        if !in_matching_block {
            continue;
        }

        if key.eq_ignore_ascii_case("IdentityAgent") {
            return parse_identity_agent_value(value.trim());
        }
    }

    None
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    // SSH config allows both `Key Value` and `Key=Value` syntax
    if let Some(idx) = line.find('=') {
        Some((line[..idx].trim(), line[idx + 1..].trim()))
    } else if let Some(idx) = line.find(char::is_whitespace) {
        Some((line[..idx].trim(), line[idx..].trim()))
    } else {
        None
    }
}

fn parse_identity_agent_value(value: &str) -> Option<PathBuf> {
    // SSH config values may be quoted with double quotes; strip them.
    let value = value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(value);

    match value {
        "none" => None,
        "SSH_AUTH_SOCK" => None,
        path => {
            let expanded = if let Some(rest) = path.strip_prefix("~/") {
                home_dir()?.join(rest)
            } else {
                PathBuf::from(path)
            };
            Some(expanded)
        }
    }
}

/// Read `IdentityAgent` from `~/.ssh/config` for the host in `url` and, if
/// found, update `SSH_AUTH_SOCK` so that libgit2 connects to the right agent.
///
/// This is necessary because libgit2 does not read `~/.ssh/config` for the
/// `IdentityAgent` directive — it only reads `SSH_AUTH_SOCK`.
///
/// # Safety
/// `std::env::set_var` is process-global and not thread-safe. This is
/// intentional: `git-workon` is a single-threaded CLI, and setting
/// `SSH_AUTH_SOCK` once before the first libgit2 SSH connection is safe.
pub(crate) fn apply_identity_agent(url: &str) {
    let Some(host) = extract_host_from_url(url) else {
        debug!("ssh_config: could not extract host from URL, skipping IdentityAgent lookup");
        return;
    };

    debug!("ssh_config: looking up IdentityAgent for host {}", host);

    let Some(agent_path) = resolve_identity_agent(&host) else {
        debug!("ssh_config: no IdentityAgent configured for {}", host);
        return;
    };

    if !agent_path.exists() {
        warn!(
            "ssh_config: IdentityAgent socket {} does not exist",
            agent_path.display()
        );
    }

    debug!("ssh_config: setting SSH_AUTH_SOCK={}", agent_path.display());

    // SAFETY: see doc comment above.
    unsafe {
        std::env::set_var("SSH_AUTH_SOCK", &agent_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_host_from_url ---

    #[test]
    fn test_extract_host_scp_style() {
        assert_eq!(
            extract_host_from_url("git@github.com:user/repo.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn test_extract_host_ssh_url() {
        assert_eq!(
            extract_host_from_url("ssh://git@github.com/user/repo.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn test_extract_host_https() {
        assert_eq!(
            extract_host_from_url("https://github.com/user/repo.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn test_extract_host_https_with_port() {
        assert_eq!(
            extract_host_from_url("https://github.com:443/user/repo.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn test_extract_host_unknown_scheme() {
        assert_eq!(extract_host_from_url("/local/path/repo.git"), None);
    }

    // --- host_matches ---

    #[test]
    fn test_host_matches_exact() {
        assert!(host_matches("github.com", "github.com"));
        assert!(!host_matches("github.com", "gitlab.com"));
    }

    #[test]
    fn test_host_matches_wildcard_star() {
        assert!(host_matches("*", "github.com"));
        assert!(host_matches("*", "anything"));
    }

    #[test]
    fn test_host_matches_prefix_wildcard() {
        assert!(host_matches("*.example.com", "foo.example.com"));
        assert!(!host_matches("*.example.com", "example.com"));
    }

    #[test]
    fn test_host_matches_case_insensitive() {
        assert!(host_matches("GitHub.com", "github.com"));
    }

    // --- find_identity_agent_in_config ---

    #[test]
    fn test_exact_host_match() {
        let config = "\
Host github.com
  IdentityAgent ~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock
";
        let result = find_identity_agent_in_config("github.com", config);
        let home = home_dir().unwrap();
        assert_eq!(
            result,
            Some(home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"))
        );
    }

    #[test]
    fn test_wildcard_host_match() {
        let config = "\
Host *
  IdentityAgent ~/agent.sock
";
        let result = find_identity_agent_in_config("github.com", config);
        let home = home_dir().unwrap();
        assert_eq!(result, Some(home.join("agent.sock")));
    }

    #[test]
    fn test_multi_pattern_host() {
        let config = "\
Host github.com gitlab.com
  IdentityAgent ~/agent.sock
";
        let home = home_dir().unwrap();
        assert_eq!(
            find_identity_agent_in_config("github.com", config),
            Some(home.join("agent.sock"))
        );
        assert_eq!(
            find_identity_agent_in_config("gitlab.com", config),
            Some(home.join("agent.sock"))
        );
        assert_eq!(find_identity_agent_in_config("bitbucket.org", config), None);
    }

    #[test]
    fn test_identity_agent_none() {
        let config = "\
Host github.com
  IdentityAgent none
";
        assert_eq!(find_identity_agent_in_config("github.com", config), None);
    }

    #[test]
    fn test_identity_agent_ssh_auth_sock() {
        let config = "\
Host github.com
  IdentityAgent SSH_AUTH_SOCK
";
        assert_eq!(find_identity_agent_in_config("github.com", config), None);
    }

    #[test]
    fn test_no_matching_host() {
        let config = "\
Host gitlab.com
  IdentityAgent ~/agent.sock
";
        assert_eq!(find_identity_agent_in_config("github.com", config), None);
    }

    #[test]
    fn test_no_identity_agent_directive() {
        let config = "\
Host github.com
  User git
  IdentityFile ~/.ssh/id_rsa
";
        assert_eq!(find_identity_agent_in_config("github.com", config), None);
    }

    #[test]
    fn test_comments_and_blank_lines_ignored() {
        let config = "\
# This is a comment

Host github.com
  # Another comment
  IdentityAgent ~/agent.sock
";
        let home = home_dir().unwrap();
        assert_eq!(
            find_identity_agent_in_config("github.com", config),
            Some(home.join("agent.sock"))
        );
    }

    #[test]
    fn test_key_equals_value_syntax() {
        let config = "\
Host=github.com
  IdentityAgent=~/agent.sock
";
        let home = home_dir().unwrap();
        assert_eq!(
            find_identity_agent_in_config("github.com", config),
            Some(home.join("agent.sock"))
        );
    }

    #[test]
    fn test_absolute_path() {
        let config = "\
Host github.com
  IdentityAgent /run/user/1000/agent.sock
";
        assert_eq!(
            find_identity_agent_in_config("github.com", config),
            Some(PathBuf::from("/run/user/1000/agent.sock"))
        );
    }

    #[test]
    fn test_quoted_path_value() {
        let config = "\
Host github.com
  IdentityAgent \"~/Library/Group Containers/agent.sock\"
";
        let home = home_dir().unwrap();
        assert_eq!(
            find_identity_agent_in_config("github.com", config),
            Some(home.join("Library/Group Containers/agent.sock"))
        );
    }

    #[test]
    fn test_first_matching_block_wins() {
        let config = "\
Host github.com
  IdentityAgent ~/first.sock

Host *
  IdentityAgent ~/second.sock
";
        let home = home_dir().unwrap();
        assert_eq!(
            find_identity_agent_in_config("github.com", config),
            Some(home.join("first.sock"))
        );
    }
}
