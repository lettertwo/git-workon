# 001 — Bare Repository + Sibling Worktrees Layout

## Context

Git worktrees require a repository to be accessible from multiple working directories simultaneously. A standard clone places the `.git` directory inside the working tree, which works for a single checkout but creates friction when sibling worktrees need to reference the same object store. We needed a layout that cleanly separates the git database from any individual working tree.

## Decision

`clone` and `init` both produce a bare repository stored at `<project>/.bare`, with a `.git` link file at `<project>/.git` containing `gitdir: ./.bare`. Each worktree is a sibling directory at `<project>/<branch-name>/` and contains its own `.git` link file pointing into `.bare/worktrees/<name>`.

The shared `convert_to_bare()` function handles the transition: it sets `core.bare=true`, renames the `.git` directory to `.bare`, writes the `.git` link file, reopens the repository from `.bare`, and adds the full fetch refspec for `origin`.

After conversion both `clone` and `init` call `add_worktree()` to create an initial worktree for the default branch, so the user always lands in a usable working directory.

```
my-project/
├── .bare/          ← git object store (core.bare=true)
├── .git            ← link: "gitdir: ./.bare"
└── main/           ← initial worktree
    ├── .git        ← link: "gitdir: ../.bare/worktrees/main"
    └── ...
```

## Consequences

- All worktrees share one object store — no duplicate objects, fast branch switching.
- The `.git` link file makes standard git tooling work from any worktree directory.
- `workon_root()` discovery relies on this layout (see [ADR-002](002-workon-root-discovery.md)).
- Users must use `git workon clone` or `git workon init` to set up the layout; existing standard clones require `git workon convert` (if implemented).

## References

- `docs/diagrams/clone-and-init.md` — full sequence diagram
- `git-workon-lib/src/clone.rs`, `git-workon-lib/src/init.rs`
- `git-workon-lib/src/convert_to_bare.rs` — shared conversion logic
