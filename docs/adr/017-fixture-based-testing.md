# 017 — Fixture-Based Testing with FixtureBuilder and Custom Predicates

## Context

Integration tests for a git worktree tool need to create real git repositories, perform operations, and assert on repository state. Ad-hoc test setup using `Repository::init` directly is verbose, non-declarative, and produces unclear failure messages when assertions fail. Each test would need to repeat the same boilerplate, and failures like `assertion failed` give no hint of what was expected.

## Decision

All test repository creation goes through `FixtureBuilder` (in `git-workon-fixture`), and all assertions on repository state use custom predicates from the same crate. The fixture crate provides three components:

1. **`FixtureBuilder`** — a builder API for creating temporary bare or non-bare repositories with branches, worktrees, remotes, and commits declared upfront.
2. **Custom predicates** — implementations of `predicates::Predicate<Repository>` for checking repository state (branch existence, worktree existence, remote config, HEAD position, etc.). These produce structured failure messages rather than raw boolean panics.
3. **`FixtureAssert` trait** — adds a chainable `.assert(predicate)` method to `Repository` so assertions read naturally.

When a needed fixture configuration or predicate does not exist, it is added to the fixture crate first, then used in the test. Tests never contain raw `assert!()` calls on git2 API results.

## Consequences

- Test failures include the expected state and actual state, making debugging faster.
- Repeated test setup patterns are consolidated in one place (the fixture crate), not duplicated across test files.
- Adding a new predicate or builder method has a one-time cost but benefits all future tests.
- The constraint — always extend the fixture library rather than writing ad-hoc assertions — requires discipline during code review.

## References

- `docs/adr/017-fixture-based-testing-guide.md` — prescriptive patterns, available predicates, and testing checklist
- `git-workon-fixture/src/fixture_builder.rs` — FixtureBuilder implementation
- `git-workon-fixture/src/predicates/` — all custom predicates
- `git-workon-fixture/src/assert.rs` — FixtureAssert trait
