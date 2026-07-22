//! Single integration-test harness binary for `git-workon-fixture`. Cargo only auto-discovers
//! `tests/*.rs` as separate binaries, not files in subdirectories — declaring each test file as
//! a `mod` here merges them into one binary (one link instead of one per file), cutting build
//! time for the crate's suite.

mod fixture_builder;
mod index_state;
mod metadata;
mod pr_info;
