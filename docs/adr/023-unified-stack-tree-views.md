# 023 — Unified Stack-Tree Views for `list` and `find`

## Context

After shipping the initial stack-aware `list` (ADR-013 era, updated through commit `559bd01`), the
output had two separate sections — `Stack (trunk: X)` and `Ungrouped` — rendered by different code
paths with duplicated indicator-coloring logic. The `find` picker was still entirely flat (no tree
structure). `Stack.diffs` stored a flat BFS slice that could not represent branching stacks; the
parent→child edges existed transiently inside `graphite.rs` but were discarded.

## Decision

### One unified list

`list` and `find` both render the same graphite-style tree. There are no `Stack` / `Ungrouped`
section headers; instead each trunk is a single root node with stacks hanging underneath it, and
untracked worktrees appear at the root level. All roots are sorted by most-recent activity in
their subtree.

### `◉` / `◯` glyphs + connector lines

- `◉` — diff/branch that **has a checked-out worktree**
- `◯` — diff/branch that exists only in Graphite metadata (no worktree)
- `├─` / `└─` at fork points; linear (single-child) chains use `│` continuation without
  increasing visual indentation.
- `← here` on the active worktree's row (list only; the cursor serves that role in find).
- Row label is the branch/diff name; a dim `./path` annotation appears only when the worktree
  directory differs from the branch name.

### `Stack.parents` surfaces the tree structure

`Stack` gains a `parents: HashMap<String, String>` field (diff → parent, parent may be the trunk).
Both `graphite::enumerate_stacks` and `graphite::current_stack` populate it. This replaces the
transient per-call parent map that was previously discarded. `list --json` gains an additive
`parents` object in each stack entry (backward-compatible; `diffs`/`checkouts` unchanged).

### Shared renderer

`build_tree` + `format_tree_lines` in `display.rs` build and render the forest for both commands.
The indicator-coloring logic that was duplicated between `display.rs` and `list.rs` is consolidated
into a single `format_indicators` helper.

### `find` selects from the full tree; worktree-less diffs route to `New`

The `select_from_list` picker is replaced by `select_from_tree`, which uses `format_tree_lines` to
show the same tree as `list`. Selecting a `◉` node returns its worktree directly. Selecting a `◯`
node whose stack already has a worktree returns that worktree (granularity=Stack semantics).
Selecting a `◯` node whose stack has **no** worktree constructs a `New` command for that branch
name and delegates to it — `find` now mutates in this case.

## Why `find` mutates here

The alternatives were: (a) error "no worktree for this diff", (b) route to `New`. Option (a) would
require the user to leave the picker, run `workon new <branch>`, and then re-run find — poor UX
when the tree already surfaced the branch. Smart-routing (ADR-004) already auto-attaches branches
that exist with no worktree; routing `find` to `New` is the natural extension of that principle.
The cost — `find` is no longer read-only — is real but bounded: it only fires for `◯` nodes in
stacks with no worktree, and is subject to all of `New`'s normal guards (hooks, copy, etc.).

## Non-stack degradation

`--no-stack` or `StackModel::None` falls back to the original flat `format_aligned_rows` renderer
with no glyphs or connector lines.

## Consequences

- `Stack` is a breaking struct change (new `parents` field); downstream users of the `workon`
  crate as a library will need to add the field to any struct literals.
- `list --json` gains `parents` in each stack object. `diffs` and `checkouts` are unchanged.
- The old `Stack (trunk:)` / `Ungrouped` section headers are gone.
- `find` may now create worktrees (for `◯` diffs with no worktree in their stack).
