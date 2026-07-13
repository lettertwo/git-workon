//! Single integration-test harness binary for `git-workon-review`'s PTY suite — kept SEPARATE
//! from `../suite/main.rs` (the rest of the crate's integration tests) because both PTY files
//! are unix-only (`#![cfg(unix)]`, hoisted here since inner attributes are only legal at the
//! binary's crate root) and `#[ignore]`d by default (wall-clock-bound, load-sensitive; see the
//! module doc comments). Run explicitly: `cargo test -p git-workon-review --test pty --
//! --ignored`.

#![cfg(unix)]

mod pty_responsiveness;
mod pty_smoke;
mod pty_support;
