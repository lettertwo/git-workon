# git workon

An opinionated [git worktree][git-worktree] workflow for managing multiple branches simultaneously.

`git-workon` clones repositories as bare repos with a worktrees-first layout, then provides commands for creating, finding, and cleaning up worktrees — so switching between branches is just `cd`, not `git stash && git checkout`.

[git-worktree]: https://git-scm.com/docs/git-worktree

## Installation

### Homebrew (macOS / Linux)

```sh
brew install lettertwo/tap/git-workon
```

### From crates.io

```sh
cargo install git-workon

# Optional: install man page
git workon generate-man | install -m 644 /dev/stdin /usr/local/share/man/man1/git-workon.1
```

### From source

```sh
git clone https://github.com/lettertwo/git-workon
cd git-workon
make install       # installs git hooks + man page (PREFIX=/usr/local by default)
cargo install --path ./git-workon
```

Override the man page install location with `PREFIX`:

```sh
make install-man PREFIX=~/.local
```

## Quick start

### Clone a repository

```sh
git workon clone https://github.com/owner/repo
cd repo/main        # jumps into the default branch worktree
git workon clone https://github.com/owner/repo --no-hooks   # skip post-create hooks
```

### Initialize a new repository

```sh
git workon init                  # bare init in current directory with initial worktree
git workon init my-project       # bare init in my-project/ with initial worktree
git workon init --no-hooks       # skip post-create hooks
```

### Create a new worktree

```sh
git workon new                   # interactive: prompts for name, then base branch
git workon new my-feature        # creates branch + worktree
git workon new my-feature --base main   # branch from main
git workon new my-feature -B existing   # attach to existing branch, name dir 'my-feature'
git workon new my-feature --orphan      # create branch with no parent commits
git workon new my-feature --detach      # detach HEAD in the new worktree
git workon new my-feature --copy        # copy local files from base worktree (overrides config)
git workon new my-feature --no-copy     # skip file copy even if workon.autoCopy=true
git workon new my-feature --lock        # lock worktree after creation
git workon #123                  # create worktree from PR #123 (auto-fetches)
```

When in a stack worktree, `new` defaults the base branch to the current HEAD branch (so the new branch stacks on top). Pass `--base` to override, or `--no-stack` to disable stack-aware behavior.

When a branch name exists on multiple remotes, `new` prompts you to choose one (suppressed to first-remote by `--no-interactive`).

### Find an existing worktree

```sh
git workon find                  # interactive: fuzzy picker across all worktrees
git workon find main             # prints path to the 'main' worktree
git workon find feat --dirty     # find a worktree matching 'feat' that has uncommitted changes
git workon find --clean          # interactive: only clean worktrees
git workon find --ahead          # interactive: only worktrees with unpushed commits
git workon find --behind         # interactive: only worktrees behind upstream
git workon find --gone           # interactive: only worktrees with deleted upstreams
git workon find --new my-feature # bypass resolution and force a new worktree
git workon my-feature            # smart-route: find worktree, or create one if branch exists
```

The bare `workon <name>` routing: PR reference → `new` (auto-fetch); existing worktree → `find` (navigate); local or remote tracking branch with no worktree → `new` (auto-attach); stack-home + branch in current stack → `checkout` (in-place HEAD switch); no match → `find` (error).

`find` match priority: exact name > fuzzy dir/branch name > interactive picker. Multiple fuzzy matches trigger the picker. When stack-active, the picker shows a tree with `◉` (current), `◎` (exists), `◯` (metadata-only, no worktree). Pressing `Tab` on any item forces creation of a new worktree instead of navigating to an existing one. Selecting a `◯` item automatically routes to `new` to create/attach it.

Status filters suppress the stack tree and use a flat list (metadata-only diffs cannot satisfy worktree-status filters).

### List worktrees

```sh
git workon list                  # all worktrees
git workon list --dirty          # only worktrees with uncommitted changes
git workon list --clean          # only worktrees without uncommitted changes
git workon list --ahead          # worktrees with unpushed commits
git workon list --behind         # worktrees behind their upstream
git workon list --gone           # worktrees whose upstream branch was deleted
git workon list --dirty --ahead  # filters combine with AND logic
# Note: filters produce a flat list in stack-enabled repos (the tree is suppressed).
```

### Prune stale worktrees

`prune` always analyzes every worktree in scope for every signal (branch deleted, remote gone, merged into target, PR merged) — `--gone`/`--merged` don't hide anything, they just decide what counts as an *active* criterion for pre-checking and auto-pruning. A branch-deleted worktree and a worktree with a merged PR are always active. By default, pruning deletes the local branch ref along with the worktree; use `--keep-branch` to preserve it.

```sh
git workon prune                 # interactive: multi-select picker, pre-checked with the safe default
git workon prune --yes           # skip the picker; prune exactly the pre-checked set (for scripting)
git workon prune --dry-run       # print the annotated analysis (pre-checked/selectable/locked out) without deleting
git workon prune my-feature      # narrow to this worktree only — nothing else is shown or touched
git workon prune --gone          # treat gone-upstream worktrees as active (pre-checked / auto-pruned)
git workon prune --gone --fetch  # fetch --prune from remotes first so gone status is fresh
git workon prune --merged        # treat merged-into-default worktrees as active
git workon prune --merged=release/v2  # merge target other than the default branch
git workon prune --keep-branch   # prune worktrees but keep local branch refs
git workon prune --allow-dirty   # prune even with uncommitted changes
git workon prune --allow-unmerged # prune even with unmerged commits
git workon prune --include-locked # include locked worktrees
git workon prune --force         # override all safety checks (protection, dirty, unmerged, locked)
```

Naming a worktree strictly narrows the scope — it's never additive with `--gone`/`--merged`. An unmatched name is a hard error listing every miss, before anything is deleted. A named worktree with nothing wrong with it (no signal, not dirty, not unmerged) still shows up — annotated "not prunable" — but needs `--force` to actually be pruned; naming is how a healthy worktree gets pulled into view, not how it gets deleted. The default worktree never appears, even when named with `--force`.

In an interactive terminal, `prune` opens a checkbox picker (pre-checked rows match the same "safe default" `--yes` would prune) followed by one summary confirm. Non-interactively (`--yes`, `--json`, or no TTY), it prunes the pre-checked set directly.

Safety checks (skipped with `--force`): protected branches (`workon.pruneProtectedBranches`), locked worktrees (`--include-locked`), uncommitted changes (tracked files only when the only signal is a gone upstream; `--allow-dirty`), unmerged commits (skipped when any signal is present; `--allow-unmerged`).

### Rename a worktree

```sh
git workon move new-name                # rename the current worktree
git workon move old-name new-name       # rename a specific worktree
git workon move new-name --dry-run      # preview without renaming
git workon move new-name --force        # override safety checks (dirty, unpushed, protected)
```

### Diagnose workspace issues

```sh
git workon doctor                # check for broken worktrees and missing dependencies
git workon doctor --fix          # automatically repair fixable issues
git workon doctor --dry-run      # preview fixes without applying
```

Checks performed:
- **Worktrees**: missing directories, broken git links, gone upstreams
- **Dependencies**: `gh` CLI (PR features), `gh` auth status, git remote, `gt` CLI (stack features), hook commands in PATH
- **Configuration**: renamed config keys (auto-fixable), invalid `stackModel`/`stackWorktreeGranularity`, invalid `prFormat`, `defaultBranch` not found in repo, `stackModel=graphite` without `gt init`

### Copy untracked files between worktrees

Copies git-untracked files (ignored files and untracked-but-not-staged files). The `to` argument defaults to the current worktree when omitted.

```sh
git workon copy main                         # copy from 'main' into current worktree
git workon copy main my-feature              # copy from 'main' into 'my-feature'
git workon copy main my-feature --pattern '.env*'   # copy only files matching pattern
git workon copy main my-feature --exclude '.env.prod'  # exclude specific files (additive with config)
git workon copy main my-feature --force      # overwrite existing files in destination
git workon copy main my-feature --no-include-ignored   # skip git-ignored files
```

## Shell integration

`git workon find` and `git workon new` print the worktree path to stdout — they don't `cd` on their own, since a subprocess can't change the parent shell's directory. Without the shell wrapper, you'd need to do this yourself:

```sh
# bash / zsh
cd "$(git workon find main)"

# fish
cd (git workon find main)
```

The shell integration sets up a `workon` wrapper function that captures the output and `cd`s automatically when the result is a directory:

```sh
# bash
eval "$(git workon shell-init bash)"

# zsh
eval "$(git workon shell-init zsh)"

# fish
git workon shell-init fish | source
```

The shell is auto-detected from `$SHELL` if not specified. Use `--cmd` to change the wrapper function name (default: `workon`):

```sh
eval "$(git workon shell-init --cmd wt)"   # installs a 'wt' function instead
```

After setup, `workon <name>` changes your current directory to the worktree, and `workon new <name>` drops you directly into the newly created worktree.

> **Scripting**: pass `--no-interactive` to `find` and `new`, and `--yes` to `prune`, to suppress all prompts. Pass `--json` globally to get machine-readable output on success and a `{"error": {...}}` envelope on failure.

## Global flags

These flags are accepted before any subcommand:

| Flag | Effect |
|------|--------|
| `--json` | Output results as JSON; errors are `{"error":{"code":…,"message":…}}` |
| `--no-color` | Disable ANSI color output |
| `--no-stack` | Disable stack-aware behavior for this invocation |
| `-v / -q` | Increase/decrease log verbosity (can repeat, e.g. `-vv`) |

`--json` also sets `--no-interactive` for commands that prompt.

## Man page

```sh
man git-workon
```

## Configuration

`git-workon` uses git config keys under the `workon.*` namespace:

```gitconfig
[workon]
    # New worktrees
    defaultBranch = main         # base branch when none is specified
    prFormat = pr-{number}       # worktree name pattern for PR checkouts

    # File copying
    autoCopy = false             # copy local files automatically on 'new'
    copyPattern = .env.local     # glob patterns to copy (multi-value)
    copyExclude = .env.prod      # patterns to exclude (multi-value)
    copyIncludeIgnored = true    # include git-ignored files when copying

    # Hooks
    postCreateHook = npm install # commands run after worktree creation (multi-value)
    hookTimeout = 300            # hook timeout in seconds (0 = no timeout)

    # Pruning
    pruneProtectedBranches = main       # branches protected from pruning (multi-value)
    pruneProtectedBranches = release/*
    pruneGone = false            # prune gone-upstream worktrees by default
    pruneFetch = false           # fetch from remotes before evaluating gone status

    # Stacked diffs (Graphite or gh-stack)
    stackModel = auto            # "auto", "graphite", "gh-stack", "git", or "none"
    stackWorktreeGranularity = stack  # "stack" (one worktree per stack)
    stackAutoTrack = true        # auto-register new branches with the active stack tool after 'workon new'
    gtAutoTrack = true           # deprecated alias for stackAutoTrack, read only as a fallback
```

See `man git-workon` or `git workon --help` for full documentation.

## Library

The core logic is published as the [`workon`](https://crates.io/crates/git-workon-lib) crate.
API docs are on [docs.rs](https://docs.rs/git-workon-lib).
