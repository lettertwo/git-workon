# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`git-workon` is a Rust-based CLI tool that provides an opinionated workflow around git worktrees. It simplifies managing multiple branches simultaneously through worktrees, with utilities for cloning, creating, finding, and managing worktrees.

## Workspace Structure

This is a Cargo workspace with four crates:

- **git-workon** (git-workon/): The CLI binary that provides the user-facing commands
- **git-workon-lib** (git-workon-lib/): Core library (published as `workon`) containing the git worktree manipulation logic
- **git-workon-fixture** (git-workon-fixture/): Testing utilities that provide fixture builders and custom predicates for git repository tests
- **git-workon-review** (git-workon-review/): Lib+bin crate for the review TUI domain — diff parsing, staging, changeset views; the binary is the TUI

## File Location Quick Reference

**Critical rule:** Read these files BEFORE modifying them.

**When you need to:**
- Add/modify a CLI command → `git-workon/src/cmd/{command}.rs` (implement Run trait; read an existing command first)
- Add CLI arguments → `git-workon/src/cli.rs`
- Add core worktree logic → `git-workon-lib/src/worktree.rs` (check TODOs for planned extensions)
- Add configuration → `git-workon-lib/src/config.rs`
- Add hooks logic → `git-workon/src/hooks.rs`
- Add file copying logic → `git-workon/src/copy.rs`
- Add PR parsing/fetching logic → `git-workon-lib/src/pr.rs`
- Add library error types → `git-workon-lib/src/error.rs` (concrete enums with `#[derive(Error, Diagnostic)]`; CLI only uses `.into_diagnostic()`)
- Add test fixtures → `git-workon-fixture/src/fixture_builder.rs`
- Add test predicates → `git-workon-fixture/src/predicates/{name}.rs` (extend BEFORE writing tests)
- Add integration tests → `git-workon-lib/tests/` or `git-workon/tests/`
- Find workon root logic → `git-workon-lib/src/workon_root.rs`
- Smart routing logic → `git-workon/src/main.rs` (lines 20-38)
- Add review domain logic → `git-workon-review/src/`
- Add review CLI entry → `git-workon-review/src/main.rs`
- Add review error types → `git-workon-review/src/error.rs` (ADR-008 pattern: concrete enums with `#[derive(Error, Diagnostic)]`)

## Key Architecture Concepts

### Workon Root Discovery
Finds the common ancestor between `.git` dir and working directory to locate the worktree root.
See [ADR-002](docs/adr/002-workon-root-discovery.md) | Key source: `git-workon-lib/src/workon_root.rs`

### WorktreeDescriptor Pattern
Wraps libgit2's `Worktree` type with rich metadata (branch, remote, status checks). Methods marked `unimplemented!()` are planned future features — do not implement unless asked.
Key source: `git-workon-lib/src/worktree.rs`

### CLI Command Structure
Each command struct in `cli.rs` implements the `Run` trait in `cmd/<name>.rs`. `run()` returns `Result<Option<WorktreeDescriptor>>`; `main.rs` prints the path or JSON.
See [ADR-005](docs/adr/005-run-trait-command-dispatch.md) | Key source: `git-workon/src/main.rs`, `git-workon/src/cli.rs`

### Default Command Behavior
PR references (`#123`, `pr#123`, GitHub URLs) → `New` command. Otherwise, routing checks in order: existing worktree → `Find`; local or remote tracking branch with no worktree → `New` (auto-attach); no match → `Find` (shows error).
See [ADR-004](docs/adr/004-smart-routing-default-command.md) | Key source: `git-workon/src/main.rs` (`route_branch_to_command`, `branch_exists`)

## Build, Test, and Quality

```bash
cargo build                                        # build workspace
cargo test --workspace                             # run all tests
cargo test -p git-workon-lib --test suite clone::   # run one file's tests (now a module in the merged `suite` harness)
cargo run -p git-workon -- <command>               # run CLI during development
cargo watch --ignore contrib --ignore man -- cargo check --tests  # watch mode
```

**Pre-commit:** `cargo test` (formatting and linting are handled by Claude hooks)

Inline test/clippy runs go through `cargo-gate test`/`clippy` (a raw `cargo test`/`clippy` in a Bash call is denied) — it serializes cargo, filters output, and records green proofs the Stop hook honors, skipping crates it can already prove clean. The close runs `cargo-gate test --workspace` once per changeset before commit/submit.

## Git Commit Style

**CRITICAL**: Conventional Commits format, enforced by `git-hooks/commit-msg`.

**Format**: `<type>(<scope>): <subject>` — single line, imperative mood, max 72 chars, no emoji, no body/footer.

**Valid types**: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`

**Scopes** (optional): `cli`, `lib`, `fixture`, `review`, `config`, `worktree`, `hooks`, `copy`, `pr`, `completions`, `build`, `release`

**Breaking changes**: append `!` — e.g. `feat(cli)!: change output format`

Run `make install-hooks` to install the commit-msg validation hook locally.

## Priority Rules

1. **CLAUDE.md overrides Claude Code defaults** (commit style, code style, etc.)
2. **Module docs (`//!`) and code define what exists; TODOs define what's planned**
3. **User requests override everything** — clarify if request conflicts with project principles
4. **Testing standards are non-negotiable** — always read `/docs testing` before writing tests, then use FixtureBuilder and custom predicates; existing tests may not demonstrate all available patterns
5. **Simplicity wins** — 3 simple lines beats 1 complex abstraction; write tests while implementing, not after

**If truly unsure, ask the user rather than guessing.**

## Deep Context (on-demand)

Load detailed guides only when needed — they are not in this file to save context:

- `/docs testing` — **always load before writing any tests** (FixtureBuilder, predicates, test patterns)
- `/docs errors` — **always load before adding or modifying error types** (library vs CLI patterns; the wrong pattern is easy to copy from existing code)
- `/docs implementation` — implementation principles and workflow
- `/list-tasks` — overview of all TODOs, FIXMEs, and unimplemented work
- `/next-task` — find and prioritize the next task to work on
- Module-level `//!` comments in source files — design rationale and implementation status
