# 015 — Atomic Move with Branch-Rename Rollback

## Context

Renaming a worktree requires three coordinated changes: renaming the branch, moving the directory, and updating two git metadata files (the worktree's `.git` pointer and the `.bare/worktrees/<name>/gitdir` pointer). If the directory move fails after the branch rename, the repository is left in an inconsistent state. We needed a strategy that keeps git metadata consistent even when the filesystem operation fails mid-way.

## Decision

`move_worktree()` (`git-workon-lib/src/move.rs`) performs the rename in three steps with a rollback on step 2 failure:

1. **Branch rename**: `branch.rename(to, false)` — purely in-memory/git-db, fast and reliable.
2. **Directory move**: `fs::rename(old_path, new_path)` — if this fails, immediately roll back step 1 with `branch.rename(from, false)` and return the IO error.
3. **Metadata update**: rename `.bare/worktrees/<old>` → `.bare/worktrees/<new>`, rewrite `<new>/.git` and `<new_meta>/gitdir` to point at the new paths.

Six safety checks run before execution (via `validate_move()`): detached HEAD, target name/branch already exists, protected branch (unless `--force`), dirty (unless `--force`), unpushed commits (unless `--force`).

`--dry-run` runs all validation but skips execution and prints the planned branch and path changes.

## Consequences

- The rollback on directory-move failure means the branch rename is always reversed on error; there is no partial state where the branch is renamed but the directory is not.
- Step 3 (metadata) has no rollback. If it fails after the directory move, the repository metadata is inconsistent. This is treated as an unrecoverable error that the user would need to fix manually, but in practice `fs::rename` and file writes on the same filesystem are extremely reliable.
- Branch names with `/` are supported for namespace moves; the worktree name is derived as the path basename.

## References

- `docs/diagrams/move-flow.md` — full flowchart including rollback
- `git-workon-lib/src/move.rs` — `move_worktree()`, `validate_move()`, rollback logic
