# git workon

An opinionated [git worktree][git-worktree] workflow for managing multiple branches simultaneously.

`git-workon` clones repositories as bare repos with a worktrees-first layout, then provides commands for creating, finding, and cleaning up worktrees — so switching between branches is just `cd`, not `git stash && git checkout`.

[git-worktree]: https://git-scm.com/docs/git-worktree

## Installation

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
```

### Initialize a new repository

```sh
git workon init                  # bare init in current directory with initial worktree
git workon init my-project       # bare init in my-project/ with initial worktree
```

### Create a new worktree

```sh
git workon new                   # interactive: prompts for name, then base branch
git workon new my-feature        # creates branch + worktree
git workon new my-feature --base main   # branch from main
git workon new my-feature --orphan      # create branch with no parent commits
git workon new my-feature --detach      # detach HEAD in the new worktree
git workon #123                  # create worktree from PR #123 (auto-fetches)
```

### Find an existing worktree

```sh
git workon find                  # interactive: fuzzy picker across all worktrees
git workon find main             # prints path to the 'main' worktree
git workon my-feature            # shorthand for find (prints path)
                                 # note: multiple fuzzy matches also trigger the picker
```

### List worktrees

```sh
git workon list                  # all worktrees
git workon list --dirty          # only worktrees with uncommitted changes
git workon list --ahead          # worktrees with unpushed commits
git workon list --gone           # worktrees whose upstream branch was deleted
```

### Prune stale worktrees

```sh
git workon prune                 # interactive: shows candidates, prompts for confirmation
git workon prune --yes           # skip confirmation prompt (for scripting)
git workon prune --dry-run       # preview without deleting
git workon prune my-feature      # prune a specific worktree
git workon prune --gone          # also prune worktrees whose upstream branch is gone
git workon prune --merged        # also prune worktrees merged into the default branch
git workon prune --keep-branch   # prune worktrees but keep local branch refs
git workon prune --force         # override all safety checks
```

### Rename a worktree

```sh
git workon move new-name                # rename the current worktree
git workon move old-name new-name       # rename a specific worktree
git workon move new-name --dry-run      # preview without renaming
```

### Diagnose workspace issues

```sh
git workon doctor                # check for broken worktrees and missing dependencies
git workon doctor --fix          # automatically repair fixable issues
git workon doctor --dry-run      # preview fixes without applying
```

### Copy untracked files between worktrees

```sh
git workon copy main my-feature --pattern '.env*'
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

After setup, `workon <name>` changes your current directory to the worktree, and `workon new <name>` drops you directly into the newly created worktree.

> **Scripting**: pass `--no-interactive` to `find` and `new`, and `--yes` to `prune`, to suppress all prompts.

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

    # Stacked diffs (Graphite)
    stackModel = auto            # "auto", "graphite", or "none"
    stackWorktreeGranularity = stack  # "stack" (one worktree per stack)
    gtAutoTrack = true           # auto-run 'gt track' after 'workon new'
```

See `man git-workon` or `git workon --help` for full documentation.

## Library

The core logic is published as the [`workon`](https://crates.io/crates/git-workon-lib) crate.
API docs are on [docs.rs](https://docs.rs/git-workon-lib).
