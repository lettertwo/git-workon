# Stacked Diffs Research & Implications for git-workon

## Executive Summary

Stacked diffs is a workflow where large features are broken into a series of small, dependent pull requests that build on each other. This research examines how stacked diff tools work and what design considerations git-workon should account for to avoid conflicts and enable future stacked diff support.

## What Are Stacked Diffs?

**Definition**: A workflow where you create a series of git branches where each branch depends on the previous one in the stack, enabling:
- Breaking large features into small, reviewable PRs
- Working on dependent changes without waiting for reviews
- Merging changes incrementally rather than as one large change

**Example Stack**:
```
main
  └─ feature-step-1 (PR #101)
       └─ feature-step-2 (PR #102)
            └─ feature-step-3 (PR #103)
```

Each PR is small and focused, but they have explicit dependencies.

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

### 1. Branch Parent/Child Relationships

**What it is**: Metadata tracking which branch is based on which
- Graphite: Stores DAG of parent/child relationships
- git-branchless: Infers from commit graph and event log
- Needed for: automatic rebasing, stack visualization, dependency tracking

**Implications for git-workon**:
- Our WorktreeDescriptor may need `parent_branch()` metadata method
- Move command needs to consider stack dependencies
- Doctor command should detect/repair broken parent relationships
- List/interactive modes could show stack structure

### 2. Automatic Rebasing

**What it is**: When a parent branch changes, automatically rebase children
- Complex operation: must rebase in dependency order
- Can fail at any point in the stack
- Requires conflict resolution

**Implications for git-workon**:
- We probably shouldn't implement this initially
- But our design shouldn't preclude it
- Configuration for "auto-rebase on parent change" could be added later

### 3. Commit Evolution Tracking

**What it is**: Track when commits are rewritten (amended, rebased)
- git-branchless: Explicit tracking in event log
- Graphite: Relies on git branch metadata
- Enables advanced undo functionality

**Implications for git-workon**:
- We don't need to implement this
- But we should be aware that tools like git-branchless exist
- Our metadata shouldn't conflict with their event logs

### 4. Stack Navigation

**What it is**: Commands to move between branches in a stack
- `gt up/down` (Graphite) or `git prev/next` (git-branchless)
- Navigate parent/child relationships, not just alphabetical

**Implications for git-workon**:
- Our interactive find could have "show stack" mode
- `git workon find` with stack awareness
- Shell integration could provide stack-aware navigation

### 5. Bulk Operations

**What it is**: Operations across entire stacks
- Submit all PRs at once
- Sync entire stack with upstream
- Delete merged branches in stack order

**Implications for git-workon**:
- Prune command should handle stacks (bottom-up deletion)
- Future: `git workon stack <command>` for stack operations

## Design Recommendations for git-workon

### Phase 1-5: No Breaking Changes Needed

Our current roadmap is compatible with stacked diffs:
- Git config for all metadata ✓ (Graphite uses this too)
- WorktreeDescriptor metadata methods ✓ (can add parent later)
- Move command ✓ (can be stack-aware in future)
- Doctor command ✓ (can detect broken stacks in future)
- Shell integration ✓ (can be stack-aware in future)

### Phase 6: Stacked Diffs Support

When we implement stacked diff support, we should:

**1. Metadata Storage**
- Add `workon.branchParent.<branch-name>` git config entries
- Or use git branch descriptions for parent info
- Don't invent a new metadata format - use git-native storage

**2. Stack Detection**
- Auto-detect stacks from commit graph (like git-branchless)
- Optional: Allow explicit parent specification
- Respect existing Graphite/git-branchless metadata if present

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
- Detect Graphite metadata and respect it
- Detect git-branchless and cooperate with event log
- Don't force users to choose - allow coexistence

### What We Should NOT Do

**1. Implement Automatic Rebasing** (at least initially)
- Extremely complex
- High risk of data loss
- Users can use Graphite/git-branchless for this
- Focus on worktree management, not rebase automation

**2. Invent Custom Metadata Format**
- Use git config like Graphite
- Or infer from commit graph like git-branchless
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

**Options**:
1. Git config (`workon.branchParent.<name>`)
2. Git branch descriptions
3. Infer from commit graph
4. Custom file in `.git/`

**Recommendation**: Start with inference (#3), optionally allow explicit config (#1).
- Inference works with any workflow
- Config allows overrides when needed
- Compatible with existing tools

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

## Implementation Strategy

### Phase 6.1: Stack Detection (Read-Only)

- Detect parent/child relationships from commit graph
- Add `--stack` flag to list command
- Show stack structure in interactive mode
- No writes, just visualization

### Phase 6.2: Explicit Parent Tracking

- Add `--parent` flag to `new` command
- Store in git config: `workon.branchParent.<name>`
- Enhance WorktreeDescriptor with `parent()` method
- Update metadata in `move` command

### Phase 6.3: Stack Operations

- `git workon prune --stack` - delete merged stacks
- `git workon move --stack` - move with children
- Warnings when operations affect stacks
- Integration with existing metadata (Graphite/git-branchless)

### Phase 6.4: Advanced Features (Stretch)

- Stack visualization in smartlog style
- Stack-aware shell navigation
- Integration with `gh` CLI for PR metadata
- Support for Graphite and git-branchless metadata formats

## Compatibility Matrix

| Feature | git-workon | Graphite | git-branchless | spr | Sapling | Compatible? |
|---------|-----------|----------|----------------|-----|---------|-------------|
| Git config metadata | ✓ (planned) | ✓ | Partial | - | - | ✓ Yes |
| Commit graph inference | ✓ (planned) | - | ✓ | ✓ | ✓ | ✓ Yes |
| Event log | - | - | ✓ | - | - | ✓ Yes (don't conflict) |
| Mutation tracking | - | - | ✓ Partial | - | ✓ | ✓ Yes (different systems) |
| Worktree support | ✓ (core) | ⚠️ Issues | ✓ Shared log | ? | ⚠️ Limited | ✓ Yes |
| Auto-rebasing | - | ✓ | ✓ | - | ✓ | ✓ Yes (we don't do it) |
| Stack visualization | Planned | ✓ | ✓ smartlog | - | ✓ smartlog | ✓ Yes |
| Parent metadata | Planned | ✓ Custom | ✓ Inferred | GitHub API | ✓ Inferred | ✓ Yes (git config) |
| Commit-centric | ✓ | Branch-centric | Commit-centric | ✓ | ✓ | ✓ Yes |
| Works with git repos | ✓ | ✓ | ✓ | ✓ | ✓ Git interop | ✓ Yes |

## Conclusion

**Key Findings**:
1. Stacked diffs are about managing dependent branches, not worktrees specifically
2. Multiple mature tools exist with different philosophies:
   - **Graphite**: Branch-centric, DAG metadata, automatic rebasing, GitHub-focused
   - **git-branchless**: Commit-centric, event log, Mercurial-inspired, works with any git workflow
   - **spr**: Lightweight, commit-centric, minimal branching, simple workflow
   - **Sapling**: Alternative SCM, automatic restacking, mutation tracking, Git-compatible
3. Worktrees + stacked diffs have synergy but also challenges
4. Our current roadmap doesn't need changes to support future stacked diff integration

**Recommendations**:
1. **Phase 1-5**: Proceed as planned - no conflicts with stacked diffs
2. **Phase 6**: Add stack detection and visualization first (read-only)
3. **Later**: Add parent tracking and stack-aware operations
4. **Don't**: Implement automatic rebasing or replace existing tools
5. **Do**: Focus on worktree-specific value (multiple stacks checked out simultaneously)

**Critical Design Decisions**:
- ✅ Use git config for metadata (compatible with Graphite)
- ✅ Allow inference from commit graph (compatible with git-branchless, spr, Sapling)
- ✅ Make stack operations explicit, not automatic
- ✅ Respect worktree isolation and shared state boundaries
- ✅ Complement existing tools rather than replace them
- ✅ Support both branch-centric and commit-centric workflows
- ✅ Don't force a particular local branching scheme (like spr)

**No Roadmap Changes Needed**: Our current design is forward-compatible with stacked diff support. We can add Phase 6 features incrementally without breaking earlier work.

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

### Worktrees + Stacked Diffs
- [Multiply your branches in a Git Worktree](https://sylhare.github.io/2025/10/24/Git-worktree.html)
- [Git worktrees with Graphite](https://blog.matte.fyi/posts/git-worktrees-with-graphite/)
- [Why Git Worktrees Beat Switching Branches](https://blog.balakumar.dev/2025/09/25/why-git-worktrees-beat-switching-branches-especially-with-ai-cli-agents/)

### Technical Details
- [Working with stacked branches in Git](https://lobste.rs/s/nc7x89/working_with_stacked_branches_git_is)
- [GitLab Stacked Diffs Documentation](https://docs.gitlab.com/user/project/merge_requests/stacked_diffs/)
