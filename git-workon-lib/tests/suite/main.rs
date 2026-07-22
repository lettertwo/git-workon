//! Single integration-test harness binary for `git-workon-lib`. Cargo only auto-discovers
//! `tests/*.rs` as separate binaries, not files in subdirectories — declaring each test file as
//! a `mod` here merges them into one binary (one link instead of one per file), cutting build
//! time for the crate's suite.

mod changeset;
mod clone;
mod config;
mod init;
mod resolve;
mod stack;
mod stash;
mod worktree;
