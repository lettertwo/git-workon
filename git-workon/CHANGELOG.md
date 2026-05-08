# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0](https://github.com/lettertwo/git-workon/compare/git-workon-v0.2.1...git-workon-v0.4.0) - 2026-05-08

### Added

- *(cli)* smart base default and gt track in new command when stack-active
- *(cli)* add stack-member match path to find command
- *(cli)* render stack tree in list when stack-active
- *(cli)* add global --no-stack flag and propagate to commands
- *(cli)* add stack config checks and gt detection to doctor

## [0.2.1](https://github.com/lettertwo/git-workon/compare/git-workon-v0.2.0...git-workon-v0.2.1) - 2026-05-06

### Added

- *(cli)* detect renamed workon.autoCopyUntracked in doctor

## [0.2.0](https://github.com/lettertwo/git-workon/compare/git-workon-v0.1.3...git-workon-v0.2.0) - 2026-05-06

### Added

- *(copy)* [**breaking**] rename to 'copy', include ignored files by default

### Fixed

- *(copy)* resolve worktree arg from CWD and make 'to' optional
- *(copy)* use hashset for index check, tolerate unknown dest worktree

## [0.1.3](https://github.com/lettertwo/git-workon/compare/git-workon-v0.1.2...git-workon-v0.1.3) - 2026-04-17

### Added

- *(cli)* show worktree dir as primary label with trailing branch annotation
- *(build)* install man page and completions via Homebrew formula

### Fixed

- *(copy)* skip .git file in worktrees and reduce skip message verbosity

## [0.1.2](https://github.com/lettertwo/git-workon/compare/git-workon-v0.1.1...git-workon-v0.1.2) - 2026-04-15

### Added

- *(cli)* add --include-locked to prune and is_locked to JSON output
- *(lib)* add lock parameter to add_worktree() and --lock flag to new
- *(cli)* emit structured JSON errors in --json mode

### Other

- *(dist)* remove leading article from description per brew style guide

## [0.1.1](https://github.com/lettertwo/git-workon/compare/git-workon-v0.1.0...git-workon-v0.1.1) - 2026-03-14

### Other

- update Cargo.toml dependencies

## [0.1.0](https://github.com/lettertwo/git-workon/releases/tag/git-workon-v0.1.0) - 2026-03-14

### Added

- *(copy)* add --exclude flag to copy-untracked command
- *(clone)* add per-phase spinners and progress for clone
- *(new)* add per-phase spinners and copy progress for PR worktrees
- *(prune)* show spinner during worktree status discovery
- *(hooks)* show spinner while hook is running
- *(output)* add create_spinner() helper for consistent progress UI
- *(copy)* replace status walk with ignore crate and add progress reporting
- *(copy)* git-aware untracked file copying with CoW syscalls
- *(prune)* delete local branch refs when pruning worktrees

### Fixed

- *(build)* write man page only to OUT_DIR, not source tree
- *(build)* replace invalid 'git' crates.io category slug
- *(prune)* reduce false positives for --gone pruning

### Other

- *(copy)* use shared create_spinner() helper
- *(prune)* document --keep-branch, --force, and RemoteGone safety changes
- *(deps)* bump the cargo-dependencies group across 1 directory with 12 updates ([#6](https://github.com/lettertwo/git-workon/pull/6))
- *(release)* add release-plz and cargo-dist pipeline
- cargo fmt
- *(man)* commit man file
- *(cli)* improve man install
- *(man)* pull README details into manpage
- *(cli)* add first pass at CLI docs
- add release-please
- Remove unimplemented config subcommand; add to doctor
- Add doctor subcommand
- Add help text to dynamic completions
- Remove unplanned/outdated TODO
- Add --no-color flag
- Fix colored output from shell integration
- Integrate dynamic arg value completion
- Add ls and mv aliases
- Refactor shell completion to use clap complete
- Auto-detect shell
- Implement shell integration
- Add interactive new tests
- Add interactive prune tests
- Add interactive find tests
- Add hook timeout protection
- Improve verbose output
- Add --json global flag
- Add details to list formats
- Add is_head_detached predicate
- Add colorized output
- Rename prune --allow-unpushed to --allow-unmerged
- Add prune --force flag
- Replace pr implementation with gh cli integration
- Replace ROADMAP with module comments and stubs
- Implement move command
- Add interactive modes to find and new commands
- Add status filtering to list command
- Add targeted prune <name> support
- Add automatic file copying and simplify defaults
- Add PR support
- Add error types to lib and wrap cli diagnostics
- Add enhanced file copying with pattern support
- Add post-creation hooks support
- Add workon new --base flag
- Add protected branches support to prune command
- Refactor new.rs tests with helper function
- Refactor prune.rs tests with helper functions
- Remove fixture.with and add fixture.assert
- Add prune --merged flag
- Add prune --allow-dirty and --allow-unpushed flags
- Add --gone flag to prune command
- Run cargo fmt
- Implement WorktreeDescriptor branch() and is_detached() methods
- Implement prune command
- Refactor to use BranchType enum and create initial commit for orphans
- Implement worktree options (orphan, detach)
- Add new command tests
- Update init tests
- Fix clippy warnings
- Add clone tests
- Fix build failures due to missing dirs
- Refactor around worktree path output
- Capture some notes about unimplemented features
- Use unimplemented!
- Make switch the default subcommand
- Add basic list cmd impl
- Add basic new cmd impl
- Infer clone directory from url
- Upgrade deps and replace anyhow with miette
- Extract git-workon-lib
