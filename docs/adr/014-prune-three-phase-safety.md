# 014 — Three-Phase Prune with Safety Checks

## Context

Pruning worktrees is destructive and hard to undo: it deletes the working directory and removes the git worktree registration. Users may have uncommitted work, unmerged commits, or be working on a protected branch. We needed a design that is safe by default but scriptable with explicit overrides.

## Decision

`prune` operates in three sequential, independent phases:

**Phase 1 — Candidate collection**: Builds the list of worktrees to consider. Explicit names come from positional arguments (reason: `Explicit`). Filter-based candidates come from: branches that no longer exist locally (`BranchDeleted`), worktrees whose upstream is gone with `--gone` flag (`RemoteGone`), or worktrees merged into a target with `--merged` flag (`Merged(target)`).

**Phase 2 — Safety checks**: Each candidate passes through four ordered checks. If any check fails (and `--force` is not set), the candidate is moved to the "skipped" list with a reason:

1. Protected branch (`workon.pruneProtectedBranches`)
2. Default worktree
3. Dirty (uncommitted changes) — override with `--allow-dirty`
4. Unmerged commits — override with `--allow-unmerged` (skipped for `BranchDeleted` and `Merged` candidates)

**Phase 3 — Execution**: Displays skipped and to-prune lists, then (unless `--dry-run`) confirms interactively or proceeds with `--yes`. Execution removes the directory with `fs::remove_dir_all` and calls `worktree.prune()`.

`--force` disables all four safety checks simultaneously. JSON mode skips interactive confirmation and prints a structured result.

## Consequences

- Safe by default: dirty or unmerged worktrees are never silently deleted.
- Phases are independent: safety checks always run even for explicitly named worktrees.
- `--force` is a single escape hatch rather than four separate `--no-*` flags, keeping the interface simple.
- `BranchDeleted` and `Merged` candidates skip the "unmerged commits" check because the branch state already implies the work is handled.

## References

- `docs/diagrams/prune-flow.md` — full three-phase flowchart
- `git-workon/src/cmd/prune.rs` — `PruneReason`, `PruneCandidate`, all three phases
