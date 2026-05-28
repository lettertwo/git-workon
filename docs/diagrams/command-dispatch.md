# Smart Routing & Command Dispatch

When `git workon <arg>` is called without an explicit subcommand, `main.rs` performs smart routing: PR-like references go to `new`, names matching an existing branch with no worktree go to `new`, everything else goes to `find`. With an explicit subcommand, dispatch is direct.

```mermaid
flowchart TD
    START(["`git workon [args]`"]) --> PARSE["Cli::parse()"]
    PARSE --> HAS_CMD{subcommand\nprovided?}

    HAS_CMD -->|yes| DIRECT["use subcommand directly"]

    HAS_CMD -->|no| HAS_NAME{name\nargument?}
    HAS_NAME -->|no| ROUTE_FIND["route → Find\n(interactive)"]

    HAS_NAME -->|yes| IS_PR["is_pr_reference(name)?"]

    IS_PR -->|yes| PR_EXISTS["worktree already\nexists for PR?"]
    PR_EXISTS -->|yes| ROUTE_FIND3["route → Find(formatted_name)"]
    PR_EXISTS -->|no| ROUTE_NEW_PR["route → New\n(with pr_name pre-filled)"]

    IS_PR -->|no| WT_EXISTS["worktree already\nexists for name?"]
    WT_EXISTS -->|yes| ROUTE_FIND2["route → Find(name)"]
    WT_EXISTS -->|no| BRANCH_EXISTS["local or remote\nbranch exists?"]
    BRANCH_EXISTS -->|yes| ROUTE_NEW_BR["route → New\n(auto-attach branch)"]
    BRANCH_EXISTS -->|no| ROUTE_FIND4["route → Find(name)\n(will error: not found)"]

    DIRECT --> JSON_PROP
    ROUTE_FIND --> JSON_PROP
    ROUTE_FIND2 --> JSON_PROP
    ROUTE_FIND3 --> JSON_PROP
    ROUTE_FIND4 --> JSON_PROP
    ROUTE_NEW_PR --> JSON_PROP
    ROUTE_NEW_BR --> JSON_PROP

    JSON_PROP{"--json\nflag?"}
    JSON_PROP -->|yes| SET_JSON["set json_mode\npropagate to commands:\n• List: json=true\n• Prune: json=true\n• Doctor: json=true\n• Find: no_interactive=true"]
    JSON_PROP -->|no| RUN_CMD

    SET_JSON --> RUN_CMD["cmd.run()"]

    RUN_CMD --> RESULT{Result<Option\nWorktree>}

    RESULT -->|Err| MIETTE["miette fancy\nerror output\n→ stderr"]

    RESULT -->|Ok Some + json_mode| JSON_OUT["serde_json pretty-print\n→ stdout"]
    RESULT -->|Ok Some + text_mode| PATH_OUT["print worktree.path()\n→ stdout"]
    RESULT -->|Ok None| SILENT["(no output —\ncommand already printed)"]
```

## PR reference detection

`is_pr_reference()` and `route_pr_ref_to_command()` in `main.rs`:

1. Parse the name with `parse_pr_reference()` — returns `None` if not a PR ref
2. Load config to get `workon.prFormat`
3. Format the expected worktree name with `format_pr_name()`
4. Check if that worktree already exists via `repo.find_worktree()`
5. If exists → `Find`; if not → `New` (with pre-filled name)

## Branch detection

`route_branch_to_command()` and `branch_exists()` in `main.rs`:

1. Check if a worktree already exists for the name — if so, return `None` (let `Find` handle it)
2. Check for a local branch via `repo.find_branch(name, Local)`
3. Check remote tracking branches by short name: `"origin/feature"` matches `"feature"`
4. If any branch found → `New` (auto-attach, no `--branch` flag needed); otherwise → `None` → `Find`

## JSON propagation details

`--json` is a global flag on `Cli`. After routing, `main.rs` explicitly sets fields on the selected command variant before calling `run()`:

| Command | Effect of `--json` |
|---|---|
| `List` | `list.json = true` → prints JSON array, returns `None` |
| `Prune` | `prune.json = true` → prints JSON result, returns `None` |
| `Doctor` | `doctor.json = true` → prints JSON issues, returns `None` |
| `Find` | `find.no_interactive = true` → errors instead of prompting |
| Others | no change — `Some(worktree)` is JSON-printed by `main` |

## Key files

- `git-workon/src/main.rs` — routing, JSON propagation, output (`route_pr_ref_to_command`, `route_branch_to_command`, `branch_exists`)
- `git-workon/src/cli.rs` — `Cli`, `Cmd`, and all arg structs
- `git-workon-lib/src/pr.rs` — `is_pr_reference()`, `parse_pr_reference()`
