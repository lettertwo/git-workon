# 027 — Path-Encoded Worktree Admin Names

Status: **proposed** — not implemented. The code described under "Decision" does not exist yet.

## Context

A worktree carries two identifiers, derived differently in `add_worktree()`
(`git-workon-lib/src/worktree.rs`):

- **The path** — `workon_root().join(branch_name)`, keeping every slash. `ee/feature-name`
  becomes the directory `<root>/ee/feature-name`, parent directories created as needed.
- **The admin name** — `Path::new(branch_name).file_name()`, the basename alone. This is
  the directory git creates at `.bare/worktrees/<name>`
  (see [ADR-001](001-bare-repo-worktrees-layout.md)).

Git never generates a worktree name containing a slash, and one would not survive if it did:
git discovers worktrees by listing `.bare/worktrees/` exactly one level deep, and each admin
directory's `commondir` file holds the relative path `../..`, which hard-codes that depth.
Verified by nesting an admin directory one level lower — the worktree vanishes from
`git worktree list`, `git worktree prune` offers to delete it, and `git rev-parse` inside it
fails with `fatal: not a git repository`.

Taking the basename means the admin name cannot distinguish two worktrees whose paths end in
the same component. `ee/feature-name` and `archive/feature-name` both reduce to
`feature-name`, so only one of them can exist at a time.

That collision is reachable through a normal archive-and-restart sequence:

```
workon new ee/feature-name
workon move ee/feature-name archive/feature-name    # succeeds
workon new ee/feature-name                          # fails
```

`move_worktree()` recomputes the admin name as the basename of the target
(`git-workon-lib/src/move.rs`). Here old and new both compute to `feature-name`, so the
metadata-directory rename is skipped and the archived worktree keeps the admin directory. The
pointer rewrite that follows is correct and the move is internally consistent — the slot
is simply still occupied. `validate_move()` does not catch it, because it compares the
target against worktree names and branch names, and `archive/feature-name` collides with
neither.

The second `new` then hands `feature-name` to `repo.worktree(...)`. libgit2 joins it onto
`.bare/worktrees/` and calls `mkdir` with `GIT_MKDIR_EXCL` and without `GIT_MKDIR_PATH`, so
intermediate directories are never created. It surfaces as:

```
failed to make directory '.../.bare/worktrees/feature-name': directory exists;
class=Filesystem (30); code=Exists (-4)
```

Git's own `git worktree add` avoids this by appending a counter to the basename —
verified: two adds of different paths ending in `feat` produce admin dirs `feat` and
`feat1`. libgit2 does no such deduplication and performs no validation on the name at all.

The admin name is the lookup key for most of the codebase: `find_worktree()` matches on it,
`WorktreeDescriptor::new()` resolves by it, the orphan and detached paths write into
`.bare/worktrees/<name>/HEAD` directly, `checkout --host-worktree` and prune's scope
matching pass it between commands, and shell completion offers it.

## Decision

Encode the full root-relative path into the admin name, replacing each `/` with `~`.
`ee/feature-name` gets the admin directory `ee~feature-name`; `archive/feature-name` gets
`archive~feature-name`. The two stop colliding.

Five parts:

1. **One encoder.** Called everywhere the name is computed. A single
   `encode_worktree_name(relative_path) -> String` in `git-workon-lib`. The two sites that
   compute rather than read a name — the derivation in `add_worktree()` and the target-name
   calculation in `move_worktree()` — both call it. No third site may compute a name.

2. **A separator that cannot appear in a branch name.** `git check-ref-format` rejects `~`,
   so no branch can ever produce an encoded name by accident, and a flattened name can never
   alias a real top-level branch. A refname-legal separator such as `-` only relocates the
   collision: `ee/feature-name` would encode to `ee-feature-name` and clash with a branch of
   that literal name. Two further constraints narrow the refname-illegal set to `~`. The
   separator must be legal in filenames on every target platform, which rules out `:` on
   Windows. It must also be safe to type unquoted, which rules out `^` — a negation glob
   under zsh's `EXTENDED_GLOB`. `~` expands only at the start of a shell word.

3. **Explicit branch creation for orphan and detached worktrees.** When `opts.reference` is
   `None`, libgit2 calls `git_branch_create` with the *worktree name* (`worktree.c`,
   `git_worktree_add`). Today that is harmless: the worktree name equals the branch basename,
   so the orphan path's cleanup — which deletes `refs/heads/<branch_name>` — removes exactly
   what libgit2 created. An encoded name breaks this in two ways. The cleanup no longer
   matches what was created, and creation itself fails first. Note that the rejection does
   not come from `branch_name_is_valid`, which checks only for a leading `-` and the literal
   `HEAD`; `create_branch` passes the name to `git_reference_create`, whose normalization
   rejects `~` through `is_valid_ref_char` (`refs.c`). Both worktree types must create the
   branch themselves and pass it as `opts.reference`, the way `BranchType::Normal` already
   does.

4. **Lookup by path, never by decoding.** `find_worktree()` gains a third arm matching the
   worktree's root-relative path, computed from `wt.path()`, alongside the existing
   worktree-name and branch-name arms. Nothing reverses the encoding. Git rewrites `gitdir`
   on every move including its own, so the path is always current while a stored name can go
   stale; an arm that matched a decoded name would resolve a worktree to a location it has
   already left. Completion offers the root-relative path.

5. **A doctor check for stale names.** `doctor` reports any worktree whose stored name
   differs from `encode(relative_path)`. That one predicate covers legacy basename names,
   names desynced by a raw `git worktree move`, and repos partway through migration. The
   repair is the metadata rename `move_worktree()` already performs, so the check is fixable
   under `--fix`.

The collision check in `add_worktree()` stays as a backstop. Encoding makes it close to
unreachable, but a legacy basename-named worktree can still occupy a top-level slot, and a
check that names the conflicting worktree is a better failure than libgit2's
`code=Exists (-4)`.

## Consequences

**The name becomes a creation-time artifact, not a derivable key.** `git worktree move`
does not rename the admin directory — verified: moving a worktree from `a/feat` to `c/feat`
left the admin directory as `feat` and rewrote only the `gitdir` file. Git will not maintain
the correspondence between encoded name and path, so a raw `git worktree move` leaves behind
a name that no longer describes where the worktree lives. Nothing reads it as though it did:
lookup goes through the path, so a stale name costs a misleading label in `--json` and prune
output until `doctor` reports it. Computing a name from a path is legitimate only at creation,
which is what the encoder does in `add_worktree()` and `move_worktree()`.

**The metadata rename now runs where it used to be a no-op.** A namespace-only move — `ee/x`
to `archive/x` — previously computed the same basename twice and skipped the admin-directory
rename; under encoding it renames every time. That step has no rollback (see
[ADR-015](015-atomic-move-with-rollback.md)), so a move that fails partway now leaves stale
pointer files in a case that was previously untouched. The damage is confined to `gitdir` and
`.git`, which is what `git worktree repair` fixes, so the failure path reports the error and
points at `workon doctor --fix` rather than attempting a rollback.

**Migration needs no step.** Existing worktrees keep their basename admin directories, and
every lookup reads names back from git rather than recomputing them. New worktrees get
encoded names; a `workon move` on a legacy worktree re-encodes it through the existing
metadata rename. Old and new conventions coexist. The residue is that a legacy
basename-named worktree can still collide with a newly created top-level worktree of the
same name — the rule that already holds today.

**Encoded names surface in a few places users see.** `--json` output, shell completion,
`checkout --host-worktree` values, prune's scope arguments, and `git worktree prune -v`,
which prints the admin directory — `Removing worktrees/ee~feature-name: gitdir file points
to non-existent location`. Everything else in git identifies worktrees by path:
`git worktree list` prints paths, and `remove`, `move`, `lock`, and `repair` all take a
path with last-path-component as a shortcut.

**Raw git commands keep working.** `git worktree repair` works on an encoded admin
directory — verified by renaming one to `d~tildetest` and repairing it, after which
`git worktree list` resolved it correctly. Hand-run `git worktree add` continues to produce
basename-plus-counter names. Those cannot collide with a name encoded from a namespaced path,
which always contains `~`; a top-level worktree encodes to its own basename and stays subject
to the collision described under migration. The encoding buys nothing for raw git either:
with worktrees at `a/feat` and `b/feat`, `git worktree remove feat` already fails with
`fatal: 'feat' is not a working tree`, because git's shortcut matches path components rather
than admin names. Encoding does not disambiguate that; the full path does.

**Scripts that read `git rev-parse --git-dir` see the encoded name.** Anything doing
`basename $(git rev-parse --git-dir)` to recover a worktree name gets `ee~feature-name`
instead of `feature-name`. Shell prompts and per-repo tooling commonly do this.

**Fixing the detached-HEAD stray branch comes along with it.** The detached path writes a
commit SHA into `HEAD` but never removes the branch libgit2 created from the worktree name,
so detached worktrees currently leave a stray local branch behind. Verified:
`workon new --detach scratchwt` leaves a local branch `scratchwt` that nothing references.
Passing an explicit reference, required by explicit branch creation, removes it.

## Alternatives

**Deduplicate with a counter, as git does.** Produces `feature-name`, `feature-name1`.
It resolves the collision without touching the orphan and detached paths, and matches git's
own behavior. Rejected because the resulting name carries no information about which worktree
it belongs to, which leaves prune output and JSON consumers ambiguous about which namespace a
`feature-name1` belongs to.

**Reject the move instead.** Extend `validate_move()` to refuse a target whose basename
collides with an existing worktree's admin name. This turns a latent conflict into an
immediate failure that names the conflict, but it also forbids a legitimate operation:
archiving under a parallel namespace is the case this ADR exists to make work. Worth adding
regardless as a guard for the legacy-name case, but not sufficient on its own.

## References

- `git-workon-lib/src/worktree.rs` — `add_worktree()` name derivation, orphan and detached setup
- `git-workon-lib/src/move.rs` — `move_worktree()` name recomputation, `validate_move()`
- `git-workon/src/cmd/doctor.rs` — per-worktree check framework the stale-name check joins
- [ADR-001](001-bare-repo-worktrees-layout.md) — the `.bare/worktrees/<name>` layout
- [ADR-015](015-atomic-move-with-rollback.md) — the three-step move whose metadata-directory
  rename this changes
- [ADR-012](012-three-branch-types.md) — the orphan and detached paths that must change to
  create their branch explicitly
