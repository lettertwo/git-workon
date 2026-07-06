//! Core library for `git-workon-review`, a TUI for reviewing changesets.
//!
//! This library (lib target `workon_review`) holds the review domain: diff parsing,
//! word-diff, line-precise staging, and changeset views. See `docs/rfc/workon-review.md` in
//! the workspace root for the full design.
//!
//! ## Status
//!
//! M2: the diff model ([`model`]) and its acquisition from [`workon::Changeset`]s
//! ([`acquire`]) exist; synthesis, staging, and refresh land in later M2 changesets.

pub mod acquire;
pub mod error;
pub mod model;
