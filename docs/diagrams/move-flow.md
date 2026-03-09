# Move Worktree

`move` renames a worktree and its branch atomically, keeping directory structure and git metadata in sync. The operation includes rollback if the directory move fails after the branch rename.

```mermaid
flowchart TD
    START([git workon move]) --> ARG_COUNT{argument\ncount}

    ARG_COUNT -->|1 arg: to| CURRENT["current_worktree()\n(detect from working dir)"]
    CURRENT --> GET_BRANCH["source.branch()?\nfail if detached HEAD"]
    GET_BRANCH --> HAVE_FROM["from = current branch\nto = args[0]"]

    ARG_COUNT -->|2 args: from to| HAVE_EXPLICIT["from = args[0]\nto = args[1]"]

    HAVE_FROM --> SAME_CHECK{from == to?}
    HAVE_EXPLICIT --> SAME_CHECK
    SAME_CHECK -->|yes| ERR_SAME["error: identical names"]
    SAME_CHECK -->|no| OPTIONS["MoveOptions { force }"]

    OPTIONS --> DRY_RUN{--dry-run?}

    DRY_RUN -->|yes| VALIDATE_DRY["find_worktree(from)\nvalidate_move(repo, source, to, opts)"]
    VALIDATE_DRY --> SHOW_DRY["print preview:\n  Branch: from → to\n  Path: old → new"]
    SHOW_DRY --> DONE_DRY(["Ok(None)"])

    DRY_RUN -->|no| EXEC_MOVE["move_worktree(repo, from, to, opts)"]

    subgraph VALIDATE["validate_move() — 6 safety checks"]
        V1["1. is_detached()?\n→ CannotMoveDetached"]
        V1 --> V2["2. target worktree name exists?\n→ TargetExists"]
        V2 --> V3["3. target branch name exists?\n→ TargetExists"]
        V3 --> V4["4. source is protected?\n(unless force)\n→ ProtectedBranchMove"]
        V4 --> V5["5. is_dirty()?\n(unless force)\n→ DirtyWorktree"]
        V5 --> V6["6. has_unpushed_commits()?\n(unless force)\n→ UnpushedCommits"]
        V6 --> V_OK["validation passed"]
    end

    subgraph ATOMIC["atomic 3-step move"]
        A1["Step 1: branch.rename(to, false)"]
        A1 -->|ok| A2["Step 2: fs::rename(old_path, new_path)"]
        A1 -->|err| A_ERR1["return error"]
        A2 -->|ok| A3["Step 3: update metadata"]
        A2 -->|err| A_ROLLBACK["rollback: branch.rename(from, false)\nreturn Io error"]
        A3 --> A3_DETAIL["rename .bare/worktrees/<old> → .bare/worktrees/<new>\nupdate <new>/.git → gitdir: <new_meta_dir>\nupdate <new_meta_dir>/gitdir → <new_path>/.git"]
    end

    EXEC_MOVE --> VALIDATE
    VALIDATE --> ATOMIC
    ATOMIC --> RESULT["WorktreeDescriptor::new(repo, new_name)"]
    RESULT --> DONE(["Ok(Some(worktree))\n→ main prints new path"])
```

## Namespace support

Branch names containing `/` are supported, enabling moves between namespaces. `worktree_name` is derived as the basename (`Path::file_name()`) of the branch name. Parent directories are created automatically:

```
git workon move feature user/feature     # moves into namespace
git workon move user/feature feature     # moves out of namespace
git workon move old/path new/deep/path   # cross-namespace
```

## Metadata update details

After directory rename, two files are updated to keep git's bidirectional pointers consistent:

| File | Content |
|---|---|
| `<bare>/.bare/worktrees/<new_name>/gitdir` | `<new_worktree_path>/.git` |
| `<new_worktree_path>/.git` | `gitdir: <bare>/.bare/worktrees/<new_name>` |

## Key files

- `git-workon/src/cmd/move.rs` — `Run` impl, argument parsing, dry-run display
- `git-workon-lib/src/move.rs` — `move_worktree()`, `validate_move()`, `MoveOptions`, rollback logic
- `git-workon-lib/src/worktree.rs` — `current_worktree()`, `find_worktree()`, status check methods
