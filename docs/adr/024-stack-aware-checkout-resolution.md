# 024 — Stack-Aware Checkout vs Worktree Resolution

## Context

`git workon <name>` (no subcommand) currently resolves to either *navigate* (an existing
worktree, via `find`) or *materialize* (auto-attach via `new`) — see ADR-004. The tool is
strictly worktree-per-branch.

Two pressures argue for a third action — *checkout in place*:

1. **Graphite ergonomics.** A worktree is naturally the home for a whole stack; moving
   within it is checkout (`gt up/down/co`), not a worktree per branch.
2. **A hard git constraint.** git refuses to rebase or check out a branch that is already
   checked out in another worktree. Materializing a worktree per stack branch therefore
   breaks `gt restack`/`sync`, which need one working dir to rebase the stack through.

The unit of isolation is better modeled as the **stack**, not the branch. For non-stack
users every branch is a stack-of-one, so the same rules degrade to today's
worktree-per-branch behavior.

## Decision

### Three verbs, by what they touch on disk

- `find` — **navigate** between worktrees (`cd`, no new directory).
- `new` — **materialize** a worktree (creates a directory). Remains the *sole*
  worktree-materializer.
- *(new)* checkout — **move HEAD within an existing worktree** (no directory, no `cd`).

### The resolver for `workon <T>`

Evaluated in order; tie-free by construction:

1. **T has its own worktree** → navigate. (Must be first: git's lock forbids checking out
   T anywhere else while it is live in its own worktree. This subsumes the trunk case —
   `main` lives in the `main` worktree, so `workon main` always jumps there.)
2. **The current worktree's branch shares T's stack** → checkout T in place (the current
   worktree is the stack's home).
3. **The deepest non-trunk ancestor of T has a worktree** → navigate there, then checkout
   T. (Ancestors form a linear chain, so "deepest with a worktree" is unique — no
   branching-stack tie-break is reachable. The trunk worktree is never a checkout host.)
4. **Otherwise** → materialize (auto-attach for an existing branch; new branch / error if
   nothing matches).

### Guard-rail invariant

A branch lives in at most one worktree. The resolver always resolves a name to *that*
worktree (navigate) or to a worktree allowed to host it (checkout/materialize), and never
attempts a checkout git would reject.

### Uncommitted changes during in-place checkout

Two classes of uncommitted change exist, and they want opposite handling:

- **Ambient / traveling diff** (local dev hacks, debug toggles) — should follow you up and
  down the stack.
- **Branch-bound WIP** — belongs to a node; should be left behind and restored on return.

git's native `checkout` already carries non-conflicting local changes onto the new HEAD,
which *is* the traveling-diff behavior — for free, no stash. Therefore:

- **Default: carry along** via native checkout. The diff travels; no stash in the common
  case.
- **On genuine conflict** (the change clashes with the target and cannot travel) →
  **prompt: leave or abort.** "Leave" shelves the change as a labeled stash
  (`workon-autostash: <branch> @ <worktree>`) and completes the move clean.

Because carry-along handles everything non-conflicting, the stash machinery governs only
the narrow conflicting residue — most moves never touch the stash.

#### Stash tracking and restore

`refs/stash` is shared across all worktrees (it lives in the common git dir), so a naive
`stash` … `pop` could pop an unrelated entry. Entries are therefore **labeled** and
restored by matching the `(worktree, branch)` pair, not by stack position — no new
mutable workon state file is introduced.

- **Restore on return:** auto-apply the matching entry with a notice ("restored shelved
  changes for `<branch>`"); on apply conflict (e.g. a restack moved the tree), keep the
  stash intact and warn — never auto-drop.
- **Scope:** strictly the `(worktree, branch)` pair the change was shelved from; the WIP
  belongs to that physical tree, not to the branch wherever it later roams.
- `prune` warns when removing a worktree would orphan a `workon-autostash` entry.

### Escape hatch: force materialize

The smart resolver reuses an existing stack-home where it can; sometimes the user
deliberately wants a *fresh* worktree anyway. Exposed two ways:

- **Interactive:** a dual-action select in `find` — primary keybind = smart resolve,
  secondary keybind = always materialize the highlighted match.
- **Scripted:** a `--new` / `-n` flag that forces materialization.

### Multi-remote name resolution

When a short name matches a branch on more than one remote, resolve with the existing
`upstream → origin → first` precedence already used by `prepare_pr_worktree`, instead of
"first iterated". Local branches still win over any remote. Prompt only when two
equally-preferred remotes both carry the name.

### Relative motion stays with gt

`workon up` / `workon down` are intentionally **not** added. Relative intra-stack motion
and stack metadata remain gt's responsibility; workon owns the worktree layer and named
resolution. (Consistent with the deleted-node rule below.)

### Metadata-only node with a deleted branch

A `◯` stack node whose local branch ref was deleted (metadata remains in gt's store)
**errors with guidance pointing at gt** — workon does not reach into gt's metadata to
resurrect branch refs. workon orchestrates worktrees; gt owns stack metadata.

## Non-stack degradation

Under `--no-stack` or `StackModel::None`, every branch is a stack-of-one: rules 2–3 never
fire, and resolution collapses to today's navigate-or-materialize (worktree-per-branch)
behavior with no behavior change.

## Consequences

- Adds a genuine third action (in-place checkout) to a tool that was strictly
  worktree-per-branch; the worktree-per-branch default is preserved, checkout is the
  stack-relative motion and the deliberate force-materialize override stays available.
- `find` already materializes `◯` nodes (ADR-023); this generalizes routing so that
  navigate / checkout / materialize all flow through one resolver.
- Introduces a labeled-stash convention (`workon-autostash: …`) and makes `prune`
  stash-aware, but no new persistent state file.
- `route_branch_to_command`/`branch_exists` (`git-workon/src/main.rs`) and the
  `docs/diagrams/command-dispatch.md` flowchart will need updating when implemented.

## References

- ADR-004 — smart routing when no subcommand is given (extended here)
- ADR-023 — unified stack-tree views; `find` materializes `◯` nodes
- `git-workon/src/main.rs` — `route_branch_to_command`, `branch_exists`
- `git-workon-lib/src/stack.rs` / `git-workon-lib/src/stack/graphite.rs` — `current_stack`, `enumerate_stacks` (stack/parent data)
- `docs/diagrams/command-dispatch.md` — routing flowchart (update when implemented)
