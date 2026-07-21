# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.9.0...git-workon-lib-v0.10.0) - 2026-07-21

### Added

- *(lib)* read parentBranchRevision from both graphite metadata formats

### Fixed

- *(worktree)* keep branch() working when workdir is deleted
- *(cli)* stop stacked list duplicating rows on ghost branches
- *(lib)* resolve graphite metadata files from the git common dir

### Other

- *(lib)* detect gt on PATH without spawning it
- bump workspace rust-version to 1.88
- *(lib)* drop unused dialoguer and env_logger dependencies

## [0.9.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.8.0...git-workon-lib-v0.9.0) - 2026-07-05

### Added

- *(cli)* [**breaking**] replace prune discovery flow with annotated picker

### Fixed

- *(copy)* prune excluded directories from the walk

## [0.7.3](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.7.2...git-workon-lib-v0.7.3) - 2026-06-22

### Other

- *(cli)* update help text, man docs, and doctor coverage

## [0.7.2](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.7.1...git-workon-lib-v0.7.2) - 2026-06-22

### Added

- *(cli)* add prune-fetch, gone hint, and config defaults to prune

## [0.7.1](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.7.0...git-workon-lib-v0.7.1) - 2026-06-15

### Fixed

- *(lib)* filter ghost branches without git refs from enumerate_stacks

## [0.7.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.6.1...git-workon-lib-v0.7.0) - 2026-06-11

### Added

- *(lib)* add create_branch_from_remote with upstream wiring
- *(cli)* [**breaking**] route deleted stack nodes to structured error
- *(lib)* add preferred-remote resolution and ambiguity prompt
- *(lib)* add labeled stash create/apply
- *(lib)* [**breaking**] add checkout primitive and CheckoutError

### Fixed

- *(lib)* [**breaking**] resolve rule 1 by branch and never host on trunk
- *(lib)* [**breaking**] fix stash label matching and conflict handling

### Other

- [**breaking**] pass one repository handle through checkout flow
- *(lib)* collect checkout conflicts with RefCell
- *(lib)* share remote precedence via remote_priority
- *(lib)* remove redundant closures in checkout
- [**breaking**] add resolve_action + Resolution enum

## [0.6.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.5.2...git-workon-lib-v0.6.0) - 2026-06-04

### Added

- *(cli)* [**breaking**] redesign list and find as unified stack-aware tree
- *(cli)* [**breaking**] redesign list --json shape, surface metadata-only stacks
- *(lib)* [**breaking**] rename Stack.branches to Stack.diffs

### Fixed

- *(lib)* read graphite metadata from sqlite for gt >= 1.8

## [0.5.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.4.0...git-workon-lib-v0.5.0) - 2026-05-29

### Added

- *(cli)* create worktree for existing branch with --branch flag

### Fixed

- *(lib)* adapt to git2 0.21 Option→Result API changes
- *(cli)* use graphite trunk instead of hardcoded main in gt track

### Other

- *(deps)* [**breaking**] replace git2_credentials with auth-git2 for git2 0.21

## [0.4.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.3.0...git-workon-lib-v0.4.0) - 2026-05-08

### Added

- *(config)* add stack_model, stack_worktree_granularity, and gt_auto_track accessors
- *(lib)* add StackModel, Stack, StackError, and Graphite detection

## [0.3.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.2.1...git-workon-lib-v0.3.0) - 2026-05-06

### Added

- *(copy)* [**breaking**] rename to 'copy', include ignored files by default

### Fixed

- *(copy)* use hashset for index check, tolerate unknown dest worktree
- *(lib)* use repo config for credentials and tolerate absent gitconfig

## [0.2.1](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.2.0...git-workon-lib-v0.2.1) - 2026-04-17

### Fixed

- *(copy)* skip .git file in worktrees and reduce skip message verbosity

## [0.2.0](https://github.com/lettertwo/git-workon/compare/git-workon-lib-v0.1.0...git-workon-lib-v0.2.0) - 2026-04-15

### Added

- *(lib)* respect IdentityAgent from ~/.ssh/config for SSH auth
- *(lib)* add lock parameter to add_worktree() and --lock flag to new
- *(lib)* implement is_locked() and is_valid() on WorktreeDescriptor

### Fixed

- *(lib)* ensure 'main' fallback covers repo.config() failures
