//! Core library for `git-workon-review`, a TUI for reviewing changesets.
//!
//! This library (lib target `workon_review`) will hold the review domain:
//! diff parsing, word-diff, line-precise staging, and changeset views. See
//! `docs/rfc/workon-review.md` in the workspace root for the full design.
//!
//! ## Status
//!
//! M0 scaffolding only — no review logic exists yet.

pub mod error;
