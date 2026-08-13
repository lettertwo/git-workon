//! Encodes a root-relative worktree path into a worktree admin (metadata directory) name.
//!
//! See [ADR-027](../../../docs/adr/027-path-encoded-worktree-names.md). Git discovers
//! worktrees by listing `.bare/worktrees/` exactly one level deep, so the admin name can
//! never contain `/`. Deriving it as a plain basename lets two worktrees whose paths end
//! in the same component collide (`ee/feature-name` and `archive/feature-name` both reduce
//! to `feature-name`). Encoding the full path avoids that: every `/` becomes `~`, a
//! separator `git check-ref-format` rejects, so an encoded name can never alias a real
//! branch and no branch can ever produce one by accident.

use std::path::Path;

use git2::Repository;

use crate::workon_root;

/// Encode a root-relative worktree path into a worktree admin name.
///
/// Replaces every `/` with `~`. Called at the two sites that compute an admin name from a
/// path — [`add_worktree`](crate::add_worktree) and
/// [`move_worktree`](crate::move_worktree) — never anywhere else. Nothing decodes the
/// result back into a path; a stored name is a label, not a key.
pub fn encode_worktree_name(relative_path: &str) -> String {
    relative_path.replace('/', "~")
}

/// Compute `path`'s root-relative path, robust to symlinks on either side.
///
/// `workon_root()` and a worktree's `path()` can disagree on symlink resolution — git
/// canonicalizes the paths it writes into `gitdir`/`commondir`, while a path computed
/// from `workon_root()` may not be (e.g. macOS routes both `/tmp` and `/var/folders`
/// through symlinks). A plain `strip_prefix` fails silently in that case, which would
/// make every path-based lookup miss every worktree under a symlinked root. Canonicalize
/// both sides first, falling back to the original when `canonicalize` fails (e.g. a
/// worktree directory that no longer exists).
///
/// Returns `None` if `path` isn't inside `workon_root()`. The result always uses `/`
/// separators, matching the format `encode_worktree_name` expects.
pub fn relative_worktree_path(repo: &Repository, path: &Path) -> Option<String> {
    let root = workon_root(repo).ok()?;
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let relative = canonical_path.strip_prefix(&canonical_root).ok()?;
    let relative = relative.to_str()?;
    Some(if std::path::MAIN_SEPARATOR == '/' {
        relative.to_string()
    } else {
        relative.replace(std::path::MAIN_SEPARATOR, "/")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_namespaced_path() {
        assert_eq!(encode_worktree_name("ee/feature-name"), "ee~feature-name");
    }

    #[test]
    fn leaves_top_level_name_unchanged() {
        assert_eq!(encode_worktree_name("feature-name"), "feature-name");
    }

    #[test]
    fn encodes_every_separator_in_a_nested_path() {
        assert_eq!(encode_worktree_name("a/b/c"), "a~b~c");
    }
}
