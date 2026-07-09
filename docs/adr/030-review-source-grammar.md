# 030 — Review Source: One Sniffed Positional, Shape-Aware Resolution

Status: accepted (2026-07-09, M7 design session)

## Context

Through M6.5 the review binary's `Cli` is empty: it reviews only what auto-detect finds
(the Graphite stack when one is active, else a single uncommitted changeset). M7 makes it
review *anything* — the RFC's "review any source" — which forces three intertwined
decisions: how a source is spelled on the command line, what changeset(s) each spelling
resolves to, and what happens when resolution fails. This is the binary's entire
user-facing argument surface, so it is expensive to re-shape once muscle memory forms.

Alternatives considered for the spelling: subcommands (`review pr 123`, `review range a b`)
— unambiguous but verbose and unlike git's rev-positional idiom; flags (`--pr`, `--range`)
— noisiest for a daily-driver tool, and sources are mutually exclusive so flags fight.

## Decision

**Grammar — one optional sniffed positional.** `git workon review [<source>]`. No argument
keeps auto-detect unchanged. An argument is classified by precedence:

1. **PR reference** — any form `workon`'s own default command accepts (`123` excluded;
   `pr-123`, `#123`, `pr#123`, GitHub URLs), via git-workon-lib `parse_pr_reference`.
2. **Keyword** — exact bare `stack` or `uncommitted`.
3. **Range** — contains `..` or `...`.
4. **Ref** — everything else, resolved via rev-parse.

**Keywords win; qualify to escape.** Classification happens before rev-parse, so
`review stack` is deterministic regardless of repo state. A branch literally named
`stack` is reviewable via any qualified spelling (`refs/heads/stack`, `heads/stack`) —
only the exact bare word matches the keyword.

**`stack` keyword** — "give me the real stack": Graphite metadata when active, otherwise
git-inference (the lib's already-built `StackModel::Git` arm: one changeset per commit in
`upstream..HEAD`). No metadata and no upstream is a real error, never a silent fall-through
to uncommitted — an explicit ask deserves an explicit failure. This ships the M5-deferred
`StackModel::Git` wiring, scoped to the one keyword that means it.

**`uncommitted` keyword** — always the single uncommitted changeset (M2–M4 behavior),
even in a Graphite repo.

**`<ref>` — shape-aware dispatch.** Match what a person most plausibly means per shape:

- *Graphite-tracked branch* → the whole stack focused at that branch
  (`assemble_changesets` already does exactly this; outline and `]c` nav come along).
- *Untracked branch* → one committed changeset, base = merge-base(upstream if set, else
  repo trunk, else error) — "what this branch adds".
- *Bare commit-ish* (sha, tag, `HEAD~2`) → one changeset spanning just that commit
  (`parent..ref`).

**Ranges — git-diff semantics, both dot forms.** `a..b` → base `a`, head `b` (endpoint
trees, exactly a committed span). `a...b` → base merge-base(a,b), head `b` (the PR-style
"what did b add since diverging"). An empty side defaults to `HEAD`. One committed
changeset either way; git-diff muscle memory transfers unchanged.

**PR — gh metadata + fetch, one changeset.** Reuse git-workon-lib `pr.rs` end-to-end:
`fetch_pr_metadata` (gh CLI) for base/head/title/fork detection, `fetch_branch` for the
objects — no worktree is created; review is read-only. Changeset =
`merge-base(base, head)..head` (GitHub's own three-dot PR diff), PR title carried into the
changeset. Requires gh + network, like `workon #123` today.

**Uncommitted layer only when focused on real HEAD.** The layer rides along exactly when
the thing under review is where the working tree actually is: `stack`, and `<ref>` where
ref is the current `HEAD` branch. Every other source — range, commit, PR, untracked
branch, a tracked branch you're not standing on — is committed-only. Rationale:
uncommitted changes diff against `HEAD`; the lib's unconditional insert-after-current
would attach them to a branch they don't belong to.

**Failures surface before the TUI.** Unresolvable ref, bad range endpoint, missing gh, PR
fetch failure, no-upstream: pre-TUI miette errors naming the offending source text, with a
hint where one exists. Never enter the TUI on a broken source; never fall back to
auto-detect (silently reviewing the wrong thing after a typo is the one surprise a review
tool must not have). A valid-but-empty source keeps "nothing to review" + exit 0, extended
to name the source.

**Completion — keywords + local branches + tags.** Offline git2 ref enumeration only;
after a `..`/`...` prefix, complete the right-hand ref the same way. No PR-number
completion (network in the TAB hot path). This is M6's deferred sub-delegation trigger:
git-workon's dynamic completer now shells out to `COMPLETE=<shell> git-workon-review` for
post-subcommand words.

**Rename `ChangesetSource` → `ChangesetSpan`.** Its doc comment already says "what a
Changeset spans"; the rename frees "source" for the user-facing concept every roadmap
document already uses. Safe while the M1–M6.5 tower is unmerged.

## Consequences

- The review binary gains its first real argument; the `Source` enum
  (Auto | Stack | Uncommitted | Ref | Range | Pr) becomes the seam between CLI parse and
  changeset resolution.
- Stack assembly for a non-HEAD tracked branch must suppress the uncommitted layer — a
  lib-side knob or an acquire-side filter (execution detail, see the M7 plan).
- `review <trunk>` resolves through the untracked-branch arm via its upstream (unpushed
  commits) — an acceptable edge, not a special case.
- Git-inference changesets become reachable from the binary for the first time; its
  per-commit semantics get real exposure.
