//! Test fixtures and custom predicates for `git-workon` integration tests.
//!
//! This crate (not published) provides:
//! - [`fixture_builder::FixtureBuilder`] — declarative temporary git repository setup
//! - [`predicates`] — custom predicate types for asserting repository state
//! - [`assert::FixtureAssert`] — chainable `.assert()` extension on `git2::Repository`
//!
//! Import everything via the [`prelude`]:
//!
//! ```
//! use git_workon_fixture::prelude::*;
//! ```

pub mod assert;
pub mod fixture;
pub mod fixture_builder;
pub mod predicates;
pub mod prelude;
