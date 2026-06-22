# Domain Glossary

Terms used throughout the `git-workon` codebase. Implementation details do not belong here.

## Worktree States

**Gone upstream** — a local branch whose upstream tracking ref (`refs/remotes/<remote>/<branch>`) no longer exists locally. Caused by the upstream branch having been deleted on the remote and the deletion not yet fetched. Detected via `Branch::upstream()` returning `Err` while `branch.<name>.remote` is still set in git config. Accurate only after a prune-fetch. See also: `WorktreeDescriptor::has_gone_upstream()`.

**Prune-fetch** — a fetch that also deletes (prunes) stale local remote-tracking refs for branches that no longer exist on the remote. Equivalent to `git fetch --prune <remote>`. Makes "gone upstream" detection trustworthy.

## Prune Candidate Reasons

**BranchDeleted** — the local branch ref for the worktree no longer exists in the repository. Always a prune candidate regardless of flags.

**RemoteGone** — the worktree's branch has a gone upstream. Only a candidate when `--gone` / `workon.pruneGone` is enabled.

**Merged(target)** — the worktree's branch has been merged into `target`. Only a candidate when `--merged` is passed.

**Explicit** — the worktree was named directly as a positional argument to `prune`.
