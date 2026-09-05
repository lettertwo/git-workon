# 039: Annotations, One Substrate for Comments and Walkthroughs

Status: accepted (2026-09-02 plan-mode interview)

## Context

The RFC (`docs/rfc/workon-review.md`) deferred two capabilities behind "the eventual payoff":
review comments fed back to a coding agent via MCP, and a `git workon mcp` bridge to
`git-workon-lib`'s worktree tools. Separately, the user wants an integrated version of the
`/explain-diff` skill: an agent-authored walkthrough that steps a reviewer through a stack,
changeset by changeset. Both need the same three things: a place to anchor content to a
specific line (or a whole changeset) that survives the file changing underneath it, a way for
an agent to write that content over MCP, and a way for the TUI to watch for and render it.

Treating them as two features would mean two anchoring schemes, two stores, and two watchers
for what is structurally the same problem: attach text to a resolved location in a changeset,
author it from either the human or an agent, and keep the TUI's view live as the underlying
diff moves.

## Decision

**One substrate, two uses.** `AnnotationKind::{Comment, TourStop, Chapter}` share one table,
one anchoring scheme, and one store API. A walkthrough is annotations carrying a `tour` name
and `seq` order; a chapter is per-changeset prose with no line anchor.

**Content-hash context anchoring.** An anchor stores the target line's text plus up to 3
context lines each way. It's re-resolved every load, not on write: exact match first (stored
line number, text, and context all agree), then a windowed outward scan for the target text
scored by how much surrounding context still matches, then a whitespace-tolerant repeat of
that scan, else `Orphaned`. A failed resolution renders as "unanchored" (never silently
wrong, never crashes the view). `Orphaned` is derived per load, not persisted: only `Open` and
`Resolved` are real lifecycle states, so a discarded edit that restores the original content
un-orphans the annotation for free, with nothing to reconcile.

**sqlite store at `<commondir>/workon-review/annotations.db`.** `commondir`, not
`repo.path()` (every worktree of a repo shares one store, the same discipline
`git-workon-lib`'s graphite-metadata reader already uses for `.graphite_metadata.db`). WAL +
`busy_timeout(3000)` on the writer; `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` for a
read-only handle. `rusqlite` (bundled) is already a workspace dependency, so this adds no new
runtime.

**New crate `git-workon-annotations`, `publish = false` + `[package.metadata.dist] dist =
false`.** Same posture ADR-033 set for `git-workon-review`: a scaffold-stage crate stays out
of release-plz's auto-publish and cargo-dist's auto-bin-inclusion until it's actually ready to
ship, and `dist = false` is the explicit tripwire for that later flip so removing `publish =
false` alone can't silently start shipping an undesigned binary. Dependencies: `rusqlite`,
`thiserror`, `miette`. Serde-free (plain structs with rusqlite row mapping): the store is a
lib, `git-workon-mcp` (ADR-040) is its second consumer, and the serde-free/git2-free posture
exists so that consumer owns the JSON boundary. No `git2` dependency: the store takes a
`commondir: &Path` the caller resolves, so this crate never needs to open a repository itself.

**Why a new crate at all, when the RFC says "no separate core crate until a second consumer
exists."** That condition is now met. `git workon mcp` is a second consumer of the same
comment/annotation data the TUI reads and writes (a lib both `git-workon-review` and, via
its MCP binary, `git-workon` need). This is exactly the fork the RFC's Agent-loop bullet left
open. Putting the store in `git-workon-review`'s own lib would mean either the CLI depends on
the review crate's whole diff/render surface just to reach a sqlite table, or the annotation
code gets duplicated. A dedicated crate is the smaller dependency edge either way.

**MCP lives in its own crate, `git-workon-mcp` (ADR-040), this store's second consumer.**
This crate stays a lib with no bin target and no `rmcp`/tokio dependency; `git-workon-mcp`
depends on it, owns the JSON boundary, and is reached from `git-workon` via the existing
external-subcommand PATH dispatch (`git-workon/src/dispatch.rs`), not a compile-time
dependency or a built-in `Cmd::Mcp`. ADR-040 covers the crate split, the publish-blocker
reasoning for keeping it off the `git-workon` dependency graph, and the transport choice
(`rmcp` 3.2, minimal features, current-thread tokio).

**Gutter marker, not an edge glyph, for the TUI's annotation indicator (deferred to the read
slice, recorded here for continuity).** Both content edges of a diff row are already claimed
by horizontal-scroll affordances; the gutter's trailing space survives panning, so that's
where the marker goes.

## Consequences

- Comments and walkthrough stops are the same row shape, so the TUI's marker index, overlay,
  and store watcher are built once and serve both, instead of twice.
- The anchoring scheme is a genuine trade: it can misplace an annotation on a heavily
  rewritten line (context match is a heuristic, not a guarantee), but it never crashes or
  silently attaches to the wrong line without saying so: an unresolved anchor always renders
  as `Orphaned`, visibly.
- The store is `rusqlite::Connection`, which is `!Sync`: every consumer (the TUI's
  event-loop thread, each MCP tool call) must own or briefly borrow its own connection, and no
  connection crosses a thread boundary as shared state.
- `git-workon-annotations` has no bin target and never will: `git-workon-mcp` (ADR-040)
  depends on it as a lib, the same relationship `git-workon-review`'s TUI has to it.
- The initial-publish flip for this crate follows the same three steps ADR-033 lists for
  `git-workon-review`: drop `publish = false`, add the crate to `release-plz.toml` with no
  shared `version_group`, and decide `dist = false`'s fate deliberately rather than by
  omission. `git-workon-mcp`'s own distribution (a second binary through cargo-dist and the
  homebrew formula patch step) is a separate decision ADR-033 already flagged as unsolved for
  a second binary generally; this ADR doesn't resolve it.

## References

- `docs/rfc/workon-review.md`: Agent-loop bullet and Comments decision row, updated
  alongside this ADR
- [ADR-008](008-error-handling-strategy.md): the two-layer error pattern this crate's
  `error.rs` follows
- [ADR-033](033-review-crate-workspace-placement.md): the `publish = false` / `dist = false`
  scaffold posture this crate adopts, and the second-binary distribution gap it already
  flagged
- `git-workon-lib/src/stack/graphite.rs`: the `commondir`-based, `READ_ONLY | NO_MUTEX`
  sqlite reader this store's open/open_read_only follow
