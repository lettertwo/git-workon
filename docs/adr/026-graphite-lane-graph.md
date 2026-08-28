# 026 — Graphite-Style Lane Graph for Stack Rendering

## Context

The previous stack-aware renderer (ADR-023) used an **indented tree** layout:
depth → indentation, fork points rendered with `├─`/`└─`. This produced a visual
inconsistency:

- **Trunk with one stack** → every node flush-left (single-child = no connector),
  so the stack read as a flat list with no visual link to the trunk.
- **Trunk with a stack + sibling** → the first branch of the stack got a `├─` fork
  while its successors got `│ ` passthrough — the same unbroken stack looked like
  a fork at its first member and a continuation elsewhere.

Users familiar with Graphite's own `gt log` output found the indented-tree style
confusing: a stack (Graphite's primary unit) did not render as a consistent vertical
pillar.

## Decision

Switch the renderer to a **lane graph** model, matching Graphite's `gt log` style
(same family as `git log --graph`):

1. **Tip-on-top, trunk at bottom.** Each stack's tip renders at the top; the trunk
   anchors at the bottom.

2. **Column 0 is reserved for bases.** A base is a trunk branch or an ungrouped
   worktree (one with no stack at all); both render flush left, with no leading
   gutter. Every stack member that is not the trunk sits in column 1 or further
   right, so indentation alone signals stack membership. A stack's bottom-most
   member draws the connector down to its parent (its trunk or, for a mid-stack
   fork, the branch it forked from); the rest of that stack stacks vertically
   above it in the same column.

3. **A trunk gathers every stack hanging off it onto its own row**
   (`◉─┴─┴─╯ main`), not one sibling at a time. Graph width = number of
   concurrent stacks on that trunk, independent of any one stack's depth.

4. **Exactly one labeled node per row — no connector-only rows.** A converging
   lane's corner is drawn on the fork node's own row (`─╯`, `─┴─╯`, etc.), not on
   a dedicated line. Every row maps to exactly one branch.

5. **Glyph vocabulary unchanged.** `◉` green+bold = active, `◎` = worktree exists,
   `◯` dim = metadata-only. `node_glyph`/`node_label` are reused verbatim.

### Gutter encoding

Each lane column is 2 characters wide. For a node in lane `k` with `N` lanes
closing on its row:

```
(│ | )×k  GLYPH  [─ (┴─)×(N-1) ╯]
```

- Columns 0..k-1: `│ ` (a lane still open above) or `  ` (closed/empty)
- Column k: glyph character
- If N > 0: `─`, then (N−1) × `┴─`, then `╯`

Lane 0 has no columns before it, so `k=0` renders flush left with nothing
prepended. That is what makes a base's row visually distinct from a stack
member's without a dedicated marker column: the trunk's own lane simply never
appears indented under anything.

### Lane assignment: bases don't share a lane with their children

A base (the root of a rendering tree: a trunk or an ungrouped worktree) never
hands its own lane to a child. Every child of a base opens a lane of its own,
ordered tallest subtree first (see below), and all of them close on the base's
row (rule 3). This is what keeps column 0 reserved for bases per rule 2: nothing
below a base ever occupies lane 0.

Below the base, the previous **primary lane** rule still applies at every fork: a
node hands its own lane down to its **primary** child (the one with the tallest
subtree, tie-broken as below), and stacks vertically above it in that column.
Every other child is a **sibling** that opens a new lane to the right. Primary
subtrees are emitted first (recursively, tip-on-top), then sibling subtrees in
order, then the node itself, which closes whichever siblings forked under it.

### Ordering: tallest subtree leftmost

Both the lane a child opens and a base's own rank among its peers are decided by
`subtree_size` (node count of the subtree, including the node itself), largest
first. A size tie breaks on `subtree_activity` (most-recent activity first), then
on branch name, for determinism. This replaces the previous most-active-subtree
rule: activity now only matters as a tiebreaker once size is equal.

Ungrouped worktrees sort after every trunk, never interleaved with stacks by
activity; among themselves they still sort by most-recent activity, as before.

### Example

Trunk `main` with stack `s1→s2→s3` and a sibling single-branch stack `shared`:

```
  ◯ s3
  ◎ s2
  ◎ s1
  │ ◯ shared
◉─┴─╯ main
```

A mid-stack fork (node with two children) puts the corner on the fork node's row:

```
  ◎ b2
  ◎ b1
  │ ◯ c1
  ◯─╯ a2
  ◯ a1
◉─╯ main
```

## Implementation

The change is localized to `git-workon/src/display.rs`:

- **Removed**: `flatten_tree`, `flatten_node`, `flatten_tree_filtered`,
  `flatten_node_filtered`, `content_width`.
- **Added**: `LaneRow` struct; `build_lane_rows`, `build_lane_rows_filtered`,
  `build_lane_rows_impl`, `emit_lane_rows` (generic over a visibility predicate,
  and over whether the node it's emitting is a base); `render_gutter`,
  `lane_gutter_width`, `lane_content_width`; `node_order` (the shared
  tallest-subtree-first, then most-recent-activity, then branch-name comparator
  used both for a base's own rank in the forest and for lane assignment among a
  node's children); `subtree_size`.
- **Updated**: `format_tree_lines` and `render_tree` use `build_lane_rows` /
  `build_lane_rows_filtered` instead of `flatten_tree` / `flatten_tree_filtered`.
  Return types and calling conventions are unchanged. `TreeNode` gains a
  `subtree_size: usize` field (node count of its own subtree, including itself),
  computed by `build_tree`/`build_children` alongside the existing
  `subtree_activity`.

The picker (`render_tree` / `find.rs:select_from_tree`) required no logic change —
every row is still a selectable node (no non-selectable connector rows).

## Non-stack degradation

`--no-stack` / `StackModel::None` continues to use `format_aligned_rows` /
`render_flat` — no glyphs, no lanes. Untouched by this change.

## Consequences

- The old `├─`/`└─` fork connectors are gone. Integration tests that asserted on
  those symbols were updated to assert on `─╯` / `│ ` / `─┴─╯` instead.
- The interactive `find` picker: cursor initialization and navigation are unchanged
  (every row is selectable); the only difference is visual (lane lines instead of
  indented-tree lines). Navigation tests that relied on specific cursor positions
  (e.g., ArrowDown to reach a child) were updated to use ArrowUp where the
  tip-on-top ordering reversed the direction.
- `list --json` output is unaffected (layout-independent).
- ADR-023's `├─`/`└─` section is superseded by this ADR.

## Superseded: reserving lane 0 as a membership marker

An earlier revision of this ADR let the trunk's primary child inherit the
trunk's own lane, the same lane a lone ungrouped worktree also renders in. With
exactly one stack and no forks, that meant every row in the stack sat flush
left, indistinguishable from an ungrouped worktree, and the trunk carried no
marker at all. My first attempt at a fix reserved lane 0 as a dedicated
membership column: every row in a stack, including the trunk, carried an
unconditional leading `│ `, regardless of forks; `TreeNode` grew an `in_stack:
bool` field so `render_gutter` knew which rows to mark.

I rejected that output on review. A `│ ` in front of every stack row, trunk
included, reads as a second, redundant gutter next to the fork lanes the graph
already draws, not as a spine.

The rule I replaced it with needs no marker column at all: a base (a trunk or an
ungrouped worktree) never shares its own lane with a child (see "Lane assignment"
above). Column 0 is reserved for bases by construction, not by a flag checked at
render time, so a lone stack with no forks still gets indented on its own — the
trunk's only child opens lane 1 instead of inheriting lane 0, and the trunk
closes that lane on its own row even with nothing to fork. `in_stack` is gone
from `TreeNode`; nothing needs to ask "is this row in a stack" anymore, because
the lane a row occupies already answers that.

```
  ◯ s3
  ◎ s2
  ◎ s1
◉─╯ main
◎ scratch
```

`scratch` is an ungrouped worktree: flush left, same lane 0 the trunk uses,
because both are bases. `main` closes its stack's lane on its own row even
though the stack has no fork, which is what makes `s1`/`s2`/`s3` read as
indented rather than merely offset by an unrelated marker.
