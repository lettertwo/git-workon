# git-workon-review: Scaffolding Plan

Status: **accepted** — decisions below are settled (2026-07-05 design sessions); this doc is the execution plan for scaffolding.
Prior art in this repo: [stacked-diffs.md](./stacked-diffs.md), [agent-integration.md](./agent-integration.md).

## What it is

`git-workon-review` is a standalone TUI for reviewing changesets — any branch/ref/range/stack (including reviewing what a coding agent did before it lands). It renders side-by-side diffs with word-level emphasis and tree-sitter syntax highlighting, supports line-precise staging as the accept/reject verb, navigates graphite/git stacks changeset-by-changeset, and can feed review comments back to a coding agent via MCP. It embeds cleanly in an editor terminal (lazygit-style) and runs standalone.

It is the productization of a working Neovim prototype (`~/.config/nvim/lua/app/review/`, ~6k lines Lua, feature-complete through line-precise staging). The prototype is **frozen** (bug fixes only); new features land here first. A renderer spike (`~/Code/review-tui-spike`) validated the ratatui approach — port its modules, don't depend on it.

## Decision log

| Decision | Outcome |
|---|---|
| Positioning | Changeset review tool; not a lazygit competitor. Comments-to-agent is a first-class capability, not a stretch. |
| Home | This workspace, as sibling crate `git-workon-review`. |
| Crate layout | ONE crate, lib+bin targets. lib = review domain (diff parse, word-diff, staging, changeset views); bin = TUI + `mcp` subcommand. No separate core crate until a second consumer exists. |
| Name | Package == binary == `git-workon-review`. `git workon-review` works via git's native `git-*` dispatch. (`git-review` is squatted on crates.io + Gerrit-loaded; `docket` too docker-adjacent; bare `review` superseded by suite framing; `signoff` was the free runner-up.) |
| `git-workon review` dispatch | `git-workon` adds cargo-style external-subcommand dispatch: unknown subcommand → exec `git-workon-<cmd>` on PATH, args passed through. |
| NO `workon` binary | Deliberate: Python virtualenvwrapper keeps the `workon` name. Do not re-propose. |
| Git substrate | git2 throughout, aligned with git-workon-lib. Consequence: the prototype's patch/staging semantics were validated against git CLI — must re-verify against libgit2 (see Trap corpus). Escape hatch if libgit2 apply diverges: shell out to `git apply` for writes only. |
| Stack capabilities | All three land in **git-workon-lib** (not the review crate): (1) needs-restack via `parentBranchRevision` (present in both metadata formats, currently unread), (2) git-inference StackModel for metadata-less repos (in-flight semantics: upstream..HEAD per-commit changesets), (3) changeset assembly (base..head pairs + uncommitted layer + focus). Lib stays diff-free. |
| Lib hygiene | Remove unused `dialoguer`/`env_logger` from git-workon-lib deps; optionally feature-gate the network stack (clone/fetch/auth-git2 behind default-on `network` feature). |
| Fixture | `git-workon-fixture` is the test substrate for both crates. Extend it: SQLite-format graphite metadata mode (the sqlite read path is currently fixture-untested — builder only writes legacy refs blobs) and index-state builders (staged/unstaged/untracked combos). |
| Highlighting | tree-sitter (tree-sitter-highlight), syntect as long-tail fallback. Measured: ts ~0.01ms/line vs syntect ~0.19ms/line, and better output. Grammar set + gotchas are in the spike. |
| View model | Full parity with the prototype's four zoom states (split/combined/unstaged/staged + attributed rendering). If v1 must shrink, cut zoom states — never the comments loop. |
| v1 sources | uncommitted, stack, ref/range. PR deferred (git-workon-lib's `pr.rs` covers much of it later). |
| Comments | MCP: on-disk comment store (`.review/` JSON or sqlite) + `git-workon-review mcp` stdio subcommand serving get/resolve tools; TUI watches the store. Degrades to a plain file convention for non-MCP harnesses. |
| Edit flow | Embedded: `nvim --server $NVIM --remote +<line> <file>`. Standalone: `$EDITOR`. File watcher refreshes on save. |
| Completions | Full clap_complete (unstable-dynamic, already a workspace dep) on the direct binary. Work item: git-workon's dynamic completer enumerates `git-workon-*` on PATH and delegates post-subcommand completion via `COMPLETE=<shell> git-workon-review -- <partial>`. Git-level shims: on demand only. |
| Study first | `jjr` crate (agent jj-stack review surface), `triage-tui`, `wb300` — adjacent tools found during naming research. |

## Reference material

- **Prototype** (`~/.config/nvim/lua/app/review/`): the behavioral spec. Key modules: `diff/parser.lua` (hunk parse + patch synthesis — the crown jewels), `staging.lua` (FIFO queue semantics), `docket.lua` (`_gate` zoom matrix, window topology), `source/stack.lua` + `source/graph/` (graphite walk, git fallback, in-flight semantics), `ui/diff.lua` (rendering + attribution), colocated `*_spec.lua` files. E2E harness: `nvim/tests/review/`.
- **Spike** (`~/Code/review-tui-spike`): port `align.rs` (SBS row pairing + parity invariant), `wordiff.rs` (similar-based spans), `highlight_ts.rs` (grammar set, theme, per-line span splitting; JS exports `HIGHLIGHT_QUERY` singular + separate JSX query; TS/TSX queries concatenate TS-specific-first), `ui.rs` (viewport-sliced rendering), `diff.rs` (parser fallback to `diff --git` header for binary files). Bench mode worth keeping.

## Trap corpus (port as tests FIRST — none of this is guessable)

Hard-won semantics from the prototype, all of which caused real bugs. Each becomes a test before its feature is implemented:

1. **Patch direction rules**: synthesizing a partial patch (line-precise staging) has direction-dependent drop rules. Forward apply (stage): dropped adds omitted, dropped dels → context. Reverse apply (unstage `--cached --reverse`, discard `--reverse`): dropped adds → context, dropped dels omitted — git rejects any partial selection otherwise. Round-trip test both directions + a tripwire asserting forward rules do NOT reverse-apply.
2. **No-newline EOF corruption (silent!)**: a dropped del converted to context carrying the `\ No newline at end of file` marker, followed by a kept add, is ACCEPTED by git apply (exit 0) which concatenates the add onto the no-newline line — corrupt blob, no error. Fix: splice into del+re-add form when kept lines follow. Assert the exact blob bytes.
3. **Whole-file ops for A/D/U statuses**: hunk-level patches can't express creations/deletions (untracked hunk-stage errors; deleted-file hunk-stage stages an EMPTY BLOB). Fall back to file-level ops; line-selection on these REFUSES with a notify.
4. **Staging queue**: FIFO, op stays queued while in flight (remove-before-run double-runs); ops resolve direction from the LIVE index inside the queued op, never from a snapshot (stale-snapshot toggles silently no-op); retry once on `index.lock` contention (~100ms); pcall/catch around ops (a sync throw deadlocks the queue).
5. **Refresh generation/livelock**: refreshes carry a generation seq; a superseded completion must re-snapshot the index signature BEFORE the supersede check returns, or its own diff's stat-cache rewrite echoes into the index watcher and livelocks refresh forever under staging storms.
6. **git2 re-verification**: all of the above were validated against git CLI. Re-run the round-trip corpus against libgit2's apply/index. Divergence → shell out to `git apply` for writes (reads stay git2).
7. **Metadata revisions are snapshots, not refs** (found dogfooding the prototype on this repo, 2026-07-05): graphite's `branch_revision` updates only when gt runs — commits made with plain git (i.e. any commit made outside gt) leave it stale. The prototype used it as the changeset head, so a freshly-committed branch rendered an EMPTY changeset (`head_rev == parent_rev ==` fork point) while still appearing in the stack. Changeset head must resolve the live ref (`refs/heads/<branch>`); `parentBranchRevision` remains the correct BASE (diff-as-authored + needs-restack input) — do not "fix" it to live trunk. Related: the prototype swallows per-changeset diff errors into an empty file list — a failed diff must be distinguishable from a genuinely empty changeset. Test: fixture branch tracked in metadata, then commits added with plain git; assert the changeset spans fork..live-head and that a bad ref surfaces an error, not an empty changeset.

## M2 verdict (git2 vs CLI apply)

The round-trip corpus (`git-workon-review/tests/roundtrip_corpus.rs`) drives every write-path
scenario class from the trap corpus above through `ops.rs`'s entry points against both backends.
Measured result: **0 divergences** across 22 scenarios.

| Scenario class | git2 verdict |
|---|---|
| Whole-hunk stage/unstage/discard | pass |
| Partial stage (adds-only/dels-only/mixed) | pass |
| Partial unstage / partial discard | pass |
| EOFNL per verb (whole-hunk) | pass |
| EOFNL trap-2 splice (partial stage) | pass |
| Multi-hunk file, one hunk staged | pass |
| Space-in-filename header handling | pass |
| Rename (read-side, `diff_committed`) | pass |
| Untracked/added/deleted file ops | pass |
| Line-selection refusals (never reach an applier) | pass |
| Staging storm (mixed stage/unstage/discard, three-way end state) | pass |

Per the plan's decision procedure: 0 divergences means **`Git2Applier` is the default write
path**; `CliApplier` is retained as the corpus's oracle and as the documented escape hatch
(`is_lock_contention` already classifies errors from both backends identically, so the seam has
no additional cost to keep). `Applier` stays a trait specifically so this can flip without
touching call sites if a future libgit2 upgrade regresses.

`tests/roundtrip_corpus.rs` runs both backends on every `cargo test` — it is the permanent guard
this decision rests on. If a future libgit2 upgrade changes apply behavior, `corpus_against_git2`
fails with the specific scenario and divergence class, and the fix is to update
`KNOWN_DIVERGENCES` (or flip the default writer) with that evidence in hand, not to relitigate
this section from memory.

Two tripwire findings from earlier M2 changesets are now pinned as permanent regression tests,
not just corpus coverage:

- **Trap 3 (empty-blob deletion staging)**: `naive_hunk_stage_of_deletion_stages_empty_blob` in
  `git-workon-review/tests/file_ops.rs` — a naive whole-hunk stage of a deletion is accepted by
  `git apply --cached` but stages an empty blob instead of removing the index entry.
- **Trap 2 (EOFNL silent concatenation)**: `naive_unspliced_eofnl_patch_silently_corrupts_the_index`
  in `git-workon-review/tests/line_synthesis.rs` — a dropped deletion converted to context while
  still carrying its `\ No newline at end of file` marker, followed by a kept line, is accepted
  by `git apply` (exit 0) but silently concatenates the two lines into one corrupt line.

## Milestones

- **M0 — workspace plumbing.** New member crate `git-workon-review` (lib+bin, clap, error model matching workspace: thiserror+miette). Toolchain bump (ratatui/tree-sitter won't meet 1.68.2; resolved: workspace-wide `rust-version = 1.88` — no crate had ever inherited the old value, so there was no lib MSRV to preserve). Lib hygiene (drop unused dialoguer/env_logger). CI: tree-sitter C builds. Release posture per [ADR-033](../adr/033-review-crate-workspace-placement.md): `publish = false` keeps the crate out of release-plz and cargo-dist entirely; release-plz wiring is deliberately deferred to the M3 flip — do NOT add a release-plz.toml entry in M0. Acceptance: `cargo build --workspace` green, empty `git-workon-review` binary runs and prints help.
- **M1 — fixture extensions + lib stack capabilities (test-first).** Fixture: sqlite metadata mode (also finally exercises the lib's primary read path), index-state builders. Lib: `parentBranchRevision` read (both formats) + needs-restack; git-inference StackModel; changeset assembly API (`Vec<Changeset> {branch, base_ref, head_ref, title, current, needs_restack}` + uncommitted layer). Acceptance: existing lib tests green + new capabilities spec'd against fixtures in both metadata formats.
- **M2 — trap corpus port.** Diff parser + patch synthesis in the review lib, the six trap items as tests, git2-vs-CLI verdict rendered (and the write-path decision recorded here). Acceptance: round-trip corpus green against real repos. — DONE (2026-07-06): corpus green on both backends; verdict recorded above.
- **M3 — renderer + uncommitted source.** Port spike modules; wire changeset → parsed diff → SBS/inline render; file nav; the uncommitted source end-to-end. Acceptance: dogfood-able read-only review of a dirty worktree.
- **M4 — staging verbs + zoom states.** Queue, hunk/file/line ops (visual-style line selection), the `_gate` zoom matrix, attributed rendering. Acceptance: prototype staging parity, index watcher stable under external writes.
- **M5 — stack + ref sources, outline.** Changeset navigation, outline panel, needs-restack markers, focus semantics (open at current branch; uncommitted adjacent-after, focused when present).
- **M6 — comments + integration.** Comment store + `mcp` subcommand; `$NVIM`/`$EDITOR` edit jump; git-workon external dispatch + completion delegation. Acceptance: full agent loop — review, comment, agent addresses via MCP, re-review.

## Orchestration notes

Main-thread implementation; subagents only for explore/plan/code-review fan-out. Model tiers: design-heavy work on the strongest model; well-understood ports (M2 corpus, M3 spike port) delegate well to mid-tier; mechanical work (fixture builders, CI wiring) to the fast tier. Review each milestone (`/code-review`) before landing; run the full workspace test suite per milestone, not per commit.
