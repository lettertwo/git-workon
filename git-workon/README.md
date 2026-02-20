# git-workon

A git plugin for managing worktrees.

## Installation

```sh
cargo install git-workon
```

## Usage

```sh
git workon clone <url>           # clone as bare repo with worktrees layout
git workon new <branch>          # create a new worktree
git workon #123                  # create worktree from PR #123
git workon find <name>           # print path to a worktree
git workon list                  # list all worktrees
git workon prune                 # remove merged/stale worktrees
git workon move <from> <to>      # rename a worktree and its branch
```

Run `git workon --help` or `man git-workon` for full documentation.

See the [workspace README](../README.md) for installation, shell integration, and configuration details.
