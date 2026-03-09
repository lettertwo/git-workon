# 003 — Three-Crate Cargo Workspace

## Context

The project has three distinct concerns: a reusable library of git worktree operations, a CLI binary that wraps that library, and test infrastructure used across both. Combining all of these into a single crate would couple CLI-specific dependencies (clap, dialoguer) into the library, prevent the library from being published independently, and make it impossible to share test utilities without circular dependencies.

## Decision

The workspace is split into three crates with a strict dependency hierarchy:

- **`git-workon-lib`** (published as `workon`) — core library; depends only on `git2`, `miette`, and `thiserror`. No CLI dependencies.
- **`git-workon`** — CLI binary; depends on `git-workon-lib` plus `clap`, `dialoguer`, and `miette`.
- **`git-workon-fixture`** — test utilities; depends on `git-workon-lib` plus `assert_cmd`, `assert_fs`, and `predicates`. Used only in `[dev-dependencies]`.

Shared dependency versions are declared once in the root `[workspace.dependencies]` and referenced with `{ workspace = true }` in each crate's `Cargo.toml`.

## Consequences

- The library can be published to crates.io and consumed by other tools without pulling in CLI dependencies.
- The fixture crate can be a `dev-dependency` of both the library and CLI without creating cycles.
- Adding a new cross-cutting dependency requires updating the workspace root, not individual crates.
- Three `Cargo.toml` files to maintain, but the workspace root keeps versions in sync.

## References

- `docs/diagrams/architecture.md` — crate dependency diagram
- `Cargo.toml` — workspace definition
