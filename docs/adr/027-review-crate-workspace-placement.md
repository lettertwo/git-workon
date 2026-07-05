# 027 — Review Crate Workspace Placement

## Context

The RFC (`docs/rfc/workon-review.md`) defines `git-workon-review`, a standalone TUI for reviewing changesets, as a fourth sibling crate in this workspace (ADR-003). The existing release pipeline (ADR-020) auto-publishes any new publishable crate on the next `main` push — release-plz's `release` job publishes any registry-unmatched package without waiting for a release PR — and cargo-dist auto-includes any publishable bin crate as a distributed App. Both behaviors are wrong for a crate that starts as an empty scaffold.

## Decision

Add `git-workon-review` as a sibling crate: lib target `workon_review`, bin target `git-workon-review`.

- **`publish = false`** in the crate's `Cargo.toml` is the single knob that keeps it out of both release-plz and cargo-dist, following the `git-workon-fixture` precedent (proven across ~20 releases).
- **`[package.metadata.dist] dist = false`** is set explicitly as well. It is redundant today (`publish = false` already excludes the crate) but is the tripwire for the M3 flip: removing `publish = false` alone, without also deciding this field, would silently make cargo-dist ship the binary.
- **Independent versioning**: the crate does not join `version_group = "main"` in `release-plz.toml`. Joining would lockstep its version to the CLI's and cross-bump the CLI on every review-crate change.
- **Workspace `rust-version` bumped to `1.88`** (ratatui 0.30's floor). `clap` 4.6 already required 1.85, so the workspace's previous `1.68.2` declaration was already unsatisfiable in practice; only the fixture crate had ever inherited the field. `rust-version.workspace = true` is added to all four crates so the field is real everywhere.

## Consequences

- The crate builds and tests in CI from the start (M0) without appearing in crates.io or in any cargo-dist release artifact.
- The M3 flip (when the review binary is ready to distribute) requires:
  1. Remove `publish = false` from `git-workon-review/Cargo.toml`.
  2. Add `[[package]] name = "git-workon-review"` to `release-plz.toml`, with **no** `version_group` — independent versioning is intentional, not an oversight to fix later.
  3. Keep `dist = false` until binary distribution is designed. The homebrew publish job in `.github/workflows/release.yml` patches **every** `Formula/*.rb` with `git-workon`'s man page and completions install lines; it must be reworked before a second binary can safely flow through it. `release-plz.yml`'s `dist` dispatch step is also hardcoded to fire only for `package_name == "git-workon"` and needs updating too.
- Until the M3 flip, the crate's version in its own `Cargo.toml` is cosmetic — release-plz never touches it.

## References

- `docs/rfc/workon-review.md` — RFC defining the review crate
- [ADR-003](003-three-crate-workspace.md) — workspace structure this crate joins
- [ADR-019](019-ci-quality-gates.md) — CI gates the new crate is subject to
- [ADR-020](020-two-tool-release-pipeline.md) — release pipeline whose auto-publish/auto-dist behavior this ADR opts the crate out of
