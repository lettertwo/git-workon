# git-workon-review: Scaffolding Plan

Status: **accepted** — decisions below are settled (2026-07-05 design sessions); this doc is the execution plan for scaffolding.
Prior art in this repo: [stacked-diffs.md](./stacked-diffs.md), [agent-integration.md](./agent-integration.md).

## What it is

`git-workon-review` is a standalone TUI for reviewing changesets — any branch/ref/range/stack (including reviewing what a coding agent did before it lands). It renders side-by-side diffs with word-level emphasis and tree-sitter syntax highlighting, supports line-precise staging as the accept/reject verb, navigates graphite/git stacks changeset-by-changeset, and can feed review comments back to a coding agent via MCP. It embeds cleanly in an editor terminal (lazygit-style) and runs standalone.

It is the productization of a working Neovim prototype (`~/.config/nvim/lua/app/review/`, ~6k lines Lua, feature-complete through line-precise staging). The prototype is **frozen** (bug fixes only); new features land here first. A renderer spike (`~/Code/review-tui-spike`) validated the ratatui approach — port its modules, don't depend on it.

## Decision log

| Decision | Outcome |
|---|---|
| Positioning | Changeset review tool; not a lazygit competitor. Comments-to-agent is a first-class capability, not a stretch. **Reprioritized 2026-07-08 (direction B):** near-term goal is the author's own daily diff-review + git driver; the agent-loop/comments become the eventual payoff, not the next work. See "Roadmap reprioritized" under Milestones. |
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
| v1 sources | uncommitted, stack, ref/range, **PR** — all folded into **M7 "review any source"** (PR was deferred; now first, via git-workon-lib's `pr.rs`). |
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
Measured result (updated after the 2026-07-06 stack review, see below): **0 divergences** across
23 scenarios.

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
| Executable file (100755) whole-hunk stage | pass (fixed by review — was a divergence, see below) |

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

### Post-verdict corrections (2026-07-06 stack review)

The "0 divergences across 22 scenarios" claim above predates a high-effort stack review that
found two more divergence classes the original corpus missed. Both were fixed in place (in the
M2 changeset that introduced them) and are now pinned in the corpus/regression suite, so the
verdict — `Git2Applier` as the default write path — **stands**; these are corrections to the
evidence, not to the conclusion.

1. **Exec-bit mode handling** (`git-workon-review/src/synthesis.rs`): `PatchText::to_bytes`
   hardcoded `index 0000000..0000000 100644` on every synthesized patch. libgit2 takes the new
   index entry's mode straight from this line, so staging any hunk of a `100755` file via
   `Git2Applier` silently reset its mode to `100644` — a real divergence from `CliApplier`, which
   reads the mode from the working tree and never had this bug. Fixed by threading the real mode
   (`FileChange::old_mode`/`new_mode`, from `delta.{old,new}_file().mode()`) onto `PatchText` and
   swapping it in `PatchText::invert`. Pinned by the `executable_whole_hunk_stage` corpus
   scenario (table above) and by `synthesis.rs`'s own `whole_hunk_patch_carries_real_mode_into_index_line`/`invert_swaps_old_and_new_mode`
   unit tests.
2. **Kept-EOFNL-deletion under `base == New`** (`git-workon-review/src/synthesis.rs`): a KEPT
   deletion carrying `missing_newline: true`, followed by a dropped addition converted to context
   (`base == New`'s drop rule), produced a hunk where the two backends actually DISAGREED rather
   than merely diverging in end state: `CliApplier` accepted it and silently concatenated the
   next line onto the no-newline deletion (the same class of corruption as the original trap-2
   finding); `Git2Applier` rejected the patch outright (`invalid patch hunk`). In this instance
   git2 was the SAFE side — refusing a malformed patch is preferable to silently corrupting a
   file — which is itself evidence for, not against, the `Git2Applier`-default verdict. Fixed by
   extending the trap-2 splice (`splice_eofnl_context_lines`) to also rewrite a kept deletion's
   own bytes (real trailing `\n`, marker dropped) when a later emitted line is context. Pinned by
   `kept_eofnl_deletion_needs_splice_under_base_new` in `git-workon-review/tests/line_synthesis.rs`
   (covers both backends via `Discard`); not duplicated into the corpus since that test already
   exercises the identical fixture/selection/direction against both appliers end-to-end.

## Milestones

- **M0 — workspace plumbing.** New member crate `git-workon-review` (lib+bin, clap, error model matching workspace: thiserror+miette). Toolchain bump (ratatui/tree-sitter won't meet 1.68.2; resolved: workspace-wide `rust-version = 1.88` — no crate had ever inherited the old value, so there was no lib MSRV to preserve). Lib hygiene (drop unused dialoguer/env_logger). CI: tree-sitter C builds. Release posture per [ADR-033](../adr/033-review-crate-workspace-placement.md): `publish = false` keeps the crate out of release-plz and cargo-dist entirely; release-plz wiring is deliberately deferred to the M3 flip — do NOT add a release-plz.toml entry in M0. Acceptance: `cargo build --workspace` green, empty `git-workon-review` binary runs and prints help.
- **M1 — fixture extensions + lib stack capabilities (test-first).** Fixture: sqlite metadata mode (also finally exercises the lib's primary read path), index-state builders. Lib: `parentBranchRevision` read (both formats) + needs-restack; git-inference StackModel; changeset assembly API (`Vec<Changeset> {branch, base_ref, head_ref, title, current, needs_restack}` + uncommitted layer). Acceptance: existing lib tests green + new capabilities spec'd against fixtures in both metadata formats.
- **M2 — trap corpus port.** Diff parser + patch synthesis in the review lib, the six trap items as tests, git2-vs-CLI verdict rendered (and the write-path decision recorded here). Acceptance: round-trip corpus green against real repos. — DONE (2026-07-06): corpus green on both backends; verdict recorded above.
- **M3 — renderer + uncommitted source.** Port spike modules; wire changeset → parsed diff → SBS/inline render; file nav; the uncommitted source end-to-end. Acceptance: dogfood-able read-only review of a dirty worktree. — DONE (2026-07-06): combined-zoom read-only review with SBS + inline layouts, collapsed context gaps, word-diff emphasis, tree-sitter highlighting (spike's 8 grammars; syntect deferred), file/hunk nav; dogfooded against a dirty worktree. Port note: the spike's `compose_segments` had a latent first-match span-precedence bug that silently dropped word-level emphasis — fixed here (reverse-order lookup), pinned by a three-way bg test in `render.rs`.
- **M4 — staging verbs + zoom states.** Queue, hunk/file/line ops (visual-style line selection), the `_gate` zoom matrix, attributed rendering. Acceptance: prototype staging parity, index watcher stable under external writes. Design locked 2026-07-06 (plan artifact `iron-lattice`): (1) staging = prototype parity — verbs act only in unstaged/staged panes, combined refuses, direction = pane role (combined-native toggle deferred); (2) cursor-primary nav in all views, scroll derived; (3) full 4-state zoom (`split→combined→unstaged→staged`) with per-file `_gate` downgrade and stacked split panes (per-pane cursor, `w` focus), no collapse debounce; (4) runtime stays sync — poll `IndexSignature` on Tick, synchronous re-diff (no threads/notify dep); (5) queue enqueue+drain same beat, refresh, re-snapshot; (6) footer-swap for refusals/errors + discard confirm; (7) attribution via a new pure `attribute.rs` (membership sets keyed by lnum); (8) line selection in both layouts (inline one-sided, SBS row-pair). — DONE (2026-07-07): shipped as EIGHT changesets `m4-cursor → m4-zoom → m4-attribute → m4-notify → m4-refresh → m4-stage → m4-select → m4-watch` (staging split into hunk/file vs line selection; refresh pulled out as shared infra for stage + watch). Stack-reviewed continuously on the main thread; two real bugs caught by review, not by agent tests: (a) m4-zoom sub-view panes rendered worktree text where index text belonged — fixed with per-role blob sourcing (`read_index_blob`); (b) m4-select applied a multi-hunk line selection as N independent patches, which libgit2 rejects because each per-hunk patch's line numbering assumes the others are present — fixed by merging into ONE `PatchText` (`ops::apply_line_selections`), pinned by a line-shift tripwire test. Acceptance met: staging parity dogfooded against real git (stage/unstage/discard hunk/file/line, partial-hunk selection); index watcher confirmed live (external `git add` auto-refreshes on the next Tick — the watcher polls `.git/index`'s signature, so it catches index writes, not bare worktree edits, matching its name). Runtime stayed sync (no threads); combined-native staging toggle and spike `--dump`/`--bench` modes remain deferred.
- **M5 — stack + ref sources, outline.** Changeset navigation, outline panel, needs-restack markers, focus semantics (open at current branch; uncommitted adjacent-after, focused when present). Design locked 2026-07-07 (plan artifact `cairn-ledger`, 9 forks): (1) source = per-changeset `ChangesetView`, committed changesets built via `DiffState::from_committed` (empty staged/unstaged sub-models); (2) mode = derived `is_committed` + targeted guards, leaning on the existing `effective_zoom` collapse (empty sub-diffs → combined-only for free); (3) outline = left side pane, all four modes (flat/tree/stack/stack-tree); (4) load = hybrid (eager per-changeset `DiffState`, lazy per-file `FileView`); (5) nav = continuous `]f`/`[f` across the stack + `]c`/`[c` changeset jumps; (6) open-at = honor the lib's `current` flag; (7) source scope = auto-detect Graphite else single uncommitted changeset (M2–M4 preserved, backward-compatible); (8) changeset indicator = new top winbar; (9) needs-restack = first-class glyph + amber color (the lib gives a real boolean, unlike the prototype's title-string suffix). — DONE (2026-07-07): shipped as FOUR changesets `m5-stack-source → m5-changeset-nav → m5-outline-core → m5-outline-tree`, each delegated to an `implementer` subagent and main-thread diff-read before the next landed. The M1 lib already provided `assemble_changesets` + the `diff_changeset` router, so M5 was almost entirely review-App wiring; the uncommitted layer becomes one changeset *inside* the stack, keeping all of M4's staging/zoom/attribution working on it while committed changesets render read-only. Two correctness fixes surfaced during implementation, neither in the plan: (a) a committed changeset's combined-role old side must read its `base` commit's tree, not live `HEAD` (`old_side_tree_for`); (b) skipping attribution for committed changesets is not just a guard — without it `Attribution::build(None, None)`'s empty sets miscolored every Add cell as "already staged" (dim), pinned by a render test. Acceptance met: dogfooded against this repo's own live 33-changeset Graphite stack via a PTY harness (winbar changeset counter, `]c`/`[c` nav, outline flat/stack/tree/stack-tree modes with correct tree guides, open-on-uncommitted-layer focus) — a clean exit, no panic, exercising the real `resolve_changesets`→`assemble_graphite` path the hand-built unit tests don't. Full workspace green (41 suites, 804 tests, 0 fail), clippy `-D warnings --all-targets --all-features` clean. Deferred: Git-inference (`StackModel::Git`) and explicit ref-range review (the broader "ref sources") — auto-detect ships Graphite-or-uncommitted only; a fixed 35-col outline with no narrow-terminal handling.
- **M6 — git-workon CLI integration.** Ordered first: dependency-free, lowest-risk, and it unlocks dogfooding every later milestone through the real `git workon review` entry point (not `cargo run`). Cargo-style external-subcommand dispatch — `git-workon`'s unknown subcommand execs `git-workon-<cmd>` on PATH with args passed through (none exists today; `Cmd` is a closed enum), so `git workon review` works via git's native `git-*` dispatch. Plus completion: the review binary gains `CompleteEnv` (its `Cli` is currently empty) so it is a `COMPLETE=` responder, and git-workon's dynamic completer enumerates `git-workon-*` on PATH and surfaces them as top-level subcommand candidates (so `git workon <TAB>` offers `review`). **Post-subcommand sub-delegation** (`git workon review <TAB>` → shell out to the review binary's completer) is **deferred, not built**: the review binary's `Cli` is currently empty (zero candidates), and MCP lands as `git workon mcp` (not a review subcommand — see M9), so there is nothing to delegate today. Its real trigger is *not* MCP — it's whenever the review binary gains its source-selector arg (`stack | uncommitted | <ref> | <ref..range> | pr-####`, the deferred v1 sources), whose values (refs, ranges, PR numbers) are genuinely completion-worthy. Wire delegation then, against that real surface; the review binary is already a `COMPLETE=` responder, so only the git-workon-side shell-out remains. Acceptance: `git workon review` dispatches with args through; `git workon <TAB>` lists external subcommands including `review`. DONE (2026-07-07): shipped as THREE changesets `m6-dispatch → m6-review-complete → m6-complete-enum` — (1) manual pre-parse PATH intercept (`dispatch.rs`), NOT clap `allow_external_subcommands` (which would break the flattened-`find.name` default-command routing); (2) review binary as `COMPLETE=` responder; (3) top-level external enumeration in the completer. Two seam facts surfaced: the clap_complete bash protocol needs `_CLAP_COMPLETE_INDEX` (word position) or it emits "no completion generated", and an empty `Cli` yields zero candidates (which is what made sub-delegation pointless to build).
- **M6.5 — everyday-usability pass (keybindings + theming + view-config).** Inserted ahead of M7 (2026-07-07): comments are deprioritized until the tool is usable for the author's own everyday review work. Keybindings and theming were never milestones — they were baked in as hardcoded values during M3–M5 (a `match` in `tui.rs`, a `const … Color::Rgb` block in `render.rs`). This pass makes both user-configurable and adds discoverability, plus gives previously-hardcoded view settings a config home. Design locked 2026-07-07; two ADRs: [ADR-034](../adr/034-review-git-native-config-schema.md) (git-native config schema — `workon.review.*`, action-as-key per-view keymaps, token grammar) and [ADR-035](../adr/035-review-theming-base16-hybrid.md) (hybrid base16 theming, render-time color resolution, terminal-derived `auto`). Scope: (1) `ReviewConfig` reader — the review binary reads git config for the first time; (2) action registry + configurable per-view keymaps, defaults unchanged; (3) help surface (persistent curated per-view footer + `?` overlay); (4) base16 `Theme` primitive + render-time resolution refactor (`FgSpan` carries capture index); (5) curated dark+light schemes + `theme=dark|light`; (6) `theme=auto` terminal-derivation OSC probe with curated fallback; (7) view-config (`outline.width`/`mode`, `diff.layout`/`zoom`). Full plan: `docs/plans/review-usability-pass.md`. Acceptance: rebind any diff/outline/global action via `git config`; `?` overlay + footer render the resolved map; `theme` selects auto/dark/light with terminal-derived `auto` degrading to curated on probe failure; view defaults honored from config. Comments (M7) resume after.
### Roadmap reprioritized 2026-07-08 — personal daily-driver first (direction B)

The remaining roadmap is resequenced around the tool being **the author's own everyday diff-review + git surface**, not the agent-review loop (which becomes the eventual payoff once the tool is lived-in). This **supersedes the "comments next" ordering** and the decision-log **Positioning** / **v1 sources** rows above. Nothing past M6.5 is built, so renumbering is free. The old M7 (comments)/M8 (edit)/M9 (MCP) content is *relocated*, not dropped: edit-flow graduates into the daily-core (new **M10**); comments + MCP defer together into the agent-loop milestone (new **M13**). Ordering rationale is inline per bullet.

- **Prerequisite — Land M3–M6.5** (process, parallel to features; not a numbered milestone). QA the unmerged M3→M6.5 tower → merge to `main` → reliable install (a local build on PATH is enough to dogfood; the [ADR-033](../adr/033-review-crate-workspace-placement.md) release/homebrew "M3 flip" is a deferrable sub-decision). Gates real daily use regardless of features. QA checklist in memory `review-tui-priority-everyday-use` (`theme=auto` responsiveness, `theme=light` canvas, committed-changeset nav).

- **M7 — review any source.** A source selector — `stack | uncommitted | <ref> | <ref..range> | pr-####` — so the tool reviews *anything*, not just the auto-detected stack/uncommitted state. **Ordered first:** it is the tool's core *read* identity, read-only (low-risk), independent of the write verbs, and the M1/M5 lib already provides `assemble_changesets` + the `diff_changeset` router — mostly source-arg parse → resolve to changeset(s) → existing pipeline. PR support reuses git-workon-lib's `pr.rs`. Also **completes M6's deferred completion sub-delegation** (its trigger was exactly this arg gaining completion-worthy values). Acceptance: `git workon review <ref>` / `<a..b>` / `pr-123` renders the right changeset(s); `git workon review <TAB>` completes sources.

- **M8 — commit operations.** Commit the staged changes without leaving the TUI — message editor (inline vs `$EDITOR`), Conventional-Commit-aware (enforced by `git-hooks/commit-msg`); **amend** the current commit; **fixup/absorb** staged changes into an earlier changeset in the stack. Closes the review→stage→**commit** loop — the acute daily-driver gap. Acceptance: stage in the TUI, commit/amend/fixup, verified against real git.

- **M9 — stack operations.** Graphite stack verbs from the TUI: create a changeset/branch from staged (`gt create`), restack (`gt restack`), submit → PRs (`gt submit`), checkout/switch to a changeset (nav exists; actual checkout does not). Advanced reorder/fold/split deferred within. Builds on M8 — commit → create → submit is the shipping spine of a stacked workflow. Acceptance: create/restack/submit/checkout a changeset from the TUI against a real gt stack.

- **M10 — editor jump / edit flow** *(was M8)*. Jump from a diff line to `file:line` — embedded `nvim --server $NVIM --remote +<line> <file>`, standalone `$EDITOR +<line> <file>` (detect via `$NVIM`); file watcher refreshes the diff on external save — port the prototype's debounced repo-root watcher (`FocusGained` fallback, viewport-preserving refresh, selection clamp; the Neovim mechanism doesn't translate, the behavior does). **Graduated from agent-loop into daily-core:** under (B) you review and want to *fix* the thing. Acceptance: jump opens the right file+line; saving refreshes without losing viewport.

- **M11 — polish.** Worktree-switch hub in the TUI (surface git-workon's create/find/prune/switch so the TUI is a hub — vs staying review-only; decide during design) + in-diff navigation (fuzzy jump-to-file, search-in-diff, context expand/collapse, ignore-whitespace toggle, copy `path:line`). Acceptance: per the design cut.

- **M12 — conflict resolution** *(stretch)*. Resolve merge/rebase conflicts in the SBS view. Large surface; may not make v1.

- **M13 — agent loop** *(the eventual north star; was M7 comments + M9 MCP)*. On-disk comment store keyed to `(changeset_id, path, side, lnum)` with a rebase-survival anchoring strategy + TUI comment UX (create/view/resolve, store-watch refresh), and a **unified `git workon mcp`** stdio server bridging git-workon-lib worktree tools (`agent-integration.md` Model C) *and* the comment store. **Open forks (unchanged, resolve at design time):** comment-store home — a lib both the review crate and `git-workon` depend on, since `git workon mcp` is a second consumer (reopens the "no separate core crate" decision); the anchoring strategy; MCP crate/transport (`rmcp` vs hand-rolled JSON-RPC-over-stdio). Deferred behind the daily-driver work.

## Orchestration notes

Main-thread implementation; subagents only for explore/plan/code-review fan-out. Model tiers: design-heavy work on the strongest model; well-understood ports (M2 corpus, M3 spike port) delegate well to mid-tier; mechanical work (fixture builders, CI wiring) to the fast tier. Review each milestone (`/code-review`) before landing; run the full workspace test suite per milestone, not per commit.
