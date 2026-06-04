# Stacked diffs with Graphite

git-workon integrates with [Graphite](https://graphite.dev) for stacked-diff workflows.
When Graphite is active, `list`, `find`, and `new` all become stack-aware by default.
Use `--no-stack` on any invocation to fall back to today's branch-flat behavior.

## Setup

**Requirements**: `gt` CLI installed and `gt init` run inside the repository.

git-workon auto-detects Graphite when `gt` is on PATH and `.git/.graphite_repo_config`
exists. No manual config needed. Verify with:

```bash
git workon doctor
```

You should see `workon.stackModel = graphite` in the configuration section.

To opt out globally:

```bash
git config workon.stackModel none
```

## Worktree per stack

The recommended pattern is one worktree per stack. Inside a stack-worktree you navigate
between branches with `gt up` / `gt down`, and use `gt create` to extend the current
stack with a new diff.

Use `git workon new` to start a **new stack** that forks off the current one.

## Creating worktrees

### New stack from trunk

From anywhere, create a worktree for a fresh stack:

```bash
git workon new my-feature
# → creates my-feature/ branched off trunk (e.g. develop, main)
# → runs gt track --parent <trunk> inside the new worktree
```

### New stack forking off an existing one

When invoked from **inside a stack-worktree**, the base defaults to the current HEAD
branch automatically:

```bash
# Inside the stack-worktree on branch "auth-step-2"
git workon new login-form
# → creates login-form/ branched off auth-step-2
# → runs gt track --parent auth-step-2 inside the new worktree
```

Pass `--base` to override:

```bash
git workon new login-form --base develop  # explicit base always wins
git workon new login-form --no-stack     # base = trunk; no gt track
```

### Extending the current stack

To add a branch on top of the current HEAD **inside the same worktree**, use Graphite:

```bash
gt create my-next-diff
```

`git workon new` always creates a new worktree; extending the current stack stays `gt`'s
job.

## Listing worktrees with stack trees

```bash
git workon list
```

Stack-active output shows a graphite-style tree — `◉` for branches with a checked-out
worktree, `◯` for metadata-only diffs (stack branches with no worktree):

```
◉ main                        1 day ago
◯ auth-step-1
◉ auth-step-2  ./auth   ↑     2 hours ago  ← here
◯ auth-step-3
```

`← here` marks the worktree that contains the current directory. When the worktree
directory name differs from the branch (e.g. the `./auth` worktree with HEAD on
`auth-step-2`), the path is shown as a dim annotation. Fall back to flat list:

```bash
git workon list --no-stack
```

## Finding worktrees by stack-member branch

```bash
git workon find auth-step-3
```

When stack-active, `find` searches branch membership in stacks — so `auth-step-3`
returns the `auth` worktree even when its HEAD is on `auth-step-1`.

```bash
git workon find auth-step-3 --no-stack   # reverts to name/HEAD-only match
```

## Disabling gt auto-track

`gt track` runs automatically after `git workon new` when stack-active. To disable:

```bash
git config workon.gtAutoTrack false
```

The worktree is still created; the branch just won't appear in `gt log` until you run
`gt track` manually.

## Configuration reference

| Key | Default | Description |
|-----|---------|-------------|
| `workon.stackModel` | `auto` | Stack tool: `graphite`, `none`, or `auto` (detect) |
| `workon.stackWorktreeGranularity` | `stack` | Worktree mapping (only `stack` in v1) |
| `workon.gtAutoTrack` | `true` | Run `gt track` after `new` when stack-active |

## v1 limitations

- Only Graphite is supported. `branchless`, `sapling`, and `spr` are planned.
- `stackWorktreeGranularity = diff` (one worktree per branch) is planned but not yet
  implemented — setting it currently returns an error.
- Stack-aware `prune` and `move` (refuse to orphan stack children, rename whole stacks)
  are planned for a future release.
