# 028 — Provider-Neutral Stack Metadata (Graphite + gh-stack)

## Context

Through `0.12.0`, `git-workon` supported exactly one stacked-diff tool: Graphite. `stack.rs`
dispatched directly to `stack/graphite.rs`, and `Stack`, `StackMetadata`'s precursor, and the
graph-walk functions (`enumerate_stacks`, `current_stack`, the changeset ancestor/descendant
walk) all lived inside that one module.

I added `gh stack` (the `github/gh-stack` CLI extension, MIT-licensed, source read directly to
verify every claim below) as a second provider. `list`/`find` tree rendering, stack-aware
checkout routing, changeset assembly, `doctor` checks, and branch registration on `workon new`
all needed to work under it the same way they do under Graphite.

The obstacle was where the two tools keep state. Graphite writes to `repo.commondir()`, one
shared store for the bare repo and every linked worktree; `graphite.rs` reads it with
libgit2/rusqlite/`serde_json` and never runs `gt`. `gh stack` writes one JSON file at
`git rev-parse --git-dir` + `/gh-stack`, which for a linked worktree is
`<common-dir>/worktrees/<name>/gh-stack`: per-worktree, not shared. Upstream's own
`skills/gh-stack/SKILL.md` says the local file "would be wrong or absent" for worktree
workflows and points users at `gh stack link` instead.

Underneath that difference, both tools reduce to the same data: a trunk set plus
`branch → (parent, parent_revision)`. gh-stack's `branchRef.base` is Graphite's
`parentBranchRevision`, and its strictly linear `branches` array is a degenerate case of
Graphite's fork-capable DAG. The work was one refactor (lift the shared graph algorithms off
Graphite) plus one new parser, not two parallel implementations.

## Decision

### 1. Lift shared metadata and graph algorithms, no `StackProvider` trait

`git-workon-lib/src/stack/metadata.rs` holds a `pub(crate) StackMetadata` struct
(`trunks: Vec<String>`, `parents: HashMap<String, BranchMetadata>`, `pr_titles:
HashMap<String, String>`, `stack_numbers: HashMap<String, u64>`) plus three provider-free graph
functions: `enumerate` (one `Stack` per connected component, ghosts pruned), `current` (the
stack containing a given branch, ghosts retained), and `changeset_walk` (the ancestor/descendant
walk `changeset.rs` uses to order a changeset). Each provider module now shrinks to "parse my
files into a `StackMetadata`": `graphite::read_metadata` and `gh_stack::read_metadata` are the
only entry points, and both `stack.rs`'s dispatch and `changeset.rs`'s assembly call the shared
functions afterward.

I did not add a `StackProvider` trait. A trait earns its keep when call sites are generic over
the implementation; here every call site (`stack.rs`'s `enumerate_stacks`/`current_stack`,
`changeset.rs`'s assembly dispatch, `doctor.rs`'s checks) already matches on `StackModel` and
picks a concrete provider module by name, because the two providers differ in more than their
`read_metadata` signature: gh-stack alone has a write path (`register_branch`), a lock
(`lock_canonical`), and a link/migrate step (`link_worktree`, `migrate_worktree`) that Graphite
has no equivalent of. Forcing those into a shared trait would mean either a trait with
provider-specific optional methods, or pushing gh-stack-only concepts into Graphite's module. A
free function per provider plus the shared `StackMetadata` boundary gives the same code reuse
without either.

### 2. Reads are pure file parsing

Neither provider shells out to read stack state: no `gt log --json`, no `gh stack view --json`.
`graphite.rs` already read `refs/branch-metadata/*` and the SQLite database directly;
`gh_stack.rs` matches that by reading the JSON file directly and never invoking `gh`. The
consequence is the same for both: PR merged state can be stale, `isQueued` is unavailable
(gh-stack's `Queued` field is `json:"-"`, never persisted upstream, so there is nothing to read
even if we shelled out), and `Changeset.title` is `None` under gh-stack, since neither
`branchRef` nor `StackMetadata` carries a PR title the way Graphite's ref-blob does.

### 3. `Stack` gains `number: Option<u64>`

`stack.rs`'s `Stack` struct gained a `pub number: Option<u64>` field, documented as display
metadata and never an identity key. `metadata::enumerate`/`current` populate it from
`StackMetadata::stack_numbers`, which is keyed by every member branch rather than by the stack
root, because `enumerate` prunes ghost branches before its BFS and a ghost root would otherwise
take the number down with it. `group_by_stack` (`stack.rs`) still keys strictly on `(trunk,
sorted diff set)` and never consults `number`: see the first-wins dedupe section below for the
one case where two stacks with the same key would otherwise collide.

`display.rs`'s `TreeNode` carries the matching `stack_number: Option<u64>` field, set only on a
direct child of a trunk (never on the trunk root itself: `build_tree` merges every stack on one
trunk into a single root node, so the trunk has no single number to show). It renders as a dim
`" #N"` suffix (`stack_number_suffix`), degrading to plain text under `NO_COLOR` the same way the
rest of the lane graph does. `list --json` adds an additive `"number"` field to each stack
object (`null` for Graphite/Git), the same additive pattern ADR-023 used for `parents`.

### 4. `workon new` writes the gh-stack file itself

Unlike Graphite, where `workon new` shells out to `gt track` after creating the worktree,
`workon new` under gh-stack writes the canonical file directly via `gh_stack::register_branch`.
No gh-stack command can register a branch that is already checked out: `cmd/add.go` upstream
unconditionally runs `git.CheckoutBranch(branchName)` and refuses unless HEAD is already at the
top of the stack, and git's one-worktree-per-branch rule blocks both the new branch's checkout
(already live in the new worktree) and the base branch's checkout (already live wherever it was
created). There is no `gh stack` subprocess call that could do this job, so `workon` has to
write the file.

### 5. One canonical file, symlinked into every worktree

`<common-dir>/gh-stack` is the one canonical store. `gh-stack` and `gh-stack.lock` are symlinked
into each worktree's admin dir (`link_worktree`); `gh-stack-rebase-state` and
`gh-stack-modify-state` stay per-worktree, since they describe an in-progress operation in one
working tree, and sharing them would make every other worktree believe it is mid-rebase and
block with upstream's exit 7. `workon new` plants the links after `add_worktree` succeeds, gated
on `effective_model == StackModel::GhStack && !self.no_stack`; `doctor --fix` migrates
pre-existing per-worktree files via `migrate_worktree`; the union read in
`gh_stack::read_metadata` survives as a degraded fallback for whatever isn't yet linked. See
"The shared canonical file" below for why the symlink approach works at all.

## Detection: Graphite wins

`StackModel::detect` (`stack.rs`) checks Graphite first, then gh-stack: `.graphite_repo_config`
or `.graphite_metadata.db` existing means `Graphite`; otherwise a `gh-stack` file anywhere
`gh_stack::is_gh_stack_repo` looks means `GhStack`; otherwise `None`. `.graphite_repo_config`
comes from an explicit, repo-wide `gt init`, while a `gh-stack` file can appear as a side effect
of one `gh stack add` run in a single worktree, so the more deliberate, repo-scoped signal wins.
No repository that resolves to `Graphite` today can silently flip to `GhStack` because someone
tried the other tool once in one worktree. The escape hatch is an explicit
`workon.stackModel = gh-stack`; `doctor`'s `BothStackToolsDetected` check surfaces the ambiguity
when both artifacts are present so the user knows to pin the config if `auto` picked the wrong
one.

## The shared canonical file

Two mechanics in the upstream Go source make the symlink approach work, both read directly out
of `github/gh-stack`:

```go
// internal/stack/stack.go:440   -- Save
if err := os.WriteFile(path, data, 0644); err != nil {

// internal/stack/lock.go        -- Lock
f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0644)
```

`os.WriteFile` opens with `O_WRONLY|O_CREATE|O_TRUNC` and truncates in place. There is no rename
anywhere in the package, and no `Lstat` or `EvalSymlinks` call, so gh-stack never inspects
whether the path it opened is a symlink: writes land on whatever the symlink resolves to. The
lock opens the same way, so a symlinked `gh-stack.lock` means every worktree flocks the same
inode, and mutual exclusion becomes genuinely cross-worktree, which it is not under upstream's
own per-worktree layout today.

Two properties fall out of this and matter for correctness:

- A dangling symlink self-heals. `open()` with `O_CREAT` through a symlink to a path that does
  not yet exist creates the target, so planting links before any stack exists is safe. The
  first `gh stack init` run in any linked worktree creates the canonical file, and `Load` on a
  still-dangling link gets `ENOENT` and correctly reports "no stack file." There is no ordering
  constraint on when `link_worktree` runs relative to when the user first initializes gh-stack.
- Symlinks are self-cleaning. `git worktree remove`/`prune` deletes the admin dir along with
  whatever symlinks live in it, so there is no separate cleanup step for `workon prune` to own.

`plant_link` (`gh_stack.rs`) uses a relative target (`../../gh-stack`) so the links survive the
repository being moved on disk.

## The write-in-place risk and the degraded union fallback

Write-in-place is an implementation detail of upstream's `Save`, not a promised contract. If
gh-stack ever switches to temp-and-rename (a correctness improvement they would be right to
make, since it is what `register_branch`'s own write path does), the rename replaces the symlink
in that worktree with a real file, and that worktree's writes stop reaching canonical from that
point on.

`gh_stack::read_metadata` guards against this by reading canonical first, then unioning in
`unlinked_files(repo)` (worktree admin-dir files that are not symlinks resolving to canonical) in
directory-name order. In a healthy, fully-linked repository `unlinked_files` is empty and the
union never runs; it exists purely so that if a worktree ever drifts out of the symlinked state,
nothing written there goes invisible. `doctor`'s `GhStackWorktreeNotLinked` check names every
such worktree and, under `--fix`, calls `link_worktree` (path missing) or `migrate_worktree`
(path holds a real file) to bring it back in line.

## First-wins dedupe, not field-level merge

When the union in `read_metadata` combines more than one source, entries are deduped by
`StackIdentity`: `number` when non-zero, else `id` when non-empty, else `(trunk, first branch)`.
The **entire** stack object from the earliest source is kept; a later source with a colliding
identity is discarded wholesale, never merged field-by-field. Merging two disagreeing ordered
`branches` arrays has no defined semantics, since an insertion in one array is indistinguishable
from a deletion in the other, so a field-level merge could synthesize a stack that existed in
neither worktree's file. `doctor`'s `GhStackDivergentStacks` check names any stack number that
appears in more than one source, which is only reachable in this degraded, unlinked-worktree
path.

## Truncated reads are tolerated, contrary to Graphite's rule

`gh_stack::read_doc` retries a read-and-parse up to 3 times, 25ms apart, and skips the file with
`log::warn!` if every attempt still fails to parse. A partial file is the expected steady state
during a concurrent `gh stack` command, since upstream's `os.WriteFile` truncates and rewrites in
place rather than writing to a temp file and renaming. This read runs on every `list` and every
bare `workon <name>` routing call, so treating a transient truncated read as fatal would make
those commands flaky under nothing more than ordinary concurrent gh-stack use.

This is the deliberate opposite of Graphite's rule. `graphite.rs`'s `read_branch_metadata`
(`graphite.rs:124-137`) treats a present-but-unreadable `.graphite_metadata.db` as a hard error
(`StackError::GtParseFailed`) and never falls back to the older refs-based format. The difference
is what "unreadable" implies for each store: SQLite writes are atomic, so an unreadable database
means corruption, not a mid-write snapshot, and silently ignoring it there would hide a real
problem instead of a transient one.

`schemaVersion > 1` is a hard error in both `read_doc` and `plan_registered_doc`
(`StackError::GhStackSchemaUnsupported`), and is never retried: retrying a version mismatch
cannot fix it, and skipping it would silently render a confidently wrong, outdated stack. Missing
or `0` is treated as `1`, matching Go's zero-value behavior for an unset int field.

## register_branch's read-under-lock, and the per-worktree-write asymmetry it closes

`register_branch` (`gh_stack.rs`) takes `<common-dir>/gh-stack.lock` via `flock(LOCK_EX |
LOCK_NB)` before writing, retried every 100ms up to 5s. Because every worktree's lock path
symlinks to the same file (decision 5), this genuinely excludes a concurrent `gh stack` process
running in any worktree, not just the one `workon new` is writing from.

I first paired the lock with a compare-and-swap on raw file bytes: read the canonical file once
before taking the lock, compare that snapshot against a fresh read taken right after the lock
was acquired, re-plan once against the fresher bytes on a mismatch, and return
`StackError::GhStackFileChanged` if it still disagreed. I removed it in a later commit
(`8c84c0c`). It existed to guard against a writer that bypasses `lock_canonical` entirely, and
it didn't actually guard against that writer: such a writer can still write between the compare
and the rename, so the CAS bought nothing over a plain read-under-lock. What it did buy was a
new failure mode: an ordinary lock-respecting `gh stack` process that wrote between the pre-lock
read and the lock acquisition would trip the mismatch and fail an otherwise-correct
registration.

`register_branch` now reads the canonical file only after the lock is held. No lock-respecting
writer, every `gh stack` invocation and every other `git-workon` call into this module, can be
mid-write once the lock is ours, so that read always sees a complete file. There is nothing left
to compare against.

Decision 5 removes an asymmetry that existed under upstream's own per-worktree layout: `view`,
`up`, `down`, `top`, `bottom` worked only in the worktree holding the file that happened to have
been written to, and a `workon new` (or a plain `gh stack add`) run from a different worktree in
the same stack would not be visible there. With one canonical file, every worktree in the stack
sees the same state regardless of which worktree wrote it.

## Consequences

- `Stack` gains a public field (`number: Option<u64>`), a breaking change for the published
  `workon` crate at `0.12.0`. The introducing commit carries the `!` marker so release-plz
  bumps the minor version correctly.
- `list --json` gains an additive `"number"` field per stack object; `diffs`, `checkouts`, and
  `parents` are unchanged.
- New `StackError` variants: `GhStackSchemaUnsupported`, `GhStackParseFailed`, `GhStackLocked`,
  `GhStackWriteFailed`, `GhStackLinkFailed`, `GhStackNoStackForBase`, and `DeletedStackNode`
  (the provider-neutral twin of `DeletedBranchNode`). `Resolution::DeletedNode`
  carries no model, so `route_branch_to_command` in `git-workon/src/main.rs` now selects between
  the two based on the effective `StackModel` (see ADR-024).
- New `doctor` checks: `GhStackWorktreeNotLinked` (warn, the `--fix` target),
  `GhStackExtensionNotFound` (warn, only when the effective model is `GhStack`, unlike the
  unconditional Graphite equivalent), `GhStackNotInitialized`, `GhStackFileUnreadable` (fail),
  `GhStackDivergentStacks` (warn), and `BothStackToolsDetected` (warn).
- `Config::stack_auto_track` replaces the Graphite-specific `gt_auto_track` as the precedence
  chain CLI override > `workon.stackAutoTrack` > `workon.gtAutoTrack` (deprecated) > `true`.
  `gt_auto_track` stays as a thin wrapper for one release; `gtAutoTrack` is deliberately **not**
  added to `doctor.rs`'s `RENAMED_SCALAR_KEYS`, since that list means "no longer read" and
  `stack_auto_track` still reads it as a fallback.
- `gh_stack.rs` parses into raw `serde_json::Value` rather than derived structs on both the read
  and write paths, matching `graphite.rs`'s existing convention: the workspace has `serde_json`
  as a dependency but no `serde` derive crate, and the write path in particular needs the raw
  `Value` round-trip to preserve `id` and `pullRequest` on stack entries workon itself never
  touches.

## References

- ADR-023 — unified stack-tree views; the `Stack.parents` field this ADR's `Stack.number`
  field sits next to.
- ADR-024 — stack-aware checkout resolution; `DeletedNode`'s remedy text is now
  provider-selected (`DeletedBranchNode` vs `DeletedStackNode`).
- `git-workon-lib/src/stack.rs`, `git-workon-lib/src/stack/metadata.rs`,
  `git-workon-lib/src/stack/graphite.rs`, `git-workon-lib/src/stack/gh_stack.rs`
- `git-workon-lib/src/changeset.rs`, `git-workon-lib/src/config.rs`,
  `git-workon-lib/src/error.rs`
- `git-workon/src/cmd/new.rs`, `git-workon/src/cmd/doctor.rs`, `git-workon/src/display.rs`
- `github/gh-stack` (MIT): `internal/stack/schema.json`, `internal/stack/stack.go`,
  `internal/stack/lock.go`, `cmd/add.go`, `skills/gh-stack/SKILL.md`
