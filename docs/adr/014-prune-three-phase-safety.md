# 014 — Three-Phase Prune with Safety Checks

## Context

Pruning worktrees is destructive and hard to undo: it deletes the working directory and removes the git worktree registration. Users may have uncommitted work, unmerged commits, or be working on a protected branch. We needed a design that is safe by default but scriptable with explicit overrides.

## Decision

`prune` operates in four sequential phases (Phase 0 is optional):

**Phase 0 (optional) — Prune-fetch**: When `--fetch` is passed or `workon.pruneFetch = true`, prune-fetches from all remotes tracked by worktree branches (`git fetch --prune <remote>`). This deletes stale `refs/remotes/<remote>/*` entries, making "gone upstream" detection accurate. On failure (network error, auth failure, unknown remote), a warning is emitted and the phase continues with cached refs — a failed fetch can only cause under-pruning, never a false prune. `--no-fetch` suppresses the config-enabled behaviour for one run.

**Phase 1 — Candidate collection**: Builds the list of worktrees to consider. Explicit names come from positional arguments (reason: `Explicit`). Filter-based candidates come from: branches that no longer exist locally (`BranchDeleted`), worktrees whose upstream is gone when `--gone` is active (`RemoteGone`, using `WorktreeDescriptor::has_gone_upstream()`), or worktrees merged into a target with `--merged` flag (`Merged(target)`).

Gone-upstream behaviour:
- `--gone` / `workon.pruneGone = true` — include `RemoteGone` candidates (default: off)
- `--no-gone` — suppress the config-enabled behaviour for one run
- `--gone` and `--fetch` are independent: `--gone` alone uses cached refs; add `--fetch` to refresh first

Bare `prune` (no `--gone`) surfaces any gone-upstream worktrees as a non-destructive notice at the end, suggesting `--gone` (and `--fetch` if not already enabled). This hint is suppressed in `--json` mode.

**Phase 2 — Safety checks**: Each candidate passes through five ordered checks. If any check fails (and `--force` is not set), the candidate is moved to the "skipped" list with a reason:

1. Protected branch (`workon.pruneProtectedBranches`)
2. Default worktree
3. Locked worktree — skip unless `--force` or `--include-locked`. Skip reason: `"locked (use --include-locked to override)"`.
4. Dirty (uncommitted changes) — override with `--allow-dirty`. `RemoteGone` candidates use `has_tracked_changes()` instead of `is_dirty()`, so untracked files (build artifacts, IDE dirs) do not block pruning.
5. Unmerged commits — override with `--allow-unmerged` (skipped for `BranchDeleted`, `Merged`, and `RemoteGone` candidates)

**Phase 3 — Execution**: Displays skipped and to-prune lists, then (unless `--dry-run`) confirms interactively or proceeds with `--yes`. Execution removes the directory with `fs::remove_dir_all`, calls `worktree.prune()`, then deletes the local branch ref (unless `--keep-branch`).

`--force` disables all five safety checks simultaneously. JSON mode skips interactive confirmation and prints a structured result.

## Gone Detection Consolidation

The "gone upstream" check is defined once in `WorktreeDescriptor::has_gone_upstream()` (`git-workon-lib/src/worktree.rs`). This is the same method used by `find --gone` and `list --gone`. An earlier private `is_upstream_gone()` in `prune.rs` was stricter (required both `branch.<n>.remote` and `branch.<n>.merge`) and inconsistent with the other commands; it has been removed.

## Consequences

- Safe by default: dirty, unmerged, or locked worktrees are never silently deleted.
- Gone detection is only accurate after a prune-fetch; the hint and `--gone` behaviour on stale refs can only under-prune, never false-prune.
- `--gone` and `--fetch` are orthogonal: users control network I/O explicitly.
- Config keys `workon.pruneGone` and `workon.pruneFetch` (both default false) let users opt into the behaviours permanently without typing extra flags each run.
- Phases are independent: safety checks always run even for explicitly named worktrees.
- `--force` is a single escape hatch rather than five separate `--no-*` flags, keeping the interface simple.
- `BranchDeleted` and `Merged` candidates skip the "unmerged commits" check because the branch state already implies the work is handled.
- `RemoteGone` candidates skip the "unmerged commits" check and use `has_tracked_changes()` for the dirty check, reducing false positives from untracked files.
- Branch cleanup is default behavior: local branch refs are deleted after pruning unless `--keep-branch` is passed.

## References

- `docs/diagrams/prune-flow.md` — full phase flowchart
- `git-workon/src/cmd/prune.rs` — `PruneReason`, `PruneCandidate`, all phases
- `git-workon-lib/src/fetch.rs` — `remotes_tracked_by_worktrees`, `prune_fetch`
- `git-workon-lib/src/config.rs` — `WorkonConfig::prune_gone`, `WorkonConfig::prune_fetch`
