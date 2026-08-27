# Stacked Diffs Research & Implications for git-workon

## Executive Summary

Stacked diffs is a workflow where large features are broken into a series of small, dependent changesets that build on each other. Tools take different approaches to what a "changeset" is — a branch, a commit, or a logical diff — and different approaches to how dependencies are tracked and submitted for review. This research examines how six stacked diff tools work and what design considerations git-workon should account for to avoid conflicts and enable future stacked diff support. Notably, git-stack bridges the branch-centric and commit-centric models — branches are the unit of work, but stack relationships are inferred purely from the commit graph with zero stored metadata.

## What Are Stacked Diffs?

**Definition**: A workflow where dependent changes are developed in series, with each change building on the previous, enabling:

- Breaking large features into small, reviewable units
- Working on dependent changes without waiting for reviews
- Landing changes incrementally rather than as one large change

The key insight is that the "unit" varies by tool: Graphite uses branches, git-branchless and Sapling use commits in a graph, and spr uses individual commits submitted directly to GitHub as PRs without requiring local branching. git-stack takes a hybrid approach — branches are the unit of work as in Graphite, but stack relationships are inferred purely from the commit graph as in git-branchless, requiring zero stored metadata.

**Branch-centric example** (Graphite's model):

```
main
  └─ feature-step-1 (PR #101)
       └─ feature-step-2 (PR #102)
            └─ feature-step-3 (PR #103)
```

**Commit-centric example** (spr / git-branchless / Sapling model):

```
main
  o─ commit A (sent for review as PR #101)
  o─ commit B (depends on A, sent as PR #102)
  o─ commit C (depends on B, sent as PR #103)
```

In the commit-centric model there may be no local branches at all — the stack lives in the commit graph, with dependencies tracked by the tool or inferred from ancestry.

Sources:

- [Stacked Diffs Guide - Graphite](https://graphite.com/guides/stacked-diffs)
- [Stacked Diffs (and why you should know about them) - Pragmatic Engineer](https://newsletter.pragmaticengineer.com/p/stacked-diffs)

## How Stacked Diff Tools Work

### Graphite CLI

**Core Workflow**:

- `gt create` - Create branches and PRs on top of existing ones
- `gt submit` - Submit entire stack to GitHub with proper target branches
- `gt sync` - Rebase stack onto newest changes, detect merged branches
- `gt checkout` - Navigate between branches in stack

**Metadata Storage**:

- Graphite stores a DAG (directed acyclic graph) showing parent/child relationships
- Metadata tracks: stack order, GitHub PRs, branch dependencies
- Stored locally (not in git objects) - uses git config or separate files
- **Critical**: Must use `gt` commands for renames to maintain metadata

**Key Operations**:

- Automatic recursive rebasing when upstream changes
- Stack visualization with `gt log short`
- Bulk operations across entire stacks

Sources:

- [Graphite CLI Quick Start](https://graphite.com/docs/cli-quick-start)
- [Track Branches - Graphite](https://graphite.com/docs/track-branches)
- [Managing stacked diffs on GitHub with Graphite](https://graphite.com/guides/stacked-diffs-on-github)

### git-branchless

**Architecture**:

- **Event Log**: SQLite database tracking all repository changes via git hooks
- **Commit Evolution**: Tracks when commits are amended/rebased (like Mercurial's changeset evolution)
- **Segmented Changelog**: Efficient commit graph queries (O(log n) merge-base)
- Event log is shared across all worktrees

**Core Features**:

- `git smartlog` - Visual commit graph without requiring branches
- `git undo` - General-purpose undo for commits, merges, rebases
- `git restack` - Repair broken commit graphs after rebases
- `git next/prev` - Navigate commit stacks
- `git move` - Relocate commits in the graph
- `git sync` - Rebase multiple stacks without checking them out
- In-memory operations for performance

**Data Structures**:

- Event log in SQLite (comprehensive, unlike git reflog)
- Commit evolution tracking (old commit → new commit after rebase)
- Loads all events into memory on startup, replays to determine state

**Worktree Support**:

- Event log shared between all worktrees
- Commits made in one worktree visible in others
- `git submit` runs in the worktree where invoked

Sources:

- [git-branchless GitHub Repository](https://github.com/arxanas/git-branchless)
- [git-branchless Architecture](https://github.com/arxanas/git-branchless/wiki/Architecture)
- [Branchless Git - Ben Congdon](https://benjamincongdon.me/blog/2021/12/07/Branchless-Git/)

### spr (spacedentist/spr)

**Philosophy**: One commit per logical change

- Each commit should be coherent, complete, and leave the codebase buildable
- Work directly on local `main` branch (or any branch scheme you prefer)
- Individual commits are sent for review, not entire branches

**Core Workflow**:

- `spr init` - Authorize GitHub API access
- `spr diff` - Submit commit as PR or update existing PR
- `spr land` - Squash-merge approved PR onto latest main

**Key Features**:

- Written in Rust for performance
- Commits remain "amendable and rebaseable"
- Eliminates forced branching per review
- Supports stacked PRs for interdependent code reviews
- Prompts for change description when updating PRs

**Workflow Model**:

1. Make change as single commit on local main
2. Run `spr diff` to create GitHub PR
3. Amend commit in response to feedback
4. Run `spr diff` again to update PR
5. Rebase onto newer upstream as needed
6. Land with `spr land` when approved

**Metadata Storage**:

- Uses GitHub API to link commits to PRs
- Details on local metadata storage not extensively documented
- Designed to work with standard git commits

**Distinctive Approach**:

- No forced local branching scheme
- Commit-centric rather than branch-centric
- Works with existing git workflow without imposing structure
- Particularly lightweight compared to Graphite

Sources:

- [spr GitHub Repository (spacedentist)](https://github.com/spacedentist/spr)
- [spr Documentation](https://spacedentist.github.io/spr/)

### Sapling SCM

**What it is**: A source control system from Meta (Facebook) that emphasizes usability and scalability

- Git-compatible client that can clone from GitHub and push to Git repos
- Uses own architecture with Sapling servers but supports Git repositories
- Derived from Mercurial with commit evolution built-in

**Architecture**:

- **Mutation tracking**: Records commit rewrites (replaces Mercurial's obsstore)
  - Uses IndexedLog for O(log N) lookup vs O(N) for obsstore
  - Requires at least one successor commit (no "prune" operations)
  - Mutation doesn't affect visibility (separate concern)
- **Visibility model**: Treats commits as invisible by default
  - Uses "visible heads" and bookmark references
  - Opposite of Mercurial (which makes all commits visible, then hides obsolete)
- **Smartlog**: ASCII graph visualization of commit relationships
  - Shows unpushed local commits, main branch, current position
  - Elides thousands of commits to show only relevant ones
  - Enhanced "Super Smartlog" (`sl ssl`) fetches GitHub test/review status

**Core Features for Stacked Commits**:

- **Automatic restacking**: Amending a commit auto-rebases dependent commits
- **Navigation**: `sl prev` and `sl next` move between stacked commits
- **Smartlog visualization**: Shows commit graph with relationships
- **Amend-friendly**: Modify earlier commits without manual rebasing
- **Hide/unhide**: Archive commits temporarily without deletion
- **Bookmarks**: Optional local reference points (similar to git branches)

**Git Interoperability**:

- Uses git under the hood for clone/push/pull operations
- Compatible with `.git/` file formats (can run git commands)
- Stores Sapling-specific features (mutation) in `.git/sl/` directory
- **Caveat**: Mixing `sl` and `git` commands may not work in all cases
  - Example: Must use `sl rebase --continue`, not `git rebase --continue`

**Commands** (Sapling equivalents):

- `sl smartlog` - Visual commit graph (like git-branchless)
- `sl restack` - Auto-rebase dependent commits (replaces Mercurial's evolve)
- `sl web` - Interactive GUI with drag-and-drop rebasing
- `sl prev/next` - Navigate commit stack

**Worktree Considerations**:

- Meta may not prioritize worktree features (monorepo too large)
- Git interop mode may have worktree support through git backend
- Focus is on commit evolution and automatic restacking

**Key Insight**: Sapling demonstrates that UX and scale can be separated from repository format

- Modern stack-aware workflows without requiring infrastructure changes
- Can slot into existing Git-centric infrastructure

Sources:

- [Sapling SCM Introduction](https://sapling-scm.com/docs/introduction/)
- [Sapling Internal Differences from Mercurial](https://sapling-scm.com/docs/dev/internals/internal-difference-hg/)
- [Sapling Visibility and Mutation](https://sapling-scm.com/docs/dev/internals/visibility-and-mutation/)
- [Sapling Smartlog Overview](https://sapling-scm.com/docs/overview/smartlog)
- [Sapling Git Interop](https://sapling-scm.com/docs/category/git-interop/)
- [Understanding Sapling's Integration with Git](https://graphite.com/guides/understanding-saplings-integration-with-git)

### git-stack (gitext-rs/git-stack)

**Model**: Branch-centric with implicit stack detection

- Branches are the unit of work (like Graphite), but dependencies are inferred from the commit graph — no metadata stored anywhere
- Stack relationships are determined entirely by ancestry: a branch is a child of the branch whose tip commit is the merge-base of the child's tip and the parent branch's tip
- Zero metadata footprint: no special refs, no config DAG, no SQLite, no `.git/` subdirectory

**Architecture**:

- Written in Rust; uses the `git2` crate (libgit2) — closest tool architecturally to git-workon
- Uses `petgraph` for DAG representation of branch relationships
- `Repo` trait abstracts over production (libgit2) and in-memory (test) implementations, enabling deterministic unit tests without a real git repository
- `Script`/`Batch`/`Executor` pattern: all destructive operations (rebase, branch updates) are staged into a `Script` before any are executed — if a conflict is detected during planning, the entire script is aborted before any mutation occurs

**Core Commands**:

- `git stack` — visualize the current branch stack
- `git stack --push` — push all branches in the stack
- `git stack --pull` — pull changes and restack the entire stack
- `git branch-stash` — snapshot branch state for undo (git-stack's equivalent of `git undo`)
- `git next` / `git prev` — navigate between stacked branches

**Stack Detection**:

- Pure commit graph inference: walks ancestry to find the nearest "protected" branch (e.g., `main`) and identifies all local branches between HEAD and that base
- No explicit parent configuration required or supported — the graph is the authoritative source
- Handles fork points: correctly handles cases where multiple branches share a common ancestor

**Key Design Patterns**:

- **Deferred execution**: `Script`/`Batch`/`Executor` stages all destructive ops before executing any — relevant safety pattern for any tool that performs multi-step git mutations
- **No conflict resolution**: On conflict, git-stack bails out entirely and lets the user handle it manually — avoids implementing complex merge logic
- **Undo via git-branch-stash**: Snapshots branch tip SHAs before any stack operation; restoring is a series of `git branch -f` calls — no custom storage format
- **WIP detection**: Detects dirty working tree and stashes automatically before stack operations
- **Fork support**: Correctly handles stacks that branch off non-main protected branches

**Git Interaction**:

- All git operations go through libgit2 (via `git2` crate) — no subprocess calls to `git` for core operations
- Branch updates are performed as force-updates after rebase planning is complete
- Compatible with standard git workflows; leaves no traces in `.git/` beyond normal branch refs

**Worktree Support**:

- No worktree support: single-worktree focused; does not detect or operate across multiple worktrees
- Stack detection operates on the shared branch namespace, so branches checked out in other worktrees are visible but not specially handled

Sources:

- [git-stack GitHub Repository](https://github.com/gitext-rs/git-stack)

### gh-stack (github/gh-stack)

**Model**: Branch-centric, GitHub CLI extension

- `gh stack` is a `gh` CLI extension (not a standalone binary) for managing linear stacks of
  branches and their pull requests. Its `branches` array within a stack is strictly linear (no
  forking), which is a degenerate case of Graphite's fork-capable DAG.
- Each `branchRef` entry records `branch`, `head`, `base`, and `pullRequest`. `base` is the same
  concept as Graphite's `parentBranchRevision`: the parent's tip at the time the child was
  created or last restacked, used to detect when the parent has moved on.

**Metadata storage**:

- One JSON file (`schemaVersion: 1`) per git dir, at `git rev-parse --git-dir` + `/gh-stack`.
  For the main checkout that is `.git/gh-stack`; for a linked worktree it is
  `<common-dir>/worktrees/<name>/gh-stack`, meaning the file is **per-worktree by default**, not
  shared the way Graphite's SQLite database or ref-blobs are.
- Concurrency is a lock file (`gh-stack.lock`, `flock(LOCK_EX|LOCK_NB)`, 5s timeout with 100ms
  retry) plus an in-process compare-and-swap keyed on a sha256 checksum of the file's contents.
  The checksum never crosses a process boundary.
- Writes go through `os.WriteFile`, which opens with `O_TRUNC` and truncates the target file in
  place. There is no temp-file-and-rename, and no symlink inspection anywhere in the package, so
  a reader can observe a partially written file during a concurrent write, and a symlinked path
  is written through transparently rather than replaced.

**Core commands**:

- `gh stack init`: create a new stack rooted at the current branch
- `gh stack add`: add a new branch on top of the current stack; refuses unless HEAD is already
  at the top of the stack, and unconditionally checks out the new branch itself
- `gh stack view` / `up` / `down` / `top` / `bottom`: inspect and navigate the stack
- `gh stack link`: upstream's own answer for worktree users, pointing a worktree's local file at
  another one, since its own docs (`skills/gh-stack/SKILL.md`) note the per-worktree file "would
  be wrong or absent" without it

**Worktree support**: acknowledged but not automatic. Upstream expects the user to run
`gh stack link` manually per worktree; there is no built-in shared-store mode.

Sources:

- [github/gh-stack repository](https://github.com/github/gh-stack) (MIT)

## Stacked Diffs + Worktrees

### Benefits

Git worktrees are particularly useful for stacked diffs:

- Work on dependent changes simultaneously (e.g., API feature + dependent UI)
- Each worktree can have its own build artifacts (node_modules, .venv, etc.)
- Parallel development without branch switching overhead
- Useful with AI coding assistants working on different branches

### Challenges

**1. Rebasing Complexity**

- Stacked diffs require frequent rebasing
- Each upstream change triggers recursive rebases down the stack
- Example: For 10 commits × 3 stacked branches = 30 rebases instead of 3
- **Implication**: Squashing commits is not recommended in stacked workflows

**2. Shared vs Isolated State**

- **Shared**: .git/objects, refs, remotes, event logs (git-branchless)
- **Isolated**: HEAD, index, working directory, config file
- Rebasing in one worktree affects shared refs
- Cannot checkout same branch in multiple worktrees (branch isolation)

**3. Tool-Specific Issues**

- Some tools may not handle worktrees well
- Example: "work getting erased in other worktrees when using Graphite"
- Tools may assume single working directory

**4. Workflow Patterns**

- Cannot `git checkout main` from a worktree (it's checked out elsewhere)
- Must use `git fetch && git rebase origin/main` instead
- Need to be mindful of which worktree you're in for stack operations

Sources:

- [Multiply your branches in a Git Worktree](https://sylhare.github.io/2025/10/24/Git-worktree.html)
- [Git worktrees with Graphite](https://blog.matte.fyi/posts/git-worktrees-with-graphite/)
- [Why Git Worktrees Beat Switching Branches](https://blog.balakumar.dev/2025/09/25/why-git-worktrees-beat-switching-branches-especially-with-ai-cli-agents/)

## Key Concepts for git-workon

### 1. Dependency Relationships

**What it is**: Tracking which change is based on which — at the branch level (Graphite, git-stack) or commit level (git-branchless, spr, Sapling)

- Graphite: Stores a DAG of branch parent/child relationships in git config
- git-branchless: Infers from commit graph and event log — no branches required
- spr: Uses GitHub API to link commits to PRs; local ancestry implies ordering
- Sapling: Mutation tracking records when commits are rewritten; dependency is implicit in the commit graph
- git-stack: Infers branch dependencies purely from commit graph ancestry — no stored metadata; validates that combining branch-centric units of work with graph-based inference is a coherent hybrid approach
- Needed for: automatic restacking, stack visualization, dependency tracking

**Implications for git-workon**:

- Our WorktreeDescriptor may need `parent()` metadata — could reference a branch or a commit depending on context
- Move command needs to consider stack dependencies
- Doctor command should detect/repair broken parent relationships
- List/interactive modes could show stack structure

### 2. Automatic Rebasing / Restacking

**What it is**: When a parent changes, automatically rebase or restack dependents — applies equally to branch stacks (Graphite, git-stack) and commit stacks (Sapling's auto-restack on amend, git-branchless's `git restack`)

- Complex operation: must process in dependency order
- Can fail at any point in the stack
- Requires conflict resolution — git-stack's approach of bailing out entirely on conflict (rather than attempting resolution) is a notable safety tradeoff
- git-stack's `Script`/`Batch`/`Executor` pattern stages all destructive operations before executing any, providing a useful model for safe multi-step git mutations

**Implications for git-workon**:

- We probably shouldn't implement this initially
- But our design shouldn't preclude it
- Configuration for "auto-rebase on parent change" could be added later

### 3. Commit Evolution Tracking

**What it is**: Track when commits are rewritten (amended, rebased)

- git-branchless: Explicit tracking in event log
- Graphite: Relies on git branch metadata
- git-stack: Uses `git-branch-stash` snapshots (branch tip SHAs) instead of tracking rewrites — simpler but limited to branch-level undo, not commit-level evolution
- Enables advanced undo functionality

**Implications for git-workon**:

- We don't need to implement this
- But we should be aware that tools like git-branchless exist
- Our metadata shouldn't conflict with their event logs

### 4. Stack Navigation

**What it is**: Commands to move between items in a stack — branches (Graphite, git-stack) or commits (git-branchless, Sapling, spr)

- `gt up/down` (Graphite, branch-level), `git next`/`git prev` (git-stack, branch-level), or `git prev/next` / `sl prev/next` (git-branchless/Sapling, commit-level)
- Navigate parent/child relationships, not just alphabetical order

**Implications for git-workon**:

- Our interactive find could have "show stack" mode
- `git workon find` with stack awareness
- Shell integration could provide stack-aware navigation

### 5. Bulk Operations

**What it is**: Operations across entire stacks — applies whether the stack is composed of branches or commits

- Submit all changes at once (as PRs or individual commits depending on tool)
- Sync entire stack with upstream
- Delete or hide merged/landed changes in dependency order
- git-stack's `git run` executes an arbitrary command across all branches in the stack — useful for cross-stack CI or validation

**Implications for git-workon**:

- Prune command should handle stacks (bottom-up deletion)
- Future: `git workon stack <command>` for stack operations

## Design Considerations for git-workon

### Current Implementation Compatibility

The current implementation is forward-compatible with stacked diff support:

- Git config for all metadata ✓ (Graphite uses this too)
- WorktreeDescriptor metadata methods ✓ (can add parent later)
- Move command ✓ (can be stack-aware in future)
- Doctor command ✓ (can detect broken stacks in future)
- Shell integration ✓ (can be stack-aware in future)

### Potential Future Features

If stacked diff support were added, the following areas would need consideration:

**1. Metadata Storage**

Each of the audited tools takes a different approach, each with distinct trade-offs:

- **git config** (`workon.branchParent.<branch-name>`): Portable, git-native, Graphite-compatible. Lightweight but local-only and not tied to commit history.
- **Commit graph inference**: No metadata to maintain; works with any workflow (git-branchless, spr, Sapling, and git-stack all use this). May be ambiguous when branches share commits. git-stack proves that graph inference alone is sufficient for a fully functional branch-centric stack tool.
- **GitHub API linkage** (spr's approach): Commit-to-PR mapping lives in the remote, enabling remote-first workflows without local state. Requires GitHub access.
- **Dedicated store** (Sapling's `.git/sl/`, git-branchless's SQLite): Rich history and mutation tracking. More powerful but introduces non-git dependencies and potential hook conflicts.
- **No metadata at all** (git-stack): Zero footprint — no git config entries, no files, no refs. Stack relationships are read from the commit graph on every invocation. Validates that zero metadata is a viable strategy for branch-centric stacks.

No single approach is universally best; the right choice depends on how much local state git-workon wants to own.

**2. Stack Detection**

- Auto-detect stacks from commit graph (git-branchless, Sapling, spr, git-stack) — git-stack proves that graph inference alone, without any supplementary event log or mutation tracking, is sufficient for a fully functional branch-centric stack tool
- Optional: Allow explicit parent specification via git config (Graphite)
- Use GitHub API to link commits to PRs (spr's approach, useful in remote-first workflows)
- Respect existing tool metadata if present

**3. Stack Operations**

- `git workon new --parent <branch>` - explicit parent
- `git workon list --stack <branch>` - show entire stack
- `git workon prune --stack` - delete merged stacks bottom-up
- `git workon move` - check for dependent branches, offer to move stack

**4. Stack Visualization**

- Enhance `list` command to show tree structure
- Interactive mode with stack filtering
- Show which worktrees are in same stack

**5. Integration with Existing Tools**

All five tools have different integration surface areas:

- **Graphite**: Respect git config DAG metadata; don't overwrite `gt`-managed branch tracking
- **git-branchless**: Cooperate with the shared SQLite event log; avoid hook conflicts
- **spr**: No local metadata to conflict with; GitHub API linkage is transparent to git-workon
- **Sapling**: Avoid modifying `.git/sl/`; don't mix `sl` and `git` command contexts in hooks
- **git-stack**: Zero metadata footprint — nothing to conflict with; transparently coexists with any git workflow

The goal is coexistence and complementarity across all five, not optimization for one.

**6. CLI Delegation as Integration Strategy**

Rather than reimplementing stack detection logic, git-workon could query installed tools' CLIs for stack information — the same pattern already used for `gh` CLI integration.

_Existing precedent in `git-workon-lib/src/pr.rs`_: PR metadata is delegated entirely to `gh`. The pattern is: detect availability via `Command::new("gh").arg("--version")`, shell out with `Command::new("gh")`, parse JSON output, map to internal types (`PrMetadata`), and fail with a diagnostic error (`PrError::GhNotInstalled`) if unavailable. The same structure applies to stacked diff CLIs.

_Per-tool delegation surface_:

- **Graphite**: `gt branch info --json` returns parent/child metadata for the current branch; `gt stack --format json` returns the full stack with ordering and PR state. Native JSON output, straightforward to parse.
- **Sapling**: `sl log -T '{json(node)}\n{json(parents)}\n...'` emits structured commit graph data via Sapling's template system — node hash, parent pointers, bookmarks, phase, and evolution information. Native JSON via templates, low parsing effort.
- **git-branchless**: `git query --raw 'stack()'` returns commit OIDs in topological order, one per line. The `--branches` variant yields branch names instead. The revset language (`stack()`, `children()`, `draft()`) is expressive, but output is plain OIDs — no structured metadata. Medium parsing effort; branch-to-commit mapping requires additional `git` calls.
- **spr**: No machine-readable CLI output. The useful artifact is the `commit-id` trailer spr writes into commit messages; these are parseable via `git log --format='%(trailers:key=commit-id)'`. PR lookup from those IDs requires a `gh` API call — the same `gh` integration git-workon already has.
- **git-stack**: No machine-readable CLI output. Since git-stack itself performs pure commit graph inference with no stored metadata, delegating to it provides no advantage over performing the same inference natively — native graph inference is the equivalent of "delegating" to git-stack's logic.

_Trade-offs vs. native implementation_:

|                    | CLI delegation                     | Native implementation               |
| ------------------ | ---------------------------------- | ----------------------------------- |
| Reimplementation   | None — leverages tool intelligence | Full stack detection logic required |
| Compatibility      | Automatic as tool updates          | Must track upstream changes         |
| Runtime dependency | External binary required           | None                                |
| Schema stability   | Undocumented JSON can change       | Controlled internally               |
| Performance        | Process spawn per query            | In-process                          |
| Error messages     | Tool's errors, not ours            | Full control                        |

_Delegation completeness varies significantly by tool_: Graphite and Sapling can be delegated to almost entirely — their CLIs provide authoritative stack data with rich metadata, and any native detection run alongside would be a lossy approximation of what they already know precisely. If our inference disagrees with Graphite's own DAG answer, there's no good reason to prefer ours. git-branchless and spr sit at the other end of the spectrum: `git query --raw 'stack()'` returns bare OIDs that still require git-workon to resolve branches and map to worktrees; spr's "delegation" is commit-id trailer parsing via `git log`, which is effectively native git work. For these two tools, the line between delegation and native inference blurs. git-stack falls at the same end as git-branchless and spr: since it stores no metadata and performs pure graph inference, delegating to it offers nothing that native graph inference doesn't already provide — the two are equivalent strategies.

One layer is always git-workon's regardless of which tool is installed: mapping stack branches to worktrees. Graphite knows "what are the branches in this stack" but not which of them are checked out as worktrees. That mapping is inherently git-workon's domain.

The practical model is a _priority ordering_, not a layer cake: prefer CLI delegation when the installed tool provides authoritative stack data (Graphite, Sapling); fall back to native commit graph inference when no recognized tool is present, when the tool's output is limited (git-branchless OIDs, spr trailers, git-stack's pure-inference approach), or when no stacked diff tool is installed at all. Native inference is not a baseline that delegation enriches — it is the fallback when delegation is unavailable or incomplete, and for git-stack it is the complete implementation strategy.

_Detection pattern_: Follows the `gh` precedent. The `doctor` command reports which stacked diff tools are detected via PATH scan. Runtime operations check tool availability before use and fail with actionable diagnostic errors if the expected tool is absent — consistent with how `PrError::GhNotInstalled` is surfaced today.

### What We Should NOT Do

**1. Implement Automatic Rebasing** (at least initially)

- Extremely complex
- High risk of data loss
- Users can use Graphite/git-branchless for this
- Focus on worktree management, not rebase automation

**2. Invent Custom Metadata Format**

- Use git config like Graphite
- Or infer from commit graph like git-branchless — git-stack demonstrates that zero stored metadata is sufficient for a fully functional branch-centric stack tool
- Don't create `.workon/` directory with custom files

**3. Replace Existing Tools**

- git-branchless and Graphite are mature
- We should complement, not compete
- Focus on worktree-specific value-add

**4. Break Worktree Isolation**

- Respect that each worktree has independent state
- Stack operations should be explicit, not automatic
- Don't surprise users with cross-worktree changes

## Critical Considerations

### Metadata Location

**Options and trade-offs**:

| Approach                                  | Used by                               | Strengths                                        | Weaknesses                                       |
| ----------------------------------------- | ------------------------------------- | ------------------------------------------------ | ------------------------------------------------ |
| Git config (`workon.branchParent.<name>`) | Graphite                              | Portable, git-native, explicit                   | Local-only, not tied to history                  |
| Infer from commit graph                   | git-branchless, spr, Sapling          | No metadata to maintain, works with any workflow | Can be ambiguous                                 |
| No stored metadata (commit graph only)    | git-stack                             | Zero maintenance, universal compatibility        | Can be ambiguous with complex topologies         |
| GitHub API linkage                        | spr                                   | Remote-first, no local state                     | Requires network, GitHub-specific                |
| Dedicated store (SQLite / IndexedLog)     | git-branchless, Sapling               | Rich history and mutation tracking               | Non-git dependency, hook complexity              |

Commit graph inference has the widest compatibility across tools and requires no extra metadata. Explicit git config provides a useful override mechanism. Neither approach conflicts with the others, making a combination a reasonable starting point if git-workon adds stack tracking.

### Worktree-Specific Concerns

**Branch Checkout Isolation**:

- Can't have same branch in multiple worktrees
- Stack operations must be aware of this
- `git workon new --parent <branch>` should check if parent is checked out elsewhere

**Shared State**:

- Rebasing in one worktree affects all worktrees
- Moving/deleting branches affects all worktrees
- Our operations should warn when they'll affect other worktrees

**Event Log Sharing** (git-branchless):

- Event log is shared across worktrees
- We shouldn't interfere with it
- Our hooks should not conflict with git-branchless hooks

### Stack-Aware Operations

**Move Command**:

```bash
# Current branch is in a stack
git workon move feature-step-2 better-name

# Should warn: "This branch has children: feature-step-3"
# Offer: "Move entire stack? (y/n)"
```

**Prune Command**:

```bash
# feature-step-1 was merged
git workon prune --merged

# Should detect: feature-step-2 and feature-step-3 depend on it
# Should offer: "Also prune orphaned children? (y/n)"
# Or: "Rebase children onto main? (y/n)"
```

**Doctor Command**:

```bash
git workon doctor

# Should detect:
# - Branches with missing parents
# - Stacks with broken dependencies
# - Circular dependencies
# Offer to fix or report issues
```

## Potential Implementation Areas

The following areas represent possible future work grouped by complexity. They are not sequential phases — any could be pursued independently based on user need.

### Stack Detection (Read-Only)

Low risk, high value starting point:

- Detect parent/child relationships from commit graph (as git-branchless, Sapling, and git-stack do) — git-stack validates that petgraph-based DAG analysis of the commit graph is a complete strategy for branch-centric stack detection with no supplementary metadata required
- Add `--stack` flag to list command
- Show stack structure in interactive mode
- No writes, just visualization

### Explicit Parent Tracking

Adds opt-in metadata storage:

- Add `--parent` flag to `new` command
- Store in git config: `workon.branchParent.<name>` (Graphite-compatible approach)
- Enhance WorktreeDescriptor with `parent()` method
- Update metadata in `move` command
- Complement, not replace, inference-based detection

### Stack-Aware Operations

Builds on detection or tracking:

- `git workon prune --stack` — delete merged stacks bottom-up (as any of the tools would expect)
- `git workon move --stack` — move with children
- Warnings when operations affect stacks in other worktrees
- Interoperability: respect existing Graphite git config, git-branchless event log, spr GitHub links, Sapling `.git/sl/` state; git-stack leaves no traces to respect
- Safe multi-step mutations: git-stack's `Script`/`Batch`/`Executor` pattern — stage all operations before executing any — is a model for implementing stack operations that bail out cleanly on conflict rather than leaving the repository in a partial state
- Test infrastructure: git-stack's `Repo` trait pattern (production libgit2 implementation + in-memory test implementation) is directly applicable to testing git-workon's stack logic without requiring real repositories for every test case

### Advanced Features

Higher complexity, lower urgency:

- Stack visualization in smartlog style (Sapling/git-branchless style)
- Stack-aware shell navigation
- Integration with `gh` CLI for PR metadata (aligned with spr's GitHub API model)
- Support for reading Graphite and git-branchless metadata formats explicitly

## Conclusion

**Forward-Looking Considerations**:

- **Stack detection via commit graph inference** is the most broadly compatible starting point — it aligns with git-branchless, spr, Sapling, and git-stack without requiring new metadata; git-stack validates that pure graph inference (via petgraph DAG analysis) is a complete strategy for branch-centric stacks
- **Git config for explicit parent tracking** (Graphite's approach) is a useful complement for cases where inference is ambiguous
- **CLI delegation** (querying `gt`, `sl`, or `git query` for stack data) follows the existing `gh` precedent and avoids reimplementing tool-specific intelligence — completeness varies: Graphite and Sapling can be delegated to almost entirely; git-branchless and spr require native git work regardless; git-stack offers no delegation advantage since it performs the same graph inference that native implementation would; the worktree-to-branch mapping is always git-workon's domain. The model is a priority ordering: prefer delegation when the tool is authoritative, fall back to native inference otherwise
- **spr integration** is naturally handled by parsing `commit-id` trailers from `git log` combined with the `gh` API integration git-workon already has — no separate metadata strategy needed
- **Deferred execution pattern**: git-stack's `Script`/`Batch`/`Executor` approach — stage all destructive operations before executing any, bail out entirely on conflict — is the right safety model for any multi-step git mutation git-workon implements
- **Automatic rebasing** should not be added — each of the mature tools handles this differently and it's extremely complex to get right safely; users should use the dedicated tool of their choice
- **Focus on worktree-specific value**: the unique contribution git-workon can make is managing multiple stacks checked out simultaneously — something none of the audited tools handle well

**Critical Design Decisions**:

- ✅ Use git config for metadata (compatible with Graphite)
- ✅ Allow inference from commit graph (compatible with git-branchless, spr, Sapling, and git-stack — the last of which proves graph inference is sufficient without any supplementary metadata)
- ✅ Prefer CLI delegation when the installed tool provides authoritative stack data (Graphite, Sapling); fall back to native commit graph inference when no tool is present or when delegation is incomplete (git-branchless, spr) or equivalent (git-stack)
- ✅ Stage all destructive operations before executing any; bail out cleanly on conflict rather than leaving partial state (deferred execution, modeled by git-stack's Script/Batch/Executor pattern)
- ✅ Make stack operations explicit, not automatic
- ✅ Respect worktree isolation and shared state boundaries
- ✅ Complement existing tools rather than replace them
- ✅ Support both branch-centric and commit-centric workflows
- ✅ Don't force a particular local branching scheme (like spr)

## References

### Core Concepts

- [Stacked Diffs Guide - Graphite](https://graphite.com/guides/stacked-diffs)
- [Stacked Diffs (and why you should know about them) - Pragmatic Engineer](https://newsletter.pragmaticengineer.com/p/stacked-diffs)
- [How do stacked diffs work - Graphite](https://graphite.com/guides/how-do-stacked-diffs-work)

### Graphite

- [Graphite CLI Quick Start](https://graphite.com/docs/cli-quick-start)
- [Track Branches - Graphite](https://graphite.com/docs/track-branches)
- [Managing stacked diffs on GitHub with Graphite](https://graphite.com/guides/stacked-diffs-on-github)

### git-branchless

- [git-branchless GitHub Repository](https://github.com/arxanas/git-branchless)
- [git-branchless Architecture](https://github.com/arxanas/git-branchless/wiki/Architecture)
- [Branchless Git - Ben Congdon](https://benjamincongdon.me/blog/2021/12/07/Branchless-Git/)

### spr

- [spr GitHub Repository (spacedentist)](https://github.com/spacedentist/spr)
- [spr Documentation](https://spacedentist.github.io/spr/)

### Sapling

- [Sapling SCM Introduction](https://sapling-scm.com/docs/introduction/)
- [Sapling GitHub Repository](https://github.com/facebook/sapling)
- [Sapling Internal Differences from Mercurial](https://sapling-scm.com/docs/dev/internals/internal-difference-hg/)
- [Sapling Visibility and Mutation](https://sapling-scm.com/docs/dev/internals/visibility-and-mutation/)
- [Sapling Smartlog Overview](https://sapling-scm.com/docs/overview/smartlog)
- [Sapling Git Interop](https://sapling-scm.com/docs/category/git-interop/)
- [Understanding Sapling's Integration with Git](https://graphite.com/guides/understanding-saplings-integration-with-git)

### git-stack

- [git-stack GitHub Repository](https://github.com/gitext-rs/git-stack)

### Worktrees + Stacked Diffs

- [Multiply your branches in a Git Worktree](https://sylhare.github.io/2025/10/24/Git-worktree.html)
- [Git worktrees with Graphite](https://blog.matte.fyi/posts/git-worktrees-with-graphite/)
- [Why Git Worktrees Beat Switching Branches](https://blog.balakumar.dev/2025/09/25/why-git-worktrees-beat-switching-branches-especially-with-ai-cli-agents/)

### Technical Details

- [Working with stacked branches in Git](https://lobste.rs/s/nc7x89/working_with_stacked_branches_git_is)
- [GitLab Stacked Diffs Documentation](https://docs.gitlab.com/user/project/merge_requests/stacked_diffs/)
