# 012 — Three Branch Types for Worktree Creation

## Context

Different workflows need different kinds of worktrees. Standard feature branches track an existing or new branch from a base. Documentation branches (e.g. `gh-pages`) need completely independent history with no parent commits. Exploratory work on a specific commit SHA needs a detached HEAD without creating a branch. These three use cases have distinct git semantics that needed explicit modeling.

## Decision

`BranchType` is an enum with three variants, selected by CLI flags on the `new` command:

- **`Normal`** (default): Looks up an existing local branch, then an existing remote branch, then creates a new branch from the base or HEAD. Uses `repo.worktree()` with the branch reference.
- **`Orphan`** (`--orphan`): Creates a worktree with no parent commits. Implementation: `repo.worktree()` with no ref, then manually writes `HEAD: ref: refs/heads/<name>`, removes any existing branch ref, clears the index and working directory, and writes an empty-tree commit with no parents.
- **`Detached`** (`--detach`): Creates a worktree pointing at the current HEAD SHA. Implementation: `repo.worktree()` with no ref, then writes the commit SHA directly to the worktree's `HEAD` file.

The `BranchType` enum is defined in `git-workon-lib/src/worktree.rs` and passed to `add_worktree()`.

## Consequences

- Each type has a distinct creation path; Normal is the happy path optimized for daily use.
- Orphan and Detached bypass git2's normal ref handling and write files directly, which is fragile but necessary since git2 does not expose these operations.
- PR worktrees always use Normal with a pre-fetched remote ref; `--orphan` and `--detach` are mutually exclusive with PR references.

## References

- `docs/diagrams/new-worktree.md` — branch type selection flowchart
- `git-workon-lib/src/worktree.rs` — `BranchType`, `add_worktree()`
