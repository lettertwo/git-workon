# Plan — Review Any Source (M7)

Design locked 2026-07-09. Decisions live in **[ADR-036](../adr/036-review-source-grammar.md)**
(source grammar, per-shape resolution, error posture, completion scope, the
`ChangesetSpan` rename). This doc is the *execution* plan: what lands, in what order, how
each unit is verified. Read the ADR before implementing — this plan does not restate its
rationale. Glossary terms ("Review source", "Changeset span", "Uncommitted layer") are in
[CONTEXT.md](../../CONTEXT.md).

Goal: `git workon review [<source>]` reviews *anything* — stack, uncommitted, ref, range,
PR — not just the auto-detected state. Read-only for committed sources (M5 semantics);
no-arg auto-detect behavior is byte-identical to today.

## Scope (five tracks)

1. **`ChangesetSpan` rename** — `workon::ChangesetSource` → `workon::ChangesetSpan`
   (field `source` → `span`), mechanical across lib + review crates.
2. **Source classifier + keywords** — `Source` enum in the review crate; the binary's
   `Cli` gains one optional positional (`[SOURCE]`); exact-bare-keyword precedence;
   `stack` (Graphite → Git-inference → error) and `uncommitted` resolution; the
   uncommitted-layer suppression seam in the lib.
3. **Rev sources** — `<ref>` shape-aware dispatch (tracked branch → focused stack;
   untracked branch → merge-base changeset; commit-ish → single commit) and
   `a..b` / `a...b` ranges (git-diff semantics, empty side = `HEAD`).
4. **PR source** — `parse_pr_reference` forms at top precedence; `fetch_pr_metadata` +
   `fetch_branch` (fork-aware) → one committed changeset `merge-base(base,head)..head`,
   PR title carried through. No worktree is created.
5. **Completion** — review-binary completer offers keywords + local branches + tags
   (and the RHS after `..`/`...`); git-workon's completer sub-delegates post-subcommand
   words to `COMPLETE=<shell> git-workon-review` (the M6-deferred shell-out).

## Changeset partition (Graphite stack)

Linear stack — each unit extends the classifier the previous one introduced. Base:
the current M3–M6.5 tower tip (`uc-roadmap-reprioritize`/`uc-pty-smoke`), or `main` once
the tower lands. Each unit is land-alone (green + valuable by itself) and
standalone-review (~≤400 non-mechanical lines).

```
<tower tip>
 └─ m7-span-rename        CS1 ── ChangesetSource → ChangesetSpan (mechanical)
     └─ m7-source-keywords CS2 ── Source enum, positional arg, stack/uncommitted keywords
         └─ m7-source-revs  CS3 ── <ref> dispatch + ranges (grammar complete)
             └─ m7-source-pr CS4 ── PR references via pr.rs
                 └─ m7-complete CS5 ── source completion + git-workon sub-delegation
```

Interim behavior is honest at every cut: before CS3, a ref/range argument fails the
keyword match and errors pre-TUI as an unresolvable source; before CS4, `pr-123` falls
through to the ref arm and errors the same way (named, hinted).

## Per-changeset detail

### CS1 — `m7-span-rename` (refactor, lib + review)

- `refactor(lib): rename ChangesetSource to ChangesetSpan`. Type, `Changeset.source`
  field → `Changeset.span`, doc comments, all use sites in `acquire.rs`/`app.rs`/tests.
- Purely mechanical; no behavior change. Verify: full workspace green, `grep -rn
  ChangesetSource` returns nothing.

### CS2 — `m7-source-keywords` (review crate + one lib seam)

- New `source.rs` in the review lib: `Source` enum
  (`Auto | Stack | Uncommitted | Ref(String) | Range{..} | Pr(PullRequest)`) with
  `Source::classify(&str)` implementing the ADR precedence. In CS2 the classifier ships
  with keyword + fallback-to-`Ref` arms only; `Ref` resolution errors as unresolvable
  (real resolution is CS3). Classification is pure → unit-test exhaustively (keyword
  exactness: `Stack` ≠ `stack` keyword? No — exact bare match is case-sensitive `stack`;
  `refs/heads/stack` classifies as `Ref`).
- `Cli` gains `Option<String>` positional `[SOURCE]`; `main.rs` routes
  `None` → `Source::Auto` → existing `resolve_changesets` (unchanged path).
- `stack`: Graphite → `assemble_changesets(.., Graphite)`; else Git-inference
  (`StackModel::Git` — first binary wiring); `NoUpstream` surfaces pre-TUI with a hint
  (set an upstream, or `review uncommitted`).
- `uncommitted`: the single synthetic uncommitted changeset (extract today's
  `resolve_changesets` fallback arm for reuse).
- **Lib seam**: `assemble_changesets` must be able to omit the uncommitted layer
  (ADR-036: layer only when focused on real HEAD). Prefer an explicit parameter over a
  post-filter — a post-filter must also repair the `current` flag, which is subtle.
  CS2 introduces the seam (keywords always run with the layer *on*, since `stack`
  reviews HEAD's stack); CS3 is the first caller that turns it off.
- New error variants in review `error.rs` per ADR-008 — **load `/docs errors` first**.
- Verify: fixture tests for both keyword resolutions in Graphite and plain-git repos
  (sqlite + legacy metadata modes), error cases asserted with `NO_COLOR=1`.

### CS3 — `m7-source-revs` (review crate + acquire)

- `Ref` resolution, dispatched on shape (ADR-036): Graphite-tracked branch →
  `assemble_changesets` focused there, uncommitted layer ON iff the ref is the actual
  `HEAD` branch (first user of the CS2 lib seam); untracked branch → one committed
  changeset, base = merge-base(upstream, else trunk, else error); commit-ish →
  `parent..ref` (root commit: empty-tree base).
- `Range` resolution: split on `...` first, then `..`; empty side → `HEAD`; rev-parse
  each endpoint; `...` → merge-base base. One committed changeset named after the
  source text as typed.
- Empty-but-valid results extend the existing "nothing to review" to name the source.
- Verify: fixture matrix — tracked/untracked/commit/tag shapes; both dot forms;
  `review <current-branch>` == auto-detect output (layer present); reviewing a non-HEAD
  tracked branch on a dirty tree asserts NO uncommitted layer and correct `current`.

### CS4 — `m7-source-pr` (review crate, reuses lib `pr.rs`)

- Classifier gains the PR arm at top precedence (`parse_pr_reference`; also accept
  `pr-123` if the lib parser doesn't already — check first, extend the *lib parser*
  if not, with its existing tests as the pattern).
- Resolution: `check_gh_available` → `fetch_pr_metadata` → `detect_pr_remote` /
  `setup_fork_remote` → `fetch_branch` → merge-base(base, head) → one committed
  changeset, `title` from PR metadata. Every failure pre-TUI, named, hinted.
- Verify: classification unit tests offline; resolution wiring behind the smallest
  testable seam (metadata → changeset mapping fixture-tested with a local "remote";
  the gh-network path itself is exercised manually — record the manual check in the
  changeset description).

### CS5 — `m7-complete` (review crate + git-workon completer)

- Review-binary completer: keywords + local branch names + tag names via git2 ref
  enumeration; when the current word contains `..`/`...`, complete the RHS ref the
  same way. Offline only; no PR numbers.
- git-workon side: the dynamic completer's external-subcommand arm shells out
  `COMPLETE=<shell> git-workon-review -- <partial>` for post-subcommand words
  (M6 CS3 left this seam documented; remember `_CLAP_COMPLETE_INDEX`).
- Verify: completion integration tests per M6's pattern (`COMPLETE=` env protocol),
  asserting keyword + ref candidates and the delegation path.

## Traps / notes for the implementer

- **Load `/docs testing` before any tests; `/docs errors` before error variants.**
- `FORCE_COLOR=3` is set in this environment — output-asserting tests pin `NO_COLOR=1`.
- Verify TUI behavior by instrumenting, never by grepping ratatui frames.
- The Git-inference arm (`assemble_git`) is lib-complete and lib-tested; CS2 only wires
  it. Don't reimplement.
- `resolve_changesets`'s doc comment explains why auto-detect must NOT route plain-git
  repos to `StackModel::Git` — that reasoning stays true; only the explicit `stack`
  keyword takes the Git arm.
- Working-tree leftovers are user WIP — never stage `.claude/settings.json`,
  `.claude/hooks/post-edit-rust.sh`, `docs/diagrams/agent-integration.md`,
  `docs/recipes/agent-integration.md`. `git add` specific files, never `-A`/`-u`.
- Commits: Conventional, single line ≤72 chars, no body/footer.

## Verification gates (green before any changeset is called done)

```bash
NO_COLOR=1 cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo run -p git-workon-review -- <source>   # manual: each source shape renders
```

## Acceptance (RFC M7)

`git workon review <ref>` / `<a..b>` / `pr-123` renders the right changeset(s);
`git workon review <TAB>` completes sources.
