# 011 — Two-Mode File Copy Design (Manual vs Automatic)

## Context

Untracked files (`.env.local`, `.vscode/settings.json`, compiled artifacts) need to be available in new worktrees but cannot be committed to git. Users need both an explicit "copy now" command and an automatic "copy on every new worktree" mode. The two modes have slightly different defaults and priority rules that needed to be resolved consistently.

## Decision

The copy system operates in two modes:

**Mode 1 — Standalone command (`git workon copy-untracked`)**

- Copies from the current worktree to a named destination worktree.
- Default pattern: `**/*` (all untracked files).
- `--pattern` flag overrides the default; `workon.copyPattern` config is used if `--pattern` is not given.
- Priority: `--pattern` > `workon.copyPattern` > default `**/*`.
- `--force` overwrites existing files at the destination.

**Mode 2 — Automatic copying (`new` command with `workon.autoCopyUntracked=true`)**

- Runs after worktree creation, before post-create hooks.
- Source is the base branch's worktree (or HEAD's worktree if no base).
- Always respects `workon.copyExclude` patterns.
- `--copy-untracked` / `--no-copy-untracked` CLI flags override the config.
- Gracefully skips if the source worktree does not exist.
- Failures warn but do not abort `new`.

Both modes use the same `copy_files()` function from `git-workon-lib/src/copy.rs` and the same platform-optimized copy backend (see [ADR-010](010-platform-copy-on-write.md)).

## Consequences

- Users can configure automatic copying once and forget about it.
- The standalone command is useful for one-off copies or debugging copy patterns.
- Automatic copy runs before hooks, so hooks can depend on copied files being present.
- The two modes have different default patterns (`**/*` vs config-driven), which can surprise users who expect the `new` command to copy everything by default.

## References

- `git-workon-lib/src/copy.rs` — `copy_files()`, module doc with mode descriptions
- `docs/diagrams/new-worktree.md` — copy step in the `new` flow
