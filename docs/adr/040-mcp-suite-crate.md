# 040: `git-workon-mcp`, One MCP Crate for the Whole Suite

Status: accepted (2026-09-03)

## Context

ADR-039 created `git-workon-annotations` as a lib and named `git-workon-mcp`, the MCP server
that serves the store to a coding agent over stdio, as its second consumer. It left the
server's crate home to this ADR. Two homes were on the table. The first was a feature-gated
`[[bin]]` inside `git-workon-annotations`, reached only with `--features mcp`: the fewest
workspace members, and it dodges the publish blocker the same way any `publish = false`
crate does. The second was a crate of its own. The binary and its `git workon mcp` surface
are suite-scoped, while the annotations crate is one domain: every tool it serves today is
an annotation tool, so the first home costs nothing until a worktree or stack tool needs
`git-workon-lib`, which `git-workon-annotations` has no reason to depend on. A feature-gated
bin also hides its test from a bare `cargo test --workspace`, which is what CI runs.
`docs/rfc/agent-integration.md` (Model C, "Phase 3: MCP Server (New Crate)") already
specified `git-workon-mcp` as its own crate depending on `git-workon-lib`. I took the second
home. Same eight tools as the annotations design, no worktree tools yet.

## Decision

**Own crate, bin-only, `publish = false` + `[package.metadata.dist] dist = false`, reached by
PATH dispatch, never a `Cmd::Mcp`.** The actual publish blocker this decision resolves: a
published crate cannot depend on a `publish = false` crate, and `git-workon-annotations` stays
`publish = false` (its schema and API are still settling). Building `git-workon-mcp` as its
own crate and having `git-workon`'s existing external-subcommand dispatch
(`git-workon/src/dispatch.rs`) exec it from `PATH` for `git workon mcp` keeps the user-visible
surface without the published `git-workon` binary ever taking a compile-time dependency on an
unpublished crate. A built-in `Cmd::Mcp` would look simpler today, but it would permanently
shadow the external dispatch and re-couple the crates the moment either is ready to publish,
so the dispatch route is taken now.

**Tools grouped by domain module, annotations first.** `src/tools/annotations.rs` holds the
eight annotation `#[tool]` fns, their argument structs, and their helpers (repo
discovery, store access, anchor building, JSON encoding). `src/tools/mod.rs` re-exports it and
is where a worktree or stack module would land next; `docs/rfc/agent-integration.md` Model C
is still the plan for those, not yet built here. `src/server.rs` owns `WorkonServer` and its
`ServerHandler` impl; `src/main.rs` only wires stdio and calls `serve`. rmcp's `#[tool_router]`
macro generates its router-building associated function without a visibility keyword by
default, so calling it from a sibling module (`server.rs` constructing a `WorkonServer` whose
router impl lives in `tools/annotations.rs`) needs `#[tool_router(vis = "pub(crate)")]`, the
one macro accommodation this split required. Everything else about the module boundary is
ordinary Rust visibility.

**Transport: rmcp 3.2, minimal features, confined to this one crate.** `default-features =
false, features = ["server", "macros", "transport-io"]`, run on a current-thread tokio
runtime; this pulls in serde derive, schemars, and tokio for this crate only, not for
`git-workon-review` or `git-workon-annotations`. rmcp is the official SDK, tracks MCP protocol
revisions we don't want to hand-roll (a handshake-less transport change landed 2026-07-28),
and its MSRV (1.88) already matches the workspace floor ADR-033 set. Known cost: quarterly
breaking majors, and an open upstream issue where `#[tool_router]` can silently register zero
tools: `tool_router_serves_exactly_eight_tools` asserts the count for exactly that reason.

## Consequences

- The stdio round-trip test (`tests/stdio.rs`) runs under a bare `cargo test --workspace`,
  which is what CI runs; a feature-gated bin would have needed a flag CI does not pass.
- `git-workon-annotations` stays a lib with no bin target and no `rmcp`/`tokio`/`git2`
  dependency: `cargo tree -p git-workon-annotations -e normal` carries none of them.
- `git-workon-mcp`'s own distribution (a second binary through cargo-dist and the homebrew
  formula patch step) is still the open question ADR-033 flagged for a second binary
  generally; this ADR doesn't resolve it.
- The worktree and stack tool set `docs/rfc/agent-integration.md` Model C describes is still
  planned, not built: this ADR ships the annotation tools only.

## References

- `docs/rfc/workon-review.md`: Crate layout and Comments decision rows, and the Agent-loop
  bullet, updated alongside this ADR
- [ADR-039](039-review-annotations-substrate.md): the annotation store this crate's tools
  read and write, and the publish-blocker reasoning this ADR carries forward
- [ADR-033](033-review-crate-workspace-placement.md): the `publish = false` / `dist = false`
  scaffold posture this crate adopts, and the second-binary distribution gap it already
  flagged
- `docs/rfc/agent-integration.md`: Model C, Phase 3, the worktree/stack tool set this crate
  is shaped to grow into next
