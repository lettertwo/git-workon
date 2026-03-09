# New Worktree (Regular Path)

The `new` command creates a worktree for a branch. If the name looks like a PR reference, it delegates to the PR workflow (see `05-pr-workflow.md`). Otherwise it follows this path.

```mermaid
flowchart TD
    START([git workon new]) --> GET_NAME{name provided?}

    GET_NAME -->|yes| NAME_VAL["use provided name"]
    GET_NAME -->|no + no_interactive| ERR_NO_NAME["error: no name\n+ --no-interactive"]
    GET_NAME -->|no + interactive| PROMPT_NAME["dialoguer::Input\n'Branch name'"]
    PROMPT_NAME --> NAME_VAL

    NAME_VAL --> IS_PR{PR ref? no branch type flags?}
    IS_PR -->|yes| PR_PATH["→ PR workflow\n(05-pr-workflow.md)"]
    IS_PR -->|no| BASE_BRANCH

    BASE_BRANCH{base branch source?}
    BASE_BRANCH -->|"--base <branch>"| BASE_EXPLICIT["use --base value"]
    BASE_BRANCH -->|interactive + no name given| BASE_INTERACTIVE["dialoguer::FuzzySelect\nfrom local branches\n+ configured default"]
    BASE_BRANCH -->|otherwise| BASE_CONFIG["workon.defaultBranch\nor None"]

    BASE_EXPLICIT --> BRANCH_TYPE
    BASE_INTERACTIVE --> BRANCH_TYPE
    BASE_CONFIG --> BRANCH_TYPE

    BRANCH_TYPE{branch type flags}
    BRANCH_TYPE -->|"--orphan"| BT_ORPHAN["BranchType::Orphan"]
    BRANCH_TYPE -->|"--detach"| BT_DETACH["BranchType::Detached"]
    BRANCH_TYPE -->|default| BT_NORMAL["BranchType::Normal"]

    BT_NORMAL --> ADD_NORMAL["add_worktree(Normal)"]
    BT_ORPHAN --> ADD_ORPHAN["add_worktree(Orphan)"]
    BT_DETACH --> ADD_DETACH["add_worktree(Detached)"]

    subgraph ADD_NORMAL_DETAIL["add_worktree — Normal internals"]
        N1["find local branch?"]
        N1 -->|yes| N_USE["use local branch ref"]
        N1 -->|no| N2["find remote branch?"]
        N2 -->|yes| N_USE
        N2 -->|no| N3["create new branch\nfrom base_branch or HEAD"]
        N3 --> N_USE
        N_USE --> N_ADD["repo.worktree(name, path, opts)"]
    end

    subgraph ADD_ORPHAN_DETAIL["add_worktree — Orphan internals"]
        O1["repo.worktree(name, path, no ref)"]
        O1 --> O2["write HEAD: ref: refs/heads/<name>"]
        O2 --> O3["remove existing branch ref\nclear working directory\nclear index"]
        O3 --> O4["write empty tree commit\n(no parents = orphan)"]
    end

    subgraph ADD_DETACH_DETAIL["add_worktree — Detached internals"]
        D1["repo.worktree(name, path, no ref)"]
        D1 --> D2["get HEAD commit SHA"]
        D2 --> D3["write SHA directly\nto worktree HEAD file"]
    end

    ADD_NORMAL --> POST
    ADD_ORPHAN --> POST
    ADD_DETACH --> POST

    POST["WorktreeDescriptor created"]
    POST --> COPY{auto_copy_untracked?}
    COPY -->|yes| DO_COPY["copy_files()\nfrom base worktree\nusing copyPattern/copyExclude\n(warns on failure, continues)"]
    COPY -->|no| HOOKS

    DO_COPY --> HOOKS
    HOOKS{hooks skipped?}
    HOOKS -->|yes| DONE
    HOOKS -->|no| RUN_HOOKS["execute_post_create_hooks()\nsequentially, with timeout\n(warns on failure, continues)"]

    RUN_HOOKS --> DONE(["Ok(Some(worktree))"])
```

## Hook environment variables

Each hook command runs via `sh -c` in the new worktree directory with:

| Variable | Value |
|---|---|
| `WORKON_WORKTREE_PATH` | Absolute path to the new worktree |
| `WORKON_BRANCH_NAME` | Branch name (if not detached HEAD) |
| `WORKON_BASE_BRANCH` | Base branch used for creation (if applicable) |

## copy_untracked resolution

The `--copy-untracked` / `--no-copy-untracked` flags override `workon.autoCopyUntracked`. When copying is enabled:

1. Patterns from `workon.copyPattern` are used (default: `**/*` if not set)
2. Patterns from `workon.copyExclude` are excluded
3. Source path is `<workon_root>/<base_branch>` — must exist, otherwise copy is skipped

## Key files

- `git-workon/src/cmd/new.rs` — `Run` impl, interactive prompts, PR detection, copy/hook orchestration
- `git-workon-lib/src/worktree.rs` — `add_worktree()`, `BranchType`, all three branch type implementations
- `git-workon/src/hooks.rs` — `execute_post_create_hooks()`, timeout logic, env vars
- `git-workon-lib/src/copy.rs` — `copy_files()`, glob pattern matching
