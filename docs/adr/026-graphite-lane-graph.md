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

2. **One lane per stack.** The trunk/primary lane stays leftmost (lane 0); each
   additional sibling stack takes the next lane to the right. Graph width = number
   of concurrent sibling stacks, independent of stack depth.

3. **Exactly one labeled node per row — no connector-only rows.** A converging
   lane's corner is drawn on the fork node's own row (`─╯`, `─┴─╯`, etc.), not on
   a dedicated line. Every row maps to exactly one branch.

4. **Glyph vocabulary unchanged.** `◉` green+bold = active, `◎` = worktree exists,
   `◯` dim = metadata-only. `node_glyph`/`node_label` are reused verbatim.

### Gutter encoding

Each lane column is 2 characters wide. For a node in lane `k` with `N` sibling
lanes closing on its row:

```
(│ | )×k  GLYPH  [─ (┴─)×(N-1) ╯]
```

- Columns 0..k-1: `│ ` (active passthrough) or `  ` (closed/empty)
- Column k: glyph character
- If N > 0: `─`, then (N−1) × `┴─`, then `╯`

### Primary lane selection

Among a node's children, the **primary** child (highest `subtree_activity`,
earliest index on tie) inherits the parent's lane. All other children are
**siblings** and receive new lanes (monotonically assigned). Primary subtrees are
emitted first (recursively), then sibling subtrees in order, then the node itself.

### Example

Trunk `main` with stack `s1→s2→s3` and a sibling single-branch stack `shared`:

```
◯ s3
◎ s2
◎ s1
│ ◯ shared
◉─╯ main
```

A mid-stack fork (node with two children) puts the corner on the fork node's row:

```
◎ b2
◎ b1
│ ◯ c1
◯─╯ a2
◯ a1
◉ main
```

## Implementation

The change is localized to `git-workon/src/display.rs`:

- **Removed**: `flatten_tree`, `flatten_node`, `flatten_tree_filtered`,
  `flatten_node_filtered`, `content_width`.
- **Added**: `LaneRow` struct; `build_lane_rows`, `build_lane_rows_filtered`,
  `build_lane_rows_impl`, `emit_lane_rows` (generic over a visibility predicate);
  `render_gutter`, `lane_gutter_width`, `lane_content_width`.
- **Updated**: `format_tree_lines` and `render_tree` use `build_lane_rows` /
  `build_lane_rows_filtered` instead of `flatten_tree` / `flatten_tree_filtered`.
  Return types and calling conventions are unchanged.

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
