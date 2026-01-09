//! Stacked diff workflow support.
//!
//! Compatibility with stacked diff workflows and tools:
//! - git-branchless
//! - Graphite CLI
//! - Sapling
//! - spr (Stacked Pull Requests)
//!
//! ## Potential Features:
//! - Create worktrees for individual stack levels
//! - Track branch dependencies in metadata
//! - Add `--parent` flag to `new` command for stacked branches
//! - Visualize branch stacks across worktrees

// TODO: Design stacked diff worktree creation API
// TODO: Add parent branch tracking to WorktreeDescriptor
// TODO: Implement stack visualization
