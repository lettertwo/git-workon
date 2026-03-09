# 007 — Hybrid Hook System Alongside Git's post-checkout

## Context

Users need to run setup commands (e.g. `npm install`, copy `.env` files) automatically after a new worktree is created. Git's native `post-checkout` hook fires on `git worktree add`, but it fires for all checkouts (not just new worktrees), supports only one script, and requires conditional shell logic to detect the new-worktree case. We needed something simpler that didn't conflict with existing hooks.

## Decision

`git-workon` provides a `workon.postCreateHook` multi-value config key that lists shell commands to run after worktree creation. These hooks are executed by `execute_post_create_hooks()` in `hooks.rs` only from `new`, `clone`, and `init` — explicit worktree creation commands, never on plain checkouts.

The execution model:

- Hooks run sequentially via `sh -c` in the new worktree directory.
- Three environment variables are set: `WORKON_WORKTREE_PATH`, `WORKON_BRANCH_NAME`, `WORKON_BASE_BRANCH`.
- Each hook is subject to `workon.hookTimeout` (default 300 s; 0 = no timeout).
- Hook failures warn but do not abort the overall command.
- `--no-hooks` skips all `workon.postCreateHook` commands.

Git's `post-checkout` hook still fires first (standard git behavior) and is not affected. Both approaches coexist: git's hook handles git-level events, `workon.postCreateHook` handles worktree-creation-specific setup.

## Consequences

- No shell scripting required: `git config --add workon.postCreateHook "npm install"` is enough.
- Multiple hooks are supported natively via multi-value config, without manual multiplexing.
- Hook commands execute arbitrary code from config; users should only set hooks in trusted repos and use `--no-hooks` with untrusted code.
- The timeout prevents runaway hooks from blocking the CLI indefinitely.

## References

- `git-workon/src/hooks.rs` — full module doc and implementation
- `docs/diagrams/new-worktree.md` — hook execution in the `new` flow
