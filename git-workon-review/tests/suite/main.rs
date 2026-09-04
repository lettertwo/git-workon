//! Single integration-test harness binary for `git-workon-review`. Cargo only auto-discovers
//! `tests/*.rs` as separate binaries, not files in subdirectories — declaring each test file as
//! a `mod` here merges them into one binary (one link instead of one per file), cutting build
//! time for the crate's non-PTY suite. See `../pty/main.rs` for why the PTY suite stays a
//! separate second binary.

mod apply;
mod cli;
mod diff_model;
mod file_ops;
mod line_synthesis;
mod roundtrip_corpus;
mod source;
mod treesitter_smoke;
