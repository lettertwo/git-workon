//! Core library for `git-workon`, an opinionated git worktree workflow tool.
//!
//! This crate (published as `workon`) provides the building blocks for cloning,
//! initialising, and managing git repositories in a bare-repo-plus-worktrees layout.
//!
//! ## Key types
//!
//! - [`WorktreeDescriptor`] — wraps a git2 `Worktree` with rich metadata methods
//! - [`BranchType`] — controls how a branch is created for a new worktree
//! - [`WorkonConfig`] — reads `workon.*` settings from git config
//! - [`PullRequest`] / [`PrMetadata`] — PR reference parsing and gh CLI integration
//! - [`MoveOptions`] — options for [`move_worktree`]
//! - [`WorkonError`] and its sub-types — all error variants with miette diagnostics
//!
//! ## Key functions
//!
//! | Function | Description |
//! |---|---|
//! | [`clone`] | Clone a remote repository into the worktrees layout |
//! | [`init`] | Initialise a new bare repository with an initial commit |
//! | [`get_repo`] | Discover and open a bare repository from a path |
//! | [`get_worktrees`] | List all worktrees in a repository |
//! | [`add_worktree`] | Create a new worktree (normal, orphan, or detached) |
//! | [`find_worktree`] | Locate a worktree by name or branch |
//! | [`current_worktree`] | Return the worktree containing the current directory |
//! | [`move_worktree`] | Atomically rename a worktree and its branch |
//! | [`copy_untracked`] | Copy local (git-unmanaged) files between worktrees |
//! | [`workon_root`] | Resolve the directory that holds all worktrees |
//!
//! ## Example
//!
//! ```no_run
//! use workon::{get_repo, get_worktrees, add_worktree, BranchType};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let repo = get_repo(None)?;
//!
//! for wt in get_worktrees(&repo)? {
//!     println!("{}: {}", wt.name().unwrap_or("?"), wt.path().display());
//! }
//!
//! let wt = add_worktree(&repo, "my-feature", None, BranchType::Normal, None, false)?;
//! println!("Created: {}", wt.path().display());
//! # Ok(())
//! # }
//! ```

mod checkout;
mod clone;
mod config;
mod convert_to_bare;
mod copy_untracked;
mod default_branch;
mod empty_commit;
mod error;
mod get_remote_callbacks;
mod get_repo;
mod init;
mod r#move;
mod pr;
mod resolve;
mod ssh_config;
mod stack;
mod workon_root;
mod worktree;

pub use crate::checkout::*;
pub use crate::clone::*;
pub use crate::config::*;
pub use crate::convert_to_bare::*;
pub use crate::copy_untracked::*;
pub use crate::default_branch::*;
pub use crate::empty_commit::*;
pub use crate::error::*;
pub use crate::get_remote_callbacks::*;
pub use crate::get_repo::*;
pub use crate::init::*;
pub use crate::pr::*;
pub use crate::r#move::*;
pub use crate::resolve::*;
pub use crate::stack::{
    current_stack, enumerate_stacks, graphite_trunk, group_by_stack, is_graphite_active,
    Granularity, Stack, StackGroup, StackGrouping, StackModel,
};
pub use crate::workon_root::*;
pub use crate::worktree::*;
