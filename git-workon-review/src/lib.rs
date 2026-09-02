//! Core library for `git-workon-review`, a TUI for reviewing changesets.
//!
//! This library (lib target `workon_review`) holds the review domain: diff parsing,
//! word-diff, line-precise staging, and changeset views. See `docs/rfc/workon-review.md` in
//! the workspace root for the full design.
//!
//! ## Status
//!
//! The diff model ([`model`]), its acquisition from [`workon::Changeset`]s
//! ([`acquire`]), patch synthesis ([`synthesis`]), the apply chokepoint ([`apply`]), whole-file
//! ops ([`file_ops`]), the patch-vs-file-op routing layer ([`ops`]), the FIFO staging queue
//! ([`queue`]), and the refresh generation coordinator ([`refresh`]) exist; the round-trip
//! verdict corpus exists too, in `tests/suite/roundtrip_corpus.rs`.

pub mod acquire;
pub mod align;
pub mod app;
pub mod apply;
pub mod clipboard;
pub mod config;
pub mod editor;
pub mod error;
pub mod file_ops;
pub mod highlight;
pub mod icons;
pub mod keymap;
pub mod model;
pub mod ops;
pub mod outline;
pub mod probe_cache;
pub mod prompt;
pub mod queue;
pub mod refresh;
pub mod render;
pub mod scope;
pub mod search;
pub mod source;
pub mod stage_op;
pub mod summary;
pub mod synthesis;
pub mod terminal_query;
pub mod theme;
pub mod wordiff;
pub mod wrap;
