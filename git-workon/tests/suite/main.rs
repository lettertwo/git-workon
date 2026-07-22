//! Single integration-test harness binary for `git-workon`. Cargo only auto-discovers
//! `tests/*.rs` as separate binaries, not files in subdirectories — declaring each test file as
//! a `mod` here merges them into one binary (one link instead of one per file), cutting build
//! time for the crate's suite.
//!
//! `move` is a reserved keyword, so `tests/suite/move.rs` is declared via the raw-identifier
//! form `mod r#move;` — it still resolves to the same file.

mod checkout;
mod clone;
mod completions;
mod copy;
mod dispatch;
mod doctor;
mod find;
mod hooks;
mod init;
mod list;
mod r#move;
mod new;
mod pr;
mod prune;
mod shell_init;
