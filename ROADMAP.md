# git-workon Roadmap

## Vision

Create a git extension for daily workflows with heavy worktree use, with a stretch goal of supporting stacked diff workflows.

## Current Status

### Implemented ✅

- **Core Commands**: `clone`, `init`, `list`, `new` (with name), `find` (with name)
- **Branch Types**: Normal branches, orphan branches (with initial commit), detached HEAD
- **Features**: Bare repo + worktrees pattern, namespace support (slashes in branch names)
- **Testing**: Comprehensive test suite for core functionality

### Not Implemented ❌

- **Commands**: `prune`, `copy-untracked`
- **Interactive Modes**: `find` and `new` without arguments
- **Metadata**: WorktreeDescriptor methods (branch info, remote tracking, status)
- **Fuzzy Finding**: Smart branch name matching
- **Shell Integration**: Fast directory switching like zoxide

---

## Phase 1: Core Workflow Features

**Goal**: Complete essential commands for daily worktree management

### 1.1 Prune Command ✅ COMPLETE

- **Priority**: High
- **Description**: Remove stale worktrees and clean up branches
- **Tasks**:
  - [x] Implement basic prune (remove worktrees for deleted branches)
  - [x] Add `--gone` flag to prune branches deleted on remote
  - [x] Add `--dry-run` flag to preview what would be deleted
  - [x] Add interactive confirmation for destructive operations
  - [x] Add safety checks (dirty/unpushed) with --allow-\* overrides
  - [ ] Add --merged flag for branches merged to main
  - [ ] Write tests for prune scenarios

### 1.2 WorktreeDescriptor Metadata (In Progress)

- **Priority**: Medium-High
- **Description**: Expose worktree metadata for tooling
- **Tasks**:
  - [x] Implement `branch()` - return branch name
  - [x] Implement `is_detached()` - return detached HEAD status (bonus)
  - [x] Implement `is_dirty()` - return true if worktree has uncommitted changes
  - [x] Implement `has_unpushed_commits()` - return true if branch has unpushed commits
  - [x] Add tests for branch metadata methods
  - [ ] Implement `head_commit()` - return current commit hash
  - [ ] Implement `remote()` - return remote tracking info
  - [ ] Implement `remote_branch()` - return remote branch name
  - [ ] Implement `status()` - return worktree status (clean/dirty)
  - [ ] Implement `remote_status()` - return ahead/behind status
  - [ ] Implement remote URL methods

### 1.3 Enhanced Find Command

- **Priority**: Medium
- **Description**: Improve worktree discovery
- **Tasks**:
  - [ ] Add fuzzy matching for branch names
  - [ ] Handle multiple matches (show list, let user pick)
  - [ ] Add `--list` flag to show all worktrees with metadata
  - [ ] Consider prefix/suffix matching strategies
  - [ ] Write tests for fuzzy matching

---

## Phase 2: Interactive & Shell Integration

**Goal**: Make git-workon fast and ergonomic for daily use

### 2.1 Interactive Modes

- **Priority**: High
- **Description**: Add interactive selection when arguments not provided
- **Research**: Evaluate `skim` vs `fzf` integration
  - skim: Rust library, can be embedded
  - fzf: External dependency, more widely used
- **Tasks**:
  - [ ] Research and choose interactive library (skim vs fzf)
  - [ ] Implement interactive `find` (list all worktrees)
  - [ ] Implement interactive `new` (prompt for name)
  - [ ] Add filtering and search in interactive mode
  - [ ] Show metadata in interactive list (branch, status, age)
  - [ ] Write tests for interactive flows

### 2.2 Shell Integration

- **Priority**: Medium
- **Description**: Fast directory switching like zoxide
- **Reference**: Study zoxide implementation
- **Tasks**:
  - [ ] Design shell hook architecture (bash, zsh, fish)
  - [ ] Implement frequency/recency tracking for smart defaults
  - [ ] Create shell completion scripts
  - [ ] Add `cd` integration for automatic worktree switching
  - [ ] Write documentation for shell setup
  - [ ] Consider adding `git-workon jump` command

### 2.3 CopyUntracked Command

- **Priority**: Low-Medium
- **Description**: Copy untracked/ignored files between worktrees
- **Tasks**:
  - [ ] Implement basic file copying
  - [ ] Add macOS `clonefile` optimization (copy-on-write)
  - [ ] Add Linux `cp --reflink` support
  - [ ] Handle large file scenarios
  - [ ] Add progress reporting for large copies
  - [ ] Write tests for copy operations

---

## Phase 3: Workflow Enhancements

**Goal**: Support advanced git workflows

### 3.1 Stacked Diffs Support (Stretch Goal)

- **Priority**: Low (Future)
- **Description**: Compatibility with stacked diff workflows (git-branchless, Graphite, etc.)
- **Research Needed**:
  - [ ] Study git-branchless workflows
  - [ ] Study Graphite CLI patterns
  - [ ] Identify worktree-specific challenges
- **Potential Tasks**:
  - [ ] Support creating worktrees for stack levels
  - [ ] Handle branch dependencies in metadata
  - [ ] Add `--parent` flag to `new` for stacked branches
  - [ ] Visualize branch stacks across worktrees
  - [ ] Integration with `git-branchless` commands

### 3.2 Smart Worktree Management

- **Priority**: Medium
- **Description**: Intelligent worktree lifecycle
- **Tasks**:
  - [ ] Detect and warn about forgotten worktrees (not touched in X days)
  - [ ] Auto-prune merged branches
  - [ ] Suggest worktree reuse based on activity
  - [ ] Track worktree creation reasons/contexts
  - [ ] Add worktree notes/descriptions

---

## Phase 4: Quality of Life

**Goal**: Polish the user experience

### 4.1 Better Output & Reporting

- **Priority**: Medium
- **Tasks**:
  - [ ] Add colored output for status/errors
  - [ ] Improve error messages with suggestions
  - [ ] Add `--json` output for programmatic use
  - [ ] Add `--verbose` flag for debugging
  - [ ] Pretty-print worktree lists with aligned columns
  - [ ] Include details about branch status (ahead/behind, remote)

### 4.2 Configuration

- **Priority**: Low-Medium
- **Tasks**:
  - [ ] Support git config for defaults (workon.defaultBranch, etc.)

### 4.3 Documentation

- **Priority**: Medium
- **Tasks**:
  - [ ] Write comprehensive user guide
  - [ ] Add workflow examples (PR review, feature development, etc.)
  - [ ] Create troubleshooting guide
  - [ ] Add architecture documentation
  - [ ] Record demo videos/screencasts
