# 014 — Three-Phase Prune with Safety Checks

## Context

Pruning worktrees is destructive and hard to undo: it deletes the working directory and removes the git worktree registration. Users may have uncommitted work, unmerged commits, or be working on a protected branch. We needed a design that is safe by default but scriptable with explicit overrides.

## Decision

`prune` operates in three sequential, independent phases:

**Phase 1 — Candidate collection**: Builds the list of worktrees to consider. Explicit names come from positional arguments (reason: `Explicit`). Filter-based candidates come from: branches that no longer exist locally (`BranchDeleted`), worktrees whose upstream is gone with `--gone` flag (`RemoteGone`), or worktrees merged into a target with `--merged` flag (`Merged(target)`).

**Phase 2 — Safety checks**: Each candidate passes through five ordered checks. If any check fails (and `--force` is not set), the candidate is moved to the "skipped" list with a reason:

1. Protected branch (`workon.pruneProtectedBranches`)
2. Default worktree
3. Locked worktree — skip unless `--force` or `--include-locked`. Skip reason: `"locked (use --include-locked to override)"`. (TODO(agent-integration): implement `is_locked()` and add `--include-locked` flag)
4. Dirty (uncommitted changes) — override with `--allow-dirty`. `RemoteGone` candidates use `has_tracked_changes()` instead of `is_dirty()`, so untracked files (build artifacts, IDE dirs) do not block pruning.
5. Unmerged commits — override with `--allow-unmerged` (skipped for `BranchDeleted`, `Merged`, and `RemoteGone` candidates)

**Phase 3 — Execution**: Displays skipped and to-prune lists, then (unless `--dry-run`) confirms interactively or proceeds with `--yes`. Execution removes the directory with `fs::remove_dir_all`, calls `worktree.prune()`, then deletes the local branch ref (unless `--keep-branch`).

`--force` disables all five safety checks simultaneously. JSON mode skips interactive confirmation and prints a structured result.

## Consequences

- Safe by default: dirty, unmerged, or locked worktrees are never silently deleted.
- Phases are independent: safety checks always run even for explicitly named worktrees.
- `--force` is a single escape hatch rather than five separate `--no-*` flags, keeping the interface simple.
- Locked worktrees get their own opt-in flag (`--include-locked`) rather than being bundled under `--allow-dirty`, because a lock is an explicit administrative decision — it should require explicit opt-in.
- `BranchDeleted` and `Merged` candidates skip the "unmerged commits" check because the branch state already implies the work is handled.
- `RemoteGone` candidates skip the "unmerged commits" check and use `has_tracked_changes()` for the dirty check, reducing false positives from untracked files.
- Branch cleanup is default behavior: local branch refs are deleted after pruning unless `--keep-branch` is passed.

## References

- `docs/diagrams/prune-flow.md` — full three-phase flowchart
- `git-workon/src/cmd/prune.rs` — `PruneReason`, `PruneCandidate`, all three phases
