//! Encodes a root-relative worktree path into a worktree admin (metadata directory) name.
//!
//! See [ADR-027](../../../docs/adr/027-path-encoded-worktree-names.md). Git discovers
//! worktrees by listing `.bare/worktrees/` exactly one level deep, so the admin name can
//! never contain `/`. Deriving it as a plain basename lets two worktrees whose paths end
//! in the same component collide (`ee/feature-name` and `archive/feature-name` both reduce
//! to `feature-name`). Encoding the full path avoids that: every `/` becomes `~`, a
//! separator `git check-ref-format` rejects, so an encoded name can never alias a real
//! branch and no branch can ever produce one by accident.

/// Encode a root-relative worktree path into a worktree admin name.
///
/// Replaces every `/` with `~`. Called at the two sites that compute an admin name from a
/// path — [`add_worktree`](crate::add_worktree) and
/// [`move_worktree`](crate::move_worktree) — never anywhere else. Nothing decodes the
/// result back into a path; a stored name is a label, not a key.
pub fn encode_worktree_name(relative_path: &str) -> String {
    relative_path.replace('/', "~")
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
