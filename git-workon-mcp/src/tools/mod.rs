//! Tool routes grouped by domain, each adding its own routes to [`crate::server::WorkonServer`].
//!
//! Today: [`annotations`], the review comment/walkthrough store (ADR-039). Future: worktree
//! and stack tools — see `docs/rfc/agent-integration.md` Model C.

pub mod annotations;
