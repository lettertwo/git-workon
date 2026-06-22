# 025 — Status Filters Suppress the Stack Tree

## Context

`list` and `find` both render a graphite-style tree when a stack model is active (ADR-023).
The tree includes metadata-only `◯` diffs — branches tracked by Graphite with no checked-out
worktree — so that the full stack structure is always visible.

The five status filters (`--dirty/--clean/--ahead/--behind/--gone`) select worktrees by
checking their working-tree or branch-tracking state. A metadata-only `◯` diff has no working
tree: `is_dirty()` requires a path to open, `has_unpushed_commits()` checks the worktree's
current HEAD, etc. Per-diff filtering is therefore impossible for the most-used filters and
asymmetric at best for the others.

Before this change, applying a filter still re-inflated the tree from two filter-ignoring
sources: (1) `current_stack()` expanded each surviving worktree into its full connected stack,
and (2) the synthetic member-less group injection added every metadata-only stack regardless
of the filter. Users saw all stacked branches even when only a subset matched.

## Decision

When any status filter is active, the stack tree is suppressed and the result is a flat list
of matching worktrees only.

Concretely:

- The metadata-only group injection (`enumerate_stacks()` block) is skipped.
- The human-readable output uses the plain `format_aligned_rows` renderer — the same path
  as `--no-stack` — with no glyphs or connectors.
- `list --json` emits `stacks` derived from the filtered worktrees only (no injected
  member-less groups).
- `find` forces `StackModel::None` for the interactive picker under a filter, routing to the
  flat `render_flat` picker and eliminating the `◯`-routes-to-`New` code path.

The stack-member fuzzy fallback in `find` (which navigates to a worktree that owns a stack
containing the named branch) is unaffected: it searches within the already-filtered worktree
set and returns only worktrees, never metadata-only diffs.

## Alternatives Considered

**Keep full stack structure with context** — show a matching worktree inside its stack tree,
including non-matching trunk anchors and sibling `◯` diffs. Rejected because:
- Interior `◯` nodes have undefined status (no working tree), making the tree a mix of
  "filtered" and "unfiltered" nodes that would be confusing.
- Pruning interior nodes to just the matching worktree breaks connector lines (orphaned
  `├─` / `└─` without a valid parent visible).
- The flat result is unambiguous: every row satisfied the filter.

**Per-diff status computation** — compute status for `◯` diffs from their branch refs alone
(possible for `--ahead/--behind/--gone` but not `--dirty/--clean`). Rejected because the
asymmetry between filter types would produce an inconsistent user model, and the flat output
is the right semantic anyway.

## Consequences

- `workon list --dirty` in a stack repo shows a flat list identical to `--no-stack`, not a
  tree. The tree reappears when no filter is active.
- `workon --dirty` (bare default command, which routes to `find`) opens the flat picker over
  dirty worktrees; it does not print a list.
- `find --dirty --clean` now errors with "Cannot specify both --dirty and --clean filters"
  (previously silently produced an empty result).
- Shared filter logic lives in `git-workon/src/cmd/filter.rs` (`StatusFilter`), used by both
  `list` and `find`.

## References

- ADR-023 — Unified stack-tree views for `list` and `find`
- `git-workon/src/cmd/filter.rs` — `StatusFilter` implementation
- `git-workon/src/cmd/list.rs` — injection and render gate
- `git-workon/src/cmd/find.rs` — `picker_model` selection
