# 037 — Review TUI: Progressive Pipeline (Threads, Streaming Acquisition, Generation-Tagged Loads)

Status: accepted (2026-07-10, progressive-pipeline design session)

## Context

The M7 performance pass (perf-gt-detect … perf-pty-responsiveness) removed the worst
launch and navigation stalls, but two synchronous gaps remain: the idle-deferred file load
runs on the event-loop thread (a huge file holds input hostage for its own load once the
80ms debounce fires), and startup diffs complete in full — behind the splash, but not
streamed — before the outline appears. Closing them means work moves off the event-loop
thread, which **supersedes M4's locked decision #4** ("a synchronous poll on the existing
`Tick`… No threads, no `mpsc`, no new deps" — recorded in `tui.rs`'s module doc, not an
ADR). This ADR retires the "no threads" letter of that decision while keeping its "no new
deps" spirit: everything below is `std::sync::mpsc` + `std::thread`. Zero new dependencies.

Scope: streamed startup acquisition, off-thread file loads, a dedicated input thread, and
the refresh path riding the same pipeline (one acquisition path, not two). Explicitly out
of scope: overlapping the `theme=auto` terminal probe with acquisition (deprioritized; a
later changeset can reuse this seam). Nothing here reorders startup ahead of the theme
probe / `flush_pending_tty_input` sequence — that ordering is load-bearing (see the
pty_smoke silent-terminal canary).

## Decision

**Topology — three permanent threads plus transient wave workers.** The *main thread*
owns `App`, rendering, and every repo **write** (staging stays synchronous). The *input
thread* is the sole reader of terminal events: a blocking `crossterm::event::read()` loop
forwarding into the inbox. The *loader thread* owns its own long-lived `Repository` +
`TsHighlighter`; it serves file-load requests sequentially and, for a whole-stack diff
wave (startup, refresh), spawns a transient scoped worker pool — per-worker `Repository`,
exactly today's `diff_changesets` striping — but **streams each changeset's result as it
completes** instead of joining the batch. The parallel-diff win and streaming compose.

**Protocol — one inbox, stateless loader.** A single `mpsc` inbox feeds the main loop;
`recv_timeout` replaces `event::poll`, and the timeout *is* the Tick beat (index-watcher
poll and the 80ms open-debounce survive unchanged as timeout arms). `AppEvent` grows
loader-result variants — `ChangesetReady { gen, idx, result }` and
`FileReady { gen, cs_idx, file_idx, views }` — and loses `derive(Copy)`. `drain_pending`
becomes a `try_recv` loop, so nav coalescing in `update_batch` carries over untouched. The
loader is stateless between jobs: each request carries what it needs (cloned `FileChange` +
span; content is read through the loader's own repo handle). `App` stays the single owner
of diff truth — no second copy of the stack to keep coherent across refreshes.

**Generations — one global `u64`, mismatch is the only drop rule.** The invariant:
*generation bumps ⟺ the view caches were invalidated* (launch is gen 1; every refresh
bumps). Requests are stamped at send; results carry the stamp; the main loop discards
mismatches at one chokepoint. Within a generation every `FileReady` is cached **even if
the user navigated away** — the diff hasn't changed, so an early result is warmth, not
staleness (A→B→A bounces land on a warm A). The loader never decides staleness.
Per-changeset generations were rejected: refresh rebuilds view caches wholesale, so finer
tags would model granularity the app doesn't have.

**Slots — `Pending | Ready | Failed` per changeset.** `App` is constructible from
resolved-but-undiffed changesets: all slots `Pending`, `current_cs` from lib-`current`
(metadata only), outline headers render immediately and file rows fill in per
`ChangesetReady`. Navigating onto a `Pending` changeset shows the existing placeholder
treatment. Waves diff the **current changeset first**, then input order — the changeset
the user lands on becomes interactive earliest, and the splash becomes redundant for
stacks (the first real frame is the live outline).

**The lone-changeset launch stays synchronous.** `main.rs` forks on `changesets.len()`:
one changeset (non-Graphite default, ref/range, PR) keeps today's sync diff + empty-check
+ splash byte-identical. Streaming's grain is per-changeset, so a 1-changeset review gains
nothing from it — and the "nothing to review" exit-0 must stay tty-free (the
`clean_worktree_prints_nothing_to_review_and_exits_success` canary runs with no terminal;
an in-TUI empty-detection can never serve it). Consequently `App::from_changesets`'s
≥1 assert survives unchanged.

**Force-completion — synchronous fallback on the main thread.** The `apply_action`
chokepoint keeps its meaning: an action that reads the view (`s`, cursor moves, selection)
finds the cache warm or loads *synchronously right there* — `App` keeps its own
`Repository` + `TsHighlighter` for exactly this and for staging. The in-flight loader
result later hits "already cached" and is discarded. The loader is thereby a **pure
cache-warmer: correctness never depends on it**, and the CS4 invariant (deferred-then-
completed open ≡ eager open, byte-identical) survives trivially. Accepted cost: `s` on a
just-reached huge file can still block for that file's load — the price of byte-identical
action semantics without action-replay machinery (queueing actions until `FileReady` was
rejected: replay ordering hazards for a rare case). Highlight determinism across the two
highlighter instances holds — highlighting is a pure function of content + grammar
(ADR-035's theme-free design).

**Refresh — sync resolve, span-keyed reuse, uncommitted always sync.** Resolve stays on
the main thread (offline, cheap; PR sources remain refresh-no-ops). The rebuilt view list
carries over any `Ready` slot whose `(name, span)` is unchanged — a committed diff is a
pure function of its span — so an ordinary post-staging refresh re-diffs *nothing but the
uncommitted layer*, and a restack streams only what moved ("never blank" holds by
construction: stale-but-present content renders until replaced). The **uncommitted layer
always re-diffs synchronously** in every refresh: it is ms-scale, and this preserves
staging's guarantee that the next keystroke sees the post-op world — an async refresh
would let a second `s` compute its patch against a stale diff. One refresh shape; no
staging-vs-manual modes. Every refresh bumps the generation (reused-slot in-flight loads
die valid at the inbox; accepted waste for one global rule).

**Failures — per-changeset degradation for stacks, fatal only where it's the whole
review.** `ChangesetReady { result: Err }` sets that slot `Failed`: the outline marks it,
navigating to it renders the error, the wave's first failure raises a footer notice, and
the review continues (34 reviewable changesets beat zero). The lone-changeset sync path
keeps today's pre-loop fatal miette exit. `r` is the retry — span reuse only carries
`Ready` slots. Contract change accepted: a stack review with one corrupt changeset now
exits 0 on quit where it previously died non-zero; the tty-less paths are unchanged.

**Lifecycle — kill-on-exit, one justified `catch_unwind`.** No join on quit: the loader
never writes, so killing it mid-read corrupts nothing, and a join only adds quit lag. The
loader wraps each job in `catch_unwind`, converting a job panic into a `Failed` result —
the specific class this catches that nothing else does: a panicked job silently drops into
slots stranded `Pending` forever (the inbox stays connected via the input thread's
sender), an invisible hang instead of a visible error. Input-thread read errors are
forwarded into the inbox and exit the loop as `io::Error`, same observable behavior as
today.

**Rejected: an async runtime (Tokio).** It relocates this complexity rather than removing
it: every job is blocking (libgit2 C calls, tree-sitter CPU, tty reads), so all work lands
in `spawn_blocking` — the same threads plus a runtime. `select!` buys nothing over the
single-inbox `recv_timeout`; future-cancellation cannot interrupt blocking work and
doesn't replace generation tags (the race is results-already-computed, not work-in-
flight); and the force-completion *synchronous* fallback — load-bearing for staging
correctness — is trivial in sync code and a genuine problem inside a task. The hard parts
of this design are state-model decisions that survive any executor.

**Testing — real threads confined to one smoke layer.** (1) The loader job body is a pure
function `LoadRequest → AppEvent`, unit-tested synchronously (diff correctness, error
wrapping, panic-to-`Failed`). (2) Loop behavior is tested by feeding synthetic event
sequences through `update_batch` — slot transitions, gen drops, within-gen cache-after-
nav-away, span reuse, and the carried eager-equivalence invariant — no threads, no flake
surface. (3) One real-thread integration smoke plus a `pty_responsiveness` extension
asserting the first interactive frame lands before a full wave could have finished
(`#[ignore]`, run solo, per the existing wall-clock caveat). Existing eager-mode tests
stay untouched (defer off, slots constructed `Ready`).

## Consequences

- `tui.rs`'s module doc note pinning M4 locked decision #4 must be rewritten to point
  here; the M4 index-watcher *semantics* (signature compare on the tick beat, echo
  suppression) are unchanged — only the beat's mechanism moves from `event::poll` timeout
  to `recv_timeout`.
- `AppEvent` stops being `Copy`; `drain_pending`/`next_event` reshape around the inbox;
  the input thread becomes the only code that touches crossterm's event API.
- The splash survives only on the lone-changeset path; for stacks the first frame is the
  live outline with `Pending` rows.
- Two visible behavior changes, both accepted: post-restack refreshes show placeholders
  for moved changesets while unmoved ones stay readable (today the whole UI freezes), and
  a corrupt changeset in a stack degrades to a `Failed` row instead of killing the launch.
- `App` and the loader each hold a `TsHighlighter`; grammar caches are duplicated
  per-instance (modest, accepted for the sync-fallback guarantee).
- The loader thread's `Repository` is long-lived, and libgit2 caches a repository's index
  in memory without ever re-reading it from disk. So index state a loader job reads goes
  stale the moment the main thread stages: the handle keeps serving the index as it stood
  when some earlier job on it first looked. Found in practice after this ADR landed, as a
  staged file rendering its gutter with no text (`read_index_blob` returned the pre-stage
  blob, so the staged view's new side came back shorter than its own hunks). It reproduced
  only for a stage started from the outline, since the diff pane's own staging verbs run
  the force-completion fallback above and rebuild on `App`'s handle, which just did the
  write. `read_index_blob` now calls `git_index_read(force = false)` before every read.
  Any future loader-side read of index state needs the same treatment: the force-completion
  fallback covers correctness only for what the main thread actually re-reads.
