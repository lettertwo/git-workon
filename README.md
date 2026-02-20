# git workon

An opinionated [git worktree][git-worktree] workflow for managing multiple branches simultaneously.

`git-workon` clones repositories as bare repos with a worktrees-first layout, then provides commands for creating, finding, and cleaning up worktrees — so switching between branches is just `cd`, not `git stash && git checkout`.

[git-worktree]: https://git-scm.com/docs/git-worktree

## Installation

### From crates.io

```sh
cargo install git-workon
make install-man   # optional: install man page to /usr/local/share/man/man1/
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

### Create a new worktree

```sh
git workon new my-feature        # creates branch + worktree
git workon new my-feature --from main   # branch from main
git workon #123                  # create worktree from PR #123 (auto-fetches)
```

### Find an existing worktree

```sh
git workon find main             # prints path to the 'main' worktree
git workon my-feature            # shorthand: find, then cd to it
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
git workon prune                 # dry-run by default, shows what would be removed
git workon prune --execute       # actually delete merged/stale worktrees
```

### Copy untracked files between worktrees

```sh
git workon copy-untracked --pattern '.env*' --pattern '.vscode/'
```

## Shell integration

Add worktree-aware `cd` to your shell:

```sh
# bash / zsh
eval "$(git workon shell-init bash)"

# fish
git workon shell-init fish | source
```

After setup, `git workon <name>` changes your current directory to the worktree.

## Man page

```sh
man git-workon
```

## Configuration

`git-workon` uses git config keys under the `workon.*` namespace:

```gitconfig
[workon]
    defaultBranch = main
    postCreateHook = npm install
    copyPattern = .env.local
    autoCopyUntracked = true
    pruneProtectedBranches = main
    pruneProtectedBranches = release/*
    prFormat = pr-{number}
```

See `man git-workon` or `git workon --help` for full documentation.

## Library

The core logic is published as the [`workon`](https://crates.io/crates/git-workon-lib) crate.
API docs are on [docs.rs](https://docs.rs/git-workon-lib).
