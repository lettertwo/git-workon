# Find Command

`find` locates a worktree using a three-mode strategy: exact match → single fuzzy match → interactive selection. Status filters narrow the candidate set before any name matching occurs.

```mermaid
flowchart TD
    START([git workon find]) --> GET_REPO["get_repo()\nget_worktrees()"]
    GET_REPO --> FILTER["apply status filters\n(AND logic — all must match)"]

    subgraph FILTERS["Status filters (--flag → method)"]
        F1["--dirty  → is_dirty()"]
        F2["--clean  → !is_dirty()"]
        F3["--ahead  → has_unpushed_commits()"]
        F4["--behind → is_behind_upstream()"]
        F5["--gone   → has_gone_upstream()"]
    end

    FILTER --> EMPTY{any worktrees\nleft?}
    EMPTY -->|no| ERR_EMPTY["error: no worktrees\nmatch filters"]
    EMPTY -->|yes| HAS_NAME{name\nargument?}

    HAS_NAME -->|yes| EXACT["scan for exact name match\n(worktree.name() == name)"]
    EXACT -->|found| RETURN_EXACT["return worktree"]

    EXACT -->|not found| FUZZY["fuzzy match:\ncase-insensitive substring\n(name.to_lowercase() contains)"]
    FUZZY --> COUNT{match count}

    COUNT -->|0| ERR_NO_MATCH["error: no matching\nworktree for '<name>'"]
    COUNT -->|1| RETURN_FUZZY["return the single match"]
    COUNT -->|"N > 1"| MULTI_INTERACTIVE{no_interactive?}
    MULTI_INTERACTIVE -->|yes| ERR_MULTI["error: multiple matches,\nuse full name"]
    MULTI_INTERACTIVE -->|no| INTERACTIVE["FuzzySelect from\nmatched worktrees"]

    HAS_NAME -->|no| NO_NAME_INTERACTIVE{no_interactive?}
    NO_NAME_INTERACTIVE -->|yes| ERR_NO_NAME["error: no name provided"]
    NO_NAME_INTERACTIVE -->|no| INTERACTIVE

    INTERACTIVE --> SELECT["dialoguer::FuzzySelect\nwith status indicators"]
    SELECT --> RETURN_SEL["return selected worktree"]

    RETURN_EXACT --> OUTPUT
    RETURN_FUZZY --> OUTPUT
    RETURN_SEL --> OUTPUT
    OUTPUT(["Ok(Some(worktree))\n→ main prints path"])
```

## Status indicators in interactive display

The `display.rs` module formats each worktree row with status indicators derived from `WorktreeDescriptor` methods. Indicators appear before the worktree name:

| Indicator | Meaning | Method |
|---|---|---|
| `*` | Dirty (uncommitted changes) | `is_dirty()` |
| `↑` | Ahead of upstream (unpushed commits) | `has_unpushed_commits()` |
| `↓` | Behind upstream | `is_behind_upstream()` |
| `✗` | Upstream branch gone (deleted on remote) | `has_gone_upstream()` |
| `→` | Currently active worktree | `current_dir` starts_with path |

## Filter semantics

- All active filters are combined with AND logic: `--dirty --ahead` shows only worktrees that are both dirty AND ahead
- Filters operate on the full worktree list before name matching; `git workon find feat --ahead` finds `feat*` worktrees that also have unpushed commits
- `no_interactive` is set automatically by `--json` (see `02-command-dispatch.md`) to prevent prompts in JSON mode

## Key files

- `git-workon/src/cmd/find.rs` — `Run` impl, `matches_filters()`, `select_from_list()`
- `git-workon/src/display.rs` — `worktree_display_row()`, status indicators
- `git-workon-lib/src/worktree.rs` — `WorktreeDescriptor` status methods
