# Stacked diffs with Graphite or gh-stack

git-workon integrates with [Graphite](https://graphite.dev) and with
[`gh stack`](https://github.com/github/gh-stack) (the `gh` CLI extension) for stacked-diff
workflows. When either tool is active, `list`, `find`, and `new` all become stack-aware by
default. Use `--no-stack` on any invocation to fall back to branch-flat behavior.

If both tools' artifacts are present in the same repository, Graphite wins: see
"Auto-detection and both tools present" below.

## Setup

### Graphite

**Requirements**: `gt` CLI installed and `gt init` run inside the repository.

git-workon auto-detects Graphite when `.git/.graphite_repo_config` or
`.git/.graphite_metadata.db` exists. No manual config needed.

### gh-stack

**Requirements**: the `gh stack` extension installed (`gh extension install
github/gh-stack`) and at least one `gh stack init`/`gh stack add` run somewhere in the
repository.

git-workon keeps one canonical `gh-stack` file at `<common-dir>/gh-stack` and symlinks each
worktree's `gh-stack`/`gh-stack.lock` admin-dir paths to it, so the extension behaves the same
whether you run it from the worktree that first created the stack or from any other worktree in
it. `workon new` plants these symlinks itself in a `GhStack` repository; see "The symlink model"
below for what happens to a worktree that predates git-workon's involvement.

### Verify, either way

```bash
git workon doctor
```

You should see `workon.stackModel = graphite` or `workon.stackModel = gh-stack` in the
configuration section, matching whichever tool you use.

To opt out globally:

```bash
git config workon.stackModel none
```

### Auto-detection and both tools present

`workon.stackModel = auto` (the default) checks Graphite's artifacts first, then gh-stack's:
`.graphite_repo_config` comes from an explicit, repo-wide `gt init`, while a `gh-stack` file can
appear from a single `gh stack add` run in one worktree, so the more deliberate signal wins. If
you've tried both tools in the same repository and want gh-stack instead, pin it explicitly:

```bash
git config workon.stackModel gh-stack
```

`git workon doctor` reports `BothStackToolsDetected` when it sees artifacts for both, so you
know when `auto` made a call you might want to override.

## Worktree per stack

The recommended pattern is one worktree per stack. Inside a stack-worktree you navigate
between branches with `gt up`/`gt down` (Graphite) or `gh stack up`/`gh stack down` (gh-stack),
and extend the stack with `gt create` or `gh stack add`.

Use `git workon new` to start a **new stack** that forks off the current one.

## Creating worktrees

### New stack from trunk

From anywhere, create a worktree for a fresh stack:

```bash
git workon new my-feature
# → creates my-feature/ branched off trunk (e.g. develop, main)
# → Graphite: runs gt track --parent <trunk> inside the new worktree
# → gh-stack: writes my-feature into the canonical gh-stack file directly
#   (no subprocess call: gh-stack's own `add` command refuses to run against a
#   branch that isn't already checked out at the top of the stack)
```

### New stack forking off an existing one

When invoked from **inside a stack-worktree**, the base defaults to the current HEAD
branch automatically:

```bash
# Inside the stack-worktree on branch "auth-step-2"
git workon new login-form
# → creates login-form/ branched off auth-step-2
# → Graphite: runs gt track --parent auth-step-2
# → gh-stack: registers login-form as based on auth-step-2 in the canonical file
```

Pass `--base` to override:

```bash
git workon new login-form --base develop  # explicit base always wins
git workon new login-form --no-stack      # base = trunk; no stack registration
```

### Extending the current stack

To add a branch on top of the current HEAD **inside the same worktree**, use the stack tool
directly:

```bash
gt create my-next-diff       # Graphite
gh stack add my-next-diff    # gh-stack
```

`git workon new` always creates a new worktree; extending the current stack in place stays the
stack tool's job.

## Listing worktrees with stack trees

```bash
git workon list
```

Stack-active output shows a Graphite-style lane graph with three glyphs:

- `◉` green: the active worktree (your current directory)
- `◎` plain: a worktree exists but is not current
- `◯` dim: metadata-only diff (stack branch with no worktree)

Display order is **tip-on-top**: the tip of each stack appears at the top, the
trunk at the bottom. Each stack is one straight vertical lane; sibling stacks fan
out to the right and converge back on the fork node's own row (no extra connector
lines).

```
◯ auth-step-3
◉ auth-step-2  ./auth   ↑     2 hours ago  ← here
◯ auth-step-1
◎ main                        1 day ago
```

With a sibling stack branching off `main`:

```
◯ auth-step-3
◉ auth-step-2  ./auth   ↑     2 hours ago  ← here
◯ auth-step-1
│ ◎ other-feature              3 days ago
◎─╯ main                       1 day ago
```

`← here` marks the worktree that contains the current directory. When the worktree
directory name differs from the branch (e.g. the `./auth` worktree with HEAD on
`auth-step-2`), the path is shown as a dim annotation.

Under gh-stack, a branch that is the direct child of a trunk also shows a dim ` #N` suffix
naming its `stacks[].number`:

```
◯ auth-step-3
◉ auth-step-2  ./auth   ↑     2 hours ago  ← here
◯ auth-step-1 #7
◎ main                        1 day ago
```

Status filters (`--dirty`, `--clean`, `--ahead`, `--behind`, `--gone`) suppress the tree
and produce a flat list of matching worktrees only. Metadata-only `◯` diffs have no working
tree and can never satisfy a worktree-status filter, so they are excluded.

```bash
git workon list --dirty          # flat: only worktrees with uncommitted changes
git workon list --no-stack       # flat: all worktrees, tree suppressed permanently
```

## Finding worktrees by stack-member branch

```bash
git workon find auth-step-3
```

When stack-active, `find` searches branch membership in stacks, so `auth-step-3`
returns the `auth` worktree even when its HEAD is on `auth-step-1`.

```bash
git workon find auth-step-3 --no-stack   # reverts to name/HEAD-only match
```

## The symlink model (gh-stack)

`gh stack`'s own file format was designed for a single working tree: it writes one JSON file at
`git rev-parse --git-dir` + `/gh-stack`, which for a linked worktree is
`<common-dir>/worktrees/<name>/gh-stack`. Left alone, that means `gh stack view` (and `up`,
`down`, `top`, `bottom`) only see the stacks registered from the one worktree that happened to
write the file.

git-workon works around this by keeping one canonical copy at `<common-dir>/gh-stack` and
symlinking each worktree's admin-dir path to it. gh-stack's own writes (`os.WriteFile`) truncate
and rewrite through the symlink rather than replacing it, so every worktree ends up reading and
writing the same file transparently, with no `gh stack link` step required.

A worktree can still end up **unlinked**: one created before this repository ever became a
`GhStack` repo under git-workon, or one where `gh stack init` ran before `workon new` had a
chance to plant the symlinks (see "Known limitations" below). `git workon doctor` reports these
as `GhStackWorktreeNotLinked`, and:

```bash
git workon doctor --fix
```

plants the missing symlinks for a worktree with no `gh-stack` file yet, or merges an existing
real `gh-stack` file into canonical (leaving a `gh-stack.bak` backup behind) before replacing it
with a symlink, for a worktree that already has stacks registered locally. Until `--fix` runs,
git-workon still reads an unlinked worktree's file as a degraded fallback, so nothing in it goes
invisible; it just isn't shared with other worktrees the way a linked file is.

## Disabling auto-track

`git workon new` registers the new branch with the active stack tool automatically (`gt track`
under Graphite, a direct write to the canonical file under gh-stack). To disable:

```bash
git config workon.stackAutoTrack false
```

The worktree is still created; the branch just won't appear in the stack tool's own view until
you register it manually (`gt track`, or `gh stack add` from inside the worktree).

`workon.gtAutoTrack` is the old name for this setting and is still read as a fallback when
`workon.stackAutoTrack` is unset, so existing config keeps working, but new config should use
`stackAutoTrack`.

## Configuration reference

| Key | Default | Description |
|-----|---------|--------------|
| `workon.stackModel` | `auto` | Stack tool: `graphite`, `gh-stack`, `git`, `none`, or `auto` (detect) |
| `workon.stackWorktreeGranularity` | `stack` | Worktree mapping (only `stack` in v1) |
| `workon.stackAutoTrack` | `true` | Register the new branch with the active stack tool after `new` |
| `workon.gtAutoTrack` | `true` | Deprecated alias for `workon.stackAutoTrack`, read only as a fallback |

## Known limitations

- `stackWorktreeGranularity = diff` (one worktree per branch) is planned but not yet
  implemented; setting it currently returns an error.
- Stack-aware `prune` and `move` (refuse to orphan stack children, rename whole stacks)
  are planned for a future release.
- A repository where `gh stack init` runs for the first time inside a worktree (rather than via
  `git workon new`) gets a real, unlinked `gh-stack` file in that worktree until `git workon
  doctor --fix` migrates it to canonical. `doctor` reports this, and the degraded union read
  means the stacks in that file are still visible to `list`/`find` in the meantime, just not
  shared with other worktrees until the fix runs.
