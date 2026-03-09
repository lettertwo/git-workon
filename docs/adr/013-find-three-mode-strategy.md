# 013 — Three-Mode Find Strategy (Exact → Fuzzy → Interactive)

## Context

Users switch between worktrees frequently. They may know the exact name, a partial name, or nothing at all (they want to browse). Requiring exact names is too rigid; opening interactive selection for every lookup is too slow when the name is known. We needed a single command that handles all three cases gracefully.

## Decision

`find` uses a three-stage strategy applied in order:

1. **Exact match**: if a name argument is given and a worktree's `name()` equals it exactly, return immediately.
2. **Fuzzy match**: if no exact match, search for worktrees whose lowercased name contains the lowercased argument as a substring. If exactly one match is found, return it. If multiple matches are found and `--no-interactive` is set, return an error; otherwise fall through to interactive selection.
3. **Interactive selection**: `dialoguer::FuzzySelect` displays all candidate worktrees with status indicators (`*` dirty, `↑` ahead, `↓` behind, `✗` upstream gone, `→` current). The user selects one.

Status filters (`--dirty`, `--clean`, `--ahead`, `--behind`, `--gone`) are applied before any name matching. All active filters are ANDed together.

`--no-interactive` is set automatically when `--json` is passed (see [ADR-004](004-smart-routing-default-command.md)), preventing prompts in scripted contexts.

## Consequences

- Exact names are zero-overhead; fuzzy names work for most cases; interactive mode is a fallback for exploration.
- The fuzzy match is a simple substring check, not a fuzzy-scoring algorithm — fast and predictable, but not tolerant of typos.
- Filter + fuzzy combining allows power queries like `git workon find feat --ahead` (ahead worktrees whose name contains "feat").

## References

- `docs/diagrams/find-flow.md` — full flowchart
- `git-workon/src/cmd/find.rs` — `Run` impl, `matches_filters()`
- `git-workon/src/display.rs` — status indicators
