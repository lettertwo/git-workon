# 013 — Three-Mode Find Strategy (Exact → Fuzzy → Interactive)

## Context

Users switch between worktrees frequently. They may know the exact name, a partial name, or nothing at all (they want to browse). Requiring exact names is too rigid; opening interactive selection for every lookup is too slow when the name is known. We needed a single command that handles all three cases gracefully.

## Decision

`find` uses a three-stage strategy applied in order:

1. **Exact match**: if a name argument is given and a worktree's `name()` equals it exactly, return immediately.
2. **Fuzzy match**: if no exact match, search for worktrees whose lowercased name contains the lowercased argument as a substring. If exactly one match is found, return it. If multiple matches are found and `--no-interactive` is set, return an error; otherwise fall through to interactive selection.
3. **Stack-member match** (stack-active only): if fuzzy matching finds zero worktrees, search all stacks' `diffs` lists for branches matching the argument. A single matching stack's worktree is returned directly; multiple matches go to interactive selection.
4. **Interactive selection**: `dialoguer::FuzzySelect` displays candidate worktrees as a graphite-style tree (same rendering as `list`) when stack-active, or as a flat aligned list otherwise. The user selects one.

When a metadata-only `◯` diff is selected from the tree and its stack has no checked-out
worktree, `find` delegates to `New` for that branch — creating or attaching the worktree rather
than returning an error. The active worktree is shown as `◉` (green), non-active worktrees as
`◎`, and the picker cursor is a cyan `▶` so it does not conflict with the active-worktree green.
See [ADR-023](023-unified-stack-tree-views.md) for the rationale.

Status filters (`--dirty`, `--clean`, `--ahead`, `--behind`, `--gone`) are applied before any name matching. All active filters are ANDed together.

`--no-interactive` is set automatically when `--json` is passed (see [ADR-004](004-smart-routing-default-command.md)), preventing prompts in scripted contexts.

## Consequences

- Exact names are zero-overhead; fuzzy names work for most cases; stack-member matching resolves branch names not yet checked out; interactive mode is a fallback for exploration.
- The fuzzy match is a simple substring check, not a fuzzy-scoring algorithm — fast and predictable, but not tolerant of typos.
- Filter + fuzzy combining allows power queries like `git workon find feat --ahead` (ahead worktrees whose name contains "feat").
- `find` is not purely read-only when stack-active: selecting a metadata-only `◯` diff with no worktree creates one.

## References

- `docs/diagrams/find-flow.md` — full flowchart
- `git-workon/src/cmd/find.rs` — `Run` impl, `matches_filters()`
- `git-workon/src/display.rs` — status indicators
