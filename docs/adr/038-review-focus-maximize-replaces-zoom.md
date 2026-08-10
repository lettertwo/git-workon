# 038 — Review TUI: Replace the Zoom Enum with Focus + Maximize

Status: accepted (2026-08-09, pre-merge review of the M3–M11 tower)

## Context

The diff pane has three `Role`s, each naming a pair of trees: `Combined` (HEAD ↔ worktree),
`Unstaged` (index ↔ worktree), and `Staged` (HEAD ↔ index). On top of that sits a four-variant
`Zoom` — `Split`, `Combined`, `Unstaged`, `Staged` — cycled with `Z`, settable from
`workon.review.diff.zoom`, and resolved per file by `effective_zoom` against the sub-diffs that
file actually has.

Alongside it, and independent of it, `split_focus` tracks which half of a split has focus, toggled
with `w`.

Three findings from dogfooding through M11 and from reading the state space:

**`Zoom::Combined` earns nothing.** Staging verbs refuse there — every verb writes the index, and
the index is on neither side of HEAD ↔ worktree — so the state is read-only by construction, and
`Zoom::Split` shows the same changes already separated by the axis under review, with verbs that
work. It has never been reached deliberately in daily use.

**`Zoom::Unstaged` and `Zoom::Staged` are not view modes.** The split renders as
`caption(1) + unstaged content + caption(1) + staged content` with the remainder halved evenly, and
there is no resize or collapse. Those two states exist to escape the fixed 50/50 split and give one
role the whole body. That is a maximize, described as an independent state.

**The two mechanisms overlap and can disagree.** `Zoom` and `split_focus` are separate fields, so
"zoomed to Unstaged while focused on the Staged pane" is representable, means nothing, and has to
be kept coherent by every path that touches either. `staging_role` already has to reconcile them.

The obvious alternative — deleting zoom outright and always rendering `Split` — was considered and
rejected. `effective_zoom` would still pick correctly in every case with no user input, but a file
with *both* staged and unstaged hunks would be permanently pinned at half a body each, minus two
caption rows, with no escape. On a short terminal that is single-digit content rows per pane, and
partially-staged files are common when staging hunk-by-hunk while reviewing.

A related cut is *not* available and is recorded here so it is not re-proposed.
`DiffState::from_committed` builds a committed changeset with **both sub-models empty**: nothing in
its index differs from HEAD, nothing in its worktree differs from its index. `Role::Combined` is
therefore the only non-empty role for every committed changeset — every changeset in a stack but
the uncommitted layer, and all of `git workon review <ref> | <a..b> | pr-123`. It is also the
forced role for binary files, which cannot be staged. Combined is the crate's reading view; the two
sub-roles exist because the index is writable. `Role::Combined` stays.

## Decision

**1. Delete the `Zoom` enum.** All four variants, the `cycle_zoom` cycle, and `set_zoom`.

**2. The diff pane's requested state becomes two orthogonal fields.** `split_focus: SplitPane`,
which already exists, and a new `maximized: bool`. Maximize means "give the focused pane the whole
body." The state that meant nothing — focus and zoom naming different panes — becomes
unrepresentable.

**3. `effective_zoom` takes the new inputs and narrows.** Its whole truth table:

| Condition | Result |
|---|---|
| `!can_stage` | `Single(Combined)` |
| both sub-diffs, `maximized` | `Single(focus.role())` |
| both sub-diffs, not maximized | `Split` |
| unstaged only | `Single(Unstaged)` |
| staged only | `Single(Staged)` |
| neither | `Single(Combined)` |

Maximize applies only where the result would otherwise be `Split`. Everywhere else the pane already
fills the body, so the flag is inert rather than special-cased.

**4. Pressing the maximize key when the gate is not returning `Split` is a silent no-op.** Not a
refusal. The user asked for a full-height pane and already has one. The committed-changeset case
keeps an informational notice, reworded — a committed changeset is combined-only, which is worth
saying once rather than leaving the key apparently dead.

**5. `reset_panes` preserves `split_focus` when `maximized` is set.** It currently resets focus to
`SplitPane::Unstaged` on every file open. Under maximize, focus *is* the view, so resetting it
would silently switch which role you are reading when you navigate. Today `Zoom::Staged` persists
across file navigation; preserving focus under maximize is what keeps that behavior.

**6. `maximized` persists across file navigation and refresh**, matching the `Zoom` behavior it
replaces. There are existing tests asserting zoom survives both; they carry over to the new field
rather than being deleted.

**7. Delete `attribute.rs` and its render integration.** With `Zoom::Combined` gone,
`Role::Combined` is unreachable on an uncommitted changeset, and `combined_attribution` already
returns `None` for non-combined roles and for committed changesets. Every surviving combined render
is already `AttributionMode::Plain`. Remove the module, its `pub mod` line,
`combined_attribution`, and `AttributionMode::Attributed` — which also drops that enum's lifetime
parameter, leaving `Plain` and `StagedUniform`.

**8. Remove `workon.review.diff.zoom`.** No replacement key. Maximize is a transient view action,
not a startup preference, and `split_focus` already has no config surface. Removing the read makes
the key unclaimed, so `ReviewConfig::unknown_key_warnings` warns on it for free, with its
Levenshtein "did you mean" pointing at the neighbouring `diff.*` keys. Add no compatibility alias:
the crate is unreleased and `publish = false`, so there is no configuration in the wild, and an
alias would keep a dead concept in the user-facing vocabulary.

**9. Rename the keymap action `cycle-zoom` to `toggle-maximize`,** keeping the `Z` binding. The
keymap already warns on unknown action names, so a user config naming `cycle-zoom` degrades with a
warning rather than silently doing nothing. Preserve the `zM`/`zR` collision constraint that moved
this action off bare `z` in the first place.

**10. Reword `notify_combined_refusal`'s non-committed branch.** It reads
`"{verb} in the unstaged/staged pane — cycle zoom ({key})"`. After this change, binary files are
its only caller, and that advice is wrong for them: `effective_zoom` short-circuits on `!can_stage`
before it looks at anything else, so no key press moves them out of `Role::Combined`. State that
the file is not stageable. This is a pre-existing defect the change exposes rather than creates —
the branch is currently shared with the ordinary combined-zoom case, where the advice is correct,
which masks it.

## Consequences

The diff pane's requested state goes from a four-variant enum plus an independent focus field to
one bool plus that focus field, and one inconsistent combination stops existing. `effective_zoom`
loses an input dimension and gains a smaller table.

The crate loses a pure module, a config key, an enum variant with its lifetime, and the asymmetric
attribution invariant — a combined *deletion* checked against the staged diff's old side, an
*addition* against the unstaged diff's new side. That asymmetry is the most easily-broken thing in
the crate and served only the view being removed.

Reviewing a dirty worktree no longer offers a fused pane showing staged and unstaged changes
together, color-coded by staged-ness. The split shows both as separate navigable panes, and
maximize gives either one the full body.

Committed-changeset review is unaffected. Worth stating plainly, because the change reads like it
should affect it: `git workon review <ref>`, ranges, PRs, and stack navigation render exclusively
through `Role::Combined` and are untouched.

If a fused uncommitted view is wanted later, `attribute.rs` and its tests are recoverable from
history at this ADR's commit, and the asymmetry rationale is preserved in its module header.

## Gotchas

- **Do not follow `Role::Combined` into the model.** The likeliest way to break this is to read
  "remove combined" as reaching `DiffState.files` — which *is* the combined model, and is the
  file-list spine: `role_change(idx, Role::Combined)` returns `diff.files[idx]`, and
  `unstaged_idx`/`staged_idx` are offsets into it.
- **`effective_zoom` keeps every downgrade to `Role::Combined`.** Binary files and the
  no-sub-diff case still land there. Only the `Zoom::Combined` arm goes.
- **Around twenty tests reach a view state by setting zoom** (`set_zoom(Zoom::Combined)`,
  `app.zoom = Zoom::…`, across `app.rs` and `render.rs`). This is not a mechanical rewrite. Each
  needs a decision: a test of combined *rendering* re-points at a committed changeset or a binary
  file; a test of *zoom mechanics* either moves to `maximized` or goes. Tests asserting the state
  survives navigation or refresh must move rather than be deleted — that behavior is still real.
- **`cycle_zoom_walks_the_four_states_and_persists_across_file_nav` splits in two.** The cycle half
  goes; the persistence half becomes a `maximized` test. Add a case the old test could not express:
  maximized on the staged pane, navigate to another file, assert focus and maximize both survive
  (decision 5).
- **The attribution render test guards a real bug.** One test pins that
  `Attribution::build(None, None)` would otherwise miscolor every Add cell as already-staged. Read
  it before deleting and confirm the failure mode cannot reappear through `AttributionMode::Plain`.
- **`config.rs`'s module header documents `zoom = combined`** at line 45.
- **`zoom_key_label` and its plumbing** (`main.rs::plumb_zoom_hint_and_warnings`, the `App` field,
  `set_zoom_key_label`) exist to put the live key in the refusal message. After decision 10 that
  message no longer names a key, so the whole path may be removable — check whether anything else
  consumes it before deleting.

## Verification

- `~/.claude/bin/cargo-gate test` — full workspace green. The gate is the pre-commit bar.
- `~/.claude/bin/cargo-gate clippy` — `-D warnings`, all targets, all features.
- Manual: partially stage a file so it has both staged and unstaged hunks. Confirm the split, then
  `w` to the staged pane, then `Z` — the staged pane fills the body. `Z` again restores the split.
- Manual: while maximized on the staged pane, navigate to another file and back. Confirm both the
  maximize and the staged focus survive.
- Manual: on a file with only unstaged changes, press `Z`. Confirm nothing happens and no error
  appears.
- Manual: `git workon review <some-ref>`. Confirm it renders, and that `Z` reports the
  committed-changeset notice rather than appearing dead.
- Manual: navigate to a binary file on a dirty worktree and press a staging key. Confirm the
  refusal names non-stageability and does not advise pressing anything.
- `git config workon.review.diff.zoom combined` then launch — confirm one unknown-key warning and a
  normal start.
