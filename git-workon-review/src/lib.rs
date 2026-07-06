//! Core library for `git-workon-review`, a TUI for reviewing changesets.
//!
//! This library (lib target `workon_review`) holds the review domain: diff parsing,
//! word-diff, line-precise staging, and changeset views. See `docs/rfc/workon-review.md` in
//! the workspace root for the full design.
//!
//! ## Status
//!
//! M2: the diff model ([`model`]), its acquisition from [`workon::Changeset`]s
//! ([`acquire`]), patch synthesis ([`synthesis`]), the apply chokepoint ([`apply`]), whole-file
//! ops ([`file_ops`]), the patch-vs-file-op routing layer ([`ops`]), and the FIFO staging queue
//! ([`queue`]) exist; the refresh coordinator and the round-trip verdict corpus land in later
//! M2 changesets.

pub mod acquire;
pub mod apply;
pub mod error;
pub mod file_ops;
pub mod model;
pub mod ops;
pub mod queue;
pub mod synthesis;
