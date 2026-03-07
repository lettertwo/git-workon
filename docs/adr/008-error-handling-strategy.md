# 008 — Two-Layer Error Handling with Miette

## Context

The project has two distinct roles: a library (`git-workon-lib`) that will be consumed by other Rust programs, and a CLI (`git-workon`) that presents errors to end users. Library consumers who don't use miette should still be able to use the library's errors as standard `std::error::Error` values. The CLI, however, needs rich terminal diagnostics with codes, help text, and colored output.

## Decision

**Library layer** (`git-workon-lib`): All errors are concrete enums derived with both `thiserror::Error` and `miette::Diagnostic`. Each variant carries a `#[diagnostic(code(workon::...))]` attribute and a meaningful error message. Sub-enums (`WorktreeError`, `ConfigError`, `PrError`, `CopyError`, etc.) are collected into `WorkonError` via `#[from]` conversions. Library code never calls `.into_diagnostic()`.

**CLI layer** (`git-workon`): `main()` returns `miette::Result<()>`, which causes miette to pretty-print any unhandled error. Library errors (`WorkonError` and sub-enums) propagate unchanged — they already implement `Diagnostic`. Errors from external crates (e.g. `serde_json`, `dialoguer`) are converted with `.into_diagnostic()`, and `.wrap_err()` adds user-facing context where the underlying error message is insufficient.

## Consequences

- Library errors have stable codes (`workon::worktree::not_found`, etc.) usable in scripts and tests.
- The CLI automatically renders fancy diagnostics without any extra plumbing.
- External library errors use `.into_diagnostic()` only in the CLI; this must be enforced by code review since the compiler does not prevent it.
- The test fixture crate uses `Box<dyn Error>` — no miette needed in tests.

## References

- `docs/diagrams/error-hierarchy.md` — full error type hierarchy
- `docs/adr/008-error-handling-guide.md` — detailed patterns and anti-patterns
- `git-workon-lib/src/error.rs` — `WorkonError` and all sub-enums
