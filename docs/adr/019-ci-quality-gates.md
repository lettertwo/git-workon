# 019 — CI Quality Gates and Conventional Commits Enforcement

## Context

Conventional commits are load-bearing in this project: release-plz reads commit history to determine version bumps and generate changelogs. A single non-conforming commit on `main` can corrupt the release process. Local git hooks (`git-hooks/commit-msg`, `git-hooks/pre-push`) provide fast developer feedback but are opt-in — they must be installed manually via `make install-hooks` and are never enforced automatically.

CI must therefore be the authoritative enforcement layer. Additionally, the project targets multiple platforms (macOS + Linux), so tests must run on both to catch platform-specific issues (filesystem behavior, path handling, etc.).

Dependabot manages dependency freshness across three ecosystems (Cargo packages, Rust toolchain, GitHub Actions). Its commit messages must conform to Conventional Commits so that automated PRs pass the same validation as human-authored ones.

## Decision

The CI workflow (`.github/workflows/ci.yml`) defines four jobs:

1. **conventional-commits** — validates PR title and all non-merge commits against the Conventional Commits pattern. Skipped for Dependabot PRs (their commit format is configured separately) and release-please branches (legacy, not currently used).
2. **fmt** — runs `cargo fmt -- --check` to enforce consistent formatting.
3. **clippy** — runs `cargo clippy --all-targets --all-features -- -D warnings` to catch lints as errors.
4. **test** — runs `cargo test --workspace` on a matrix of `ubuntu-latest` and `macos-latest` with `fail-fast: false` so both platforms always report results.

Local hooks mirror CI for fast feedback:
- `git-hooks/commit-msg` — validates individual commit messages before they land.
- `git-hooks/pre-push` — runs tests before pushing.

Dependabot (`.github/dependabot.yml`) is configured with `commit-message.prefix` set to `"build"` for Cargo and Rust toolchain updates and `"ci"` for GitHub Actions updates, with `include: "scope"` so generated commit subjects match the Conventional Commits pattern. Dependabot PRs are excluded from the `conventional-commits` job because their titles and commit messages are already validated by format configuration.

## Consequences

- Every PR is validated against Conventional Commits before merge, protecting the release pipeline.
- Cross-platform test coverage on macOS and Linux runs on every push and PR.
- Dependabot PRs pass commit validation automatically, requiring no manual intervention.
- Local hooks are still opt-in, but developers who install them get the same checks locally before pushing.
- The `conventional-commits` job requires `pull-requests: write` to post status checks.

## References

- `.github/workflows/ci.yml` — CI job definitions
- `.github/dependabot.yml` — Dependabot commit message configuration
- `git-hooks/` — local hook scripts
- `Makefile` — `make install-hooks` target
- [ADR-020](020-two-tool-release-pipeline.md) — release pipeline that depends on conventional commits
