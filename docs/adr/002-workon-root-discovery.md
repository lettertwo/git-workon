# 002 — Workon Root Discovery via Common Ancestor

## Context

Commands like `new`, `find`, and `prune` need to know the project root directory — the directory that contains `.bare/` and all sibling worktrees. A user may invoke the CLI from inside any worktree (e.g. `my-project/main/src/`), so the root cannot simply be `$PWD`. We needed a reliable algorithm that works from any depth inside the layout.

## Decision

`workon_root()` (`git-workon-lib/src/workon_root.rs`) finds the common ancestor between the repository's `.git` path (`repo.path()`) and the working directory (`repo.workdir()`).

For a worktree at `my-project/main/`, `repo.path()` resolves to `my-project/.bare/worktrees/main/` and `repo.workdir()` to `my-project/main/`. Walking the ancestors of both paths, the first common entry is `my-project/` — the workon root.

For the bare repository itself (no working directory), it falls back to `path.parent()` of the `.bare` directory.

## Consequences

- The algorithm is O(depth) and requires no config or environment variable.
- It correctly handles nested worktree paths at any depth.
- It depends on the bare + sibling layout from [ADR-001](001-bare-repo-worktrees-layout.md); non-standard layouts will produce incorrect results.
- Every command that needs to enumerate sibling worktrees or construct new worktree paths calls `workon_root()`.

## References

- `git-workon-lib/src/workon_root.rs` — full implementation
- `docs/diagrams/clone-and-init.md` — directory structure diagram
