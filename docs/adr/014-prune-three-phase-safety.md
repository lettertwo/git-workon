# 014 — Prune: Always-On Analysis with an Interactive Picker

> Supersedes the original three-phase design (candidate collection gated by
> `--gone`/`--merged`, list+confirm, and a gone-upstream hint). The safety-check
> ordering and override flags are unchanged; what changed is *visibility* and the
> interaction model.

## Context

Pruning worktrees is destructive and hard to undo: it deletes the working directory and removes the git worktree registration. The original design gated *visibility* of gone/merged worktrees behind `--gone`/`--merged`, treated named worktrees as fundamentally different from filtered ones, and surfaced a "gone upstream" hint on every bare run — three separate mental models for what is ultimately one question: "what's stale, and is it safe to delete?"

v2 collapses this into one analysis pipeline that always runs in full, with `--gone`/`--merged`/naming only changing what's *pre-selected*, and an interactive multi-select picker replacing the list+confirm flow and the hint.

## Decision

**Analysis is always total.** Every worktree in scope is evaluated for all signals — `BranchDeleted`, `RemoteGone`, `Merged(target)`, and (added later) `PrMerged(number)` — plus safety state (dirty, unmerged, locked, protected). `--gone` and `--merged` no longer gate whether a worktree is considered; they only decide whether its signal counts as **active** (pre-checked / auto-pruned). `BranchDeleted` and `PrMerged` are always active. Fetch stays opt-in (`--fetch` / `workon.pruneFetch`); without it, gone annotations use cached refs, which can only under-report.

**Scope**: bare `prune` considers every worktree except the default one, and only rows carrying at least one signal are shown. `prune <name>...` narrows the scope to exactly those worktrees (matched by worktree name or branch name) — nothing else is shown or touched. Any name that doesn't match is a hard error (`workon::prune::names_not_found`, nonzero exit) listing every miss, before anything is deleted. A named worktree with no signal at all still shows up, annotated "not prunable" — naming is how a healthy tree gets pulled into view — but it needs `--force` to actually be pruned (there's no affirmative reason otherwise). A named worktree that does carry a real issue (dirty, or unmerged commits) is not "healthy" and only needs the matching override (`--allow-dirty`/`--allow-unmerged`), not `--force`. The default worktree is excluded from the candidate pool unconditionally — naming it is an unmatched-name error even with `--force`.

**Safety checks** run in the same order as before, and are unaffected by whether a signal is "active": protected branch, locked worktree, dirty (uncommitted changes — `RemoteGone`-only rows use `has_tracked_changes()` instead of `is_dirty()`), unmerged commits (skipped entirely for any row carrying a signal, since the signal already implies the work was handled). `--force` disables all of them at once; `--allow-dirty`, `--allow-unmerged`, and `--include-locked` disable one each.

**Interaction model** picks one of three paths:

- **Interactive** (TTY, and none of `--yes`/`--json`/`--dry-run`): a custom checkbox picker (`picker::multi_select`, sharing the `find` picker's terminal loop) lists every selectable row in the find/list visual language — dim `./` + bold dir name, colored status indicators, dim activity time, plus a dim trailing prune annotation. Checkboxes reuse the project glyph vocabulary: `◉` (green) selected, `◯` (dim) unselected; `Space` toggles the cursor row, `a` toggles all, `Enter` confirms, `Esc` cancels. Locked-out rows — protected/locked, not overridden — are printed above the picker in the same row format with the guard reason (the picker has no per-item disable, so they simply aren't picker rows). Rows are pre-checked exactly where they'd be auto-pruned non-interactively, so Enter-Enter reproduces the old safe default. After selection, one summary confirm ("N worktree(s) and their branches will be deleted") replaces the old list-then-confirm and folds in orphaned-stash and dirty/unmerged-selection warnings.
- **`--dry-run`**: prints the same annotated analysis — pre-checked / selectable / locked-out, each with its signals — and exits. No picker, no deletion.
- **Non-interactive** (`--yes`, `--json`, or no TTY): bare mode prunes exactly the pre-checked set (unchanged from the old `--yes` behavior). Named mode prunes named worktrees when safe, per the rules above.

`--json` extends the existing envelope (`pruned`/`skipped`/`dry_run`) with a `signals` array per entry; `--dry-run --json` populates `pruned` with the would-be-pruned set and leaves `dry_run: true` without deleting anything, same as before.

## Consequences

- One mental model: "is there a signal, and is it active" replaces separate gating for gone/merged/explicit.
- The gone-upstream hint is gone; `--dry-run` is the always-available way to see what's stale without deleting it.
- Breaking UX change: bare `prune <name>` used to warn-and-skip an unmatched name and still prune whatever else matched; it now hard-errors and touches nothing.
- Breaking UX change: `prune <name>` combined with `--gone`/`--merged` used to also sweep in filter-matched worktrees; naming now strictly narrows, never adds.
- Fetch narrows to remotes tracked by named worktrees when names are given, reducing unnecessary network calls.
- The interactive experience moves from "read a static list, type y/n" to "toggle checkboxes, confirm once" — more control, at the cost of one more keystroke for the default case (still just Enter, Enter).

## References

- `docs/diagrams/prune-flow.md` — full flow diagram
- `git-workon/src/cmd/prune.rs` — `Signal`, `PruneRow`, `classify`, `run_interactive`
- `git-workon-lib/src/fetch.rs` — `remotes_tracked_by_worktrees`, `prune_fetch`
- `git-workon-lib/src/config.rs` — `WorkonConfig::prune_gone`, `WorkonConfig::prune_fetch`
- `git-workon-lib/src/error.rs` — `PruneError::NamesNotFound`
