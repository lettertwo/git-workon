# git-workon Roadmap

## Vision

Create a git extension for daily workflows with heavy worktree use, with a stretch goal of supporting stacked diff workflows.

## Current Status

### Implemented ✅

- **Core Commands**: `clone`, `init`, `list`, `new` (with name and `--base` flag), `find` (with name), `prune` (bulk and targeted), `copy-untracked`
- **Prune Features**: Named worktree pruning (`prune <name>...`), bulk pruning with `--gone`/`--merged`, `--dry-run`, safety checks with `--allow-dirty`/`--allow-unpushed`, protected branches
- **Branch Types**: Normal branches, orphan branches (with initial commit), detached HEAD
- **Features**: Bare repo + worktrees pattern, namespace support (slashes in branch names)
- **Metadata**: WorktreeDescriptor methods for branch info, dirty/unpushed/merged detection
- **Configuration System**: Git config support with 7 config keys, CLI override precedence, validation
- **Post-Creation Hooks**: Automatic execution of `workon.postCreateHook` commands with environment variables, `--no-hooks` flag
- **File Copying**: Standalone `copy-untracked` command (defaults to copying all untracked files), automatic copying in `new` command with `workon.autoCopyUntracked` config, pattern-based with `workon.copyPattern` and `workon.copyExclude`, platform-optimized (copy-on-write), `--(no-)copy-untracked`/`--pattern`/`--force` flags
- **Pull Request Support**: Create worktrees from PR references (`#123`, `pr#123`, GitHub URLs), smart routing, auto-fetch, configurable naming (`workon.prFormat`)
- **Testing**: Comprehensive test suite with FixtureBuilder pattern and custom predicates (83 tests total)

### Not Implemented ❌

- **Commands**: `move`, `doctor`
- **Interactive Modes**: `find` and `new` without arguments, fuzzy matching
- **Shell Integration**: Fast directory switching like zoxide

---

## Phase 1: Configuration Foundation ✅

**Status**: Completed
**Goal**: Establish git config as foundation for automation and defaults

### 1.1 Configuration System ✅

- **Priority**: High
- **Philosophy**: Use git config exclusively (no custom config files)
- **Description**: Support configuration via standard git config mechanisms
- **Tasks**:
  - [x] Define configuration schema and naming conventions
  - [x] Implement config reading from global (~/.gitconfig) and local (.git/config)
  - [x] Add `workon.defaultBranch` - default base branch for new worktrees
  - [x] Add `workon.postCreateHook` - commands to run after worktree creation (multi-value, convenience alternative to git's post-checkout hook)
  - [x] Add `workon.copyPattern` - glob patterns for automatic file copying (multi-value)
  - [x] Add `workon.copyExclude` - patterns to exclude from copying (multi-value)
  - [x] Add `workon.autoCopyUntracked` - enable automatic file copying when creating new worktrees (boolean)
  - [x] Add `workon.pruneProtectedBranches` - branches to protect from prune (multi-value)
  - [x] Add `workon.prFormat` - format string for PR-based worktree names
  - [x] Implement config precedence: CLI args > local config > global config > defaults
  - [x] Add validation with helpful error messages for invalid config
  - [x] Write tests for config parsing, precedence, and validation

**Implementation Notes**:
- Core config module with `WorkonConfig` struct and lifetime management
- 7 config methods with CLI override support
- Multi-value config support using `config.multivar()`
- Simple glob pattern matching for protected branches (exact match, `*`, and `prefix/*`)
- Integrated `workon.defaultBranch` into `new` command with `--base` flag
- Integrated `workon.pruneProtectedBranches` into `prune` command
- Extended test infrastructure with FixtureBuilder `.config()` method and `has_config_multivar` predicate
- 12 unit tests for config module + integration tests for prune and new commands

**Configuration Examples**:

```
# Global config (~/.gitconfig) - personal preferences
[workon]
  defaultBranch = main

# Per-repo config (.git/config) - project-specific
[workon]
  postCreateHook = npm install
  postCreateHook = cp ../.env .env
  copyPattern = .env.local
  copyPattern = .vscode/
  copyExclude = .env.production
  autoCopyUntracked = true
  pruneProtectedBranches = main
  pruneProtectedBranches = develop
  pruneProtectedBranches = release/*
  prFormat = pr-{number}
```

---

## Phase 2: Automation & Smart Defaults ✅

**Status**: Completed
**Goal**: Eliminate repetitive tasks through automation

### 2.1 Post-Creation Hooks ✅

- **Priority**: High
- **Description**: Execute commands automatically after worktree creation
- **Depends On**: Configuration System (1.1)
- **Approach**: Hybrid - support both git's native `post-checkout` hook and `workon.postCreateHook` config
- **Use Cases**:
  - Run `npm install` after creating Node.js project worktrees
  - Run `cargo build` after creating Rust project worktrees
  - Copy configuration files, run setup scripts

**Git Native Approach (Recommended for Power Users)**:

- Git's `post-checkout` hook fires on `git worktree add` (since Git 2.16)
- Hook receives: previous HEAD (all zeros for new worktree), new HEAD, flag (always 1)
- Detection: Check if first parameter equals `0000000000000000000000000000000000000000`
- Limitation: Git only supports one script per hook (must manually multiplex or use hook managers)
- Example: See `.git/hooks/post-checkout.sample` in documentation

**Config Approach (Recommended for Simple Cases)**:

- Simpler for users who just want "run npm install after creating worktree"
- No shell scripting required, just `git config --add workon.postCreateHook "npm install"`
- Doesn't conflict with existing post-checkout hooks
- Multi-value config allows multiple commands to run sequentially

- **Tasks**:
  - [ ] Document git's native `post-checkout` hook approach as primary option (deferred to Phase 5.4)
  - [ ] Provide example post-checkout script that detects worktree creation (documented in ROADMAP)
  - [x] Implement `workon.postCreateHook` config as convenience alternative
  - [x] Read `workon.postCreateHook` config (multi-value, executed sequentially)
  - [x] Execute config hooks in worktree directory after successful creation
  - [x] Provide environment variables to config hooks:
    - `WORKON_WORKTREE_PATH` - absolute path to new worktree
    - `WORKON_BRANCH_NAME` - branch name
    - `WORKON_BASE_BRANCH` - base branch (if applicable)
  - [x] Add `--no-hooks` flag to skip both git hooks and config hooks
  - [x] Respect git's native hook execution (don't interfere with post-checkout)
  - [x] Handle hook failures gracefully (show error, don't delete worktree)
  - [x] Show hook output by default, suppress with `--quiet`
  - [ ] Add timeout protection for long-running config hooks (configurable) (deferred to Phase 5)
  - [ ] Document when to use post-checkout vs workon.postCreateHook (deferred to Phase 5.4)
  - [ ] Document hook execution order (git's post-checkout runs first, then config hooks) (deferred to Phase 5.4)
  - [ ] Document security implications of hook execution (deferred to Phase 5.4)
  - [x] Write tests for config hook execution, failures, and edge cases

**Implementation Notes**:
- Created `git-workon/src/hooks.rs` module with `execute_post_create_hooks` function
- Platform-specific shell execution (sh on Unix, cmd on Windows)
- Hooks execute sequentially with progress output
- Hook failures show warnings but don't fail worktree creation
- Integrated into `new`, `clone`, and `init` commands
- Added `--no-hooks` flag to all three commands
- 8 comprehensive integration tests covering success, failure, environment variables, multiple hooks, and --no-hooks flag

### 2.2 Enhanced File Copying ✅

- **Priority**: Medium
- **Description**: Pattern-based automatic file copying between worktrees
- **Depends On**: Configuration System (1.1)
- **Upgrade From**: Basic copy-untracked command
- **Tasks**:
  - [x] Implement basic file copying between worktrees
  - [x] Support glob patterns (e.g., `.env*`, `node_modules/`, `.vscode/`)
  - [x] Respect `workon.copyExclude` patterns
  - [x] Add `--pattern` flag for one-off pattern overrides
  - [x] Add macOS `clonefile` optimization (copy-on-write)
  - [x] Add Linux `cp --reflink` support
  - [x] Handle large file scenarios gracefully
  - [ ] Add progress reporting for large copies (deferred to Phase 5)
  - [x] Skip files already present in destination
  - [x] Add `workon.autoCopyUntracked` config for automatic copying in `new` command
  - [x] Add `--(no-)copy-untracked` flags to `new` command
  - [x] Integrate automatic copying into `new` command workflow
  - [x] Write tests for pattern matching, copy operations, and automatic copying

**Implementation Notes**:
- Created `git-workon/src/copy.rs` module with platform-optimized copying
- Uses glob crate for pattern matching
- Platform-specific optimizations:
  - macOS: `cp -c` (clonefile/copy-on-write)
  - Linux: `cp --reflink=auto` (copy-on-write when supported)
  - Other: Standard `fs::copy`
- Implemented full `copy-untracked` command with worktree path resolution
- **Standalone command** (`copy-untracked`):
  - Default behavior: copies all untracked files (`**/*` pattern)
  - `--pattern` flag: override with specific pattern
  - Config: uses `workon.copyPattern` if set (convenience)
  - Priority: `--pattern` > config > default `**/*`
  - Added `--force` flag to overwrite existing files
- **Automatic copying** (`new` command):
  - Enable with `workon.autoCopyUntracked=true`
  - Uses `workon.copyPattern` if configured, otherwise defaults to `**/*`
  - Respects `workon.copyExclude` always
  - Added `--(no-)copy-untracked` flags to override config
  - Copies from base branch's worktree (or HEAD's worktree if no base specified)
  - Gracefully skips if source worktree doesn't exist
  - Runs after worktree creation, before post-create hooks
- Automatic parent directory creation for nested files
- Skips directories (only copies files)
- 8 integration tests for standalone command + 6 integration tests for automatic copying in `new`
- Total: 14 comprehensive tests covering all copying scenarios

**Configuration**:
- `workon.autoCopyUntracked` - Boolean (default: false), enables automatic copying in `new` command
- `workon.copyPattern` - Multi-value, glob patterns to copy (default: `**/*` if not set)
- `workon.copyExclude` - Multi-value, glob patterns to exclude from copying

### 2.3 Protected Branches ✅

- **Status**: Completed (implemented in Phase 1)
- **Priority**: Medium
- **Description**: Prevent accidental pruning of critical branches
- **Depends On**: Configuration System (1.1)
- **Enhancement To**: Prune command (already implemented)
- **Tasks**:
  - [x] Read `workon.pruneProtectedBranches` config (glob patterns)
  - [x] Never prune worktrees for branches matching protected patterns
  - [x] Show warning when protected branch would be pruned
  - [x] Write tests for protected branch detection

**Implementation Notes**:
- Integrated into prune command during Phase 1
- Simple glob matching: exact match, `*` wildcard, and `prefix/*` patterns
- Protected branches are skipped and shown in "Skipped" output
- 4 integration tests covering exact match, glob patterns, and multiple patterns

**Future Enhancements** (moved to Phase 5):
- Add `--force` flag to override protection
- Suggest default protection patterns in documentation

---

## Phase 3: Enhanced Discovery

**Goal**: Make finding and navigating worktrees fast and intuitive

### 3.1 Status Filtering

- **Priority**: High
- **Description**: Filter worktrees by status for better discovery
- **Enhancement To**: List and find commands
- **Tasks**:
  - [ ] Add `--dirty` flag - show only worktrees with uncommitted changes
  - [ ] Add `--clean` flag - show only worktrees without uncommitted changes
  - [ ] Add `--ahead` flag - show worktrees with unpushed commits
  - [ ] Add `--behind` flag - show worktrees behind their upstream
  - [ ] Add `--gone` flag - show worktrees whose upstream branch is deleted
  - [ ] Support combining multiple filters (AND logic)
  - [ ] Add filters to `list` command output
  - [ ] Add filters to interactive `find` mode (when implemented)
  - [ ] Add `--json` output option for programmatic filtering
  - [ ] Optimize status checks for performance with many worktrees
  - [ ] Write tests for all filter combinations

### 3.2 Interactive Modes & Fuzzy Matching

- **Priority**: High
- **Description**: Interactive selection and fuzzy matching for worktrees
- **Research**: Evaluate `skim` vs `fzf` integration
  - skim: Rust library, can be embedded
  - fzf: External dependency, more widely used
- **Tasks**:
  - [ ] Research and choose interactive library (skim vs fzf)
  - [ ] Implement interactive `find` (list all worktrees when no name provided)
  - [ ] Implement interactive `new` (prompt for name when not provided)
  - [ ] Add fuzzy matching for branch names in find
  - [ ] Handle multiple matches (show list, let user pick)
  - [ ] Integrate status filtering into interactive mode
  - [ ] Show metadata in interactive list (branch, status, age, ahead/behind)
  - [ ] Add search/filter capabilities in interactive mode
  - [ ] Consider prefix/suffix matching strategies
  - [ ] Write tests for interactive flows and fuzzy matching

---

## Phase 4: Advanced Workflows

**Goal**: Support modern development workflows

### 4.1 Pull Request Support ✅

- **Status**: Completed
- **Priority**: Medium-High
- **Description**: Create worktrees directly from pull request URLs
- **Depends On**: Configuration System (for PR naming format)
- **Examples**:
  - `git workon #123` - Smart routing creates worktree for PR #123
  - `git workon new pr#123` - Explicit PR creation
  - `git workon new https://github.com/user/repo/pull/123` - From URL
- **Tasks**:
  - [x] Parse GitHub PR URLs (github.com/user/repo/pull/123)
  - [x] Parse short PR references (pr#123, pr-123, #123)
  - [x] Fetch PR branch from appropriate remote
  - [x] Determine worktree name using `workon.prFormat` config
  - [x] Default format: `pr-{number}`
  - [x] Set up tracking to PR head branch
  - [x] Auto-detect remote (upstream > origin > first remote)
  - [x] Smart routing: `git workon #123` automatically creates PR worktree
  - [x] Write tests for PR URL parsing and worktree creation (16 tests total)
  - [x] Document PR workflow patterns and examples
  - [ ] Support format variables: `{title}`, `{author}` (deferred - requires gh CLI)
  - [ ] Handle fork-based PRs (fetch from fork remote) (deferred - future enhancement)
  - [ ] Integration with `gh` CLI for metadata (deferred - optional enhancement)
  - [ ] Support GitLab merge requests (deferred - stretch goal)

**Implementation Notes**:
- Created `git-workon-lib/src/pr.rs` module with parsing, remote detection, and fetching logic
- Added `PrError` enum to error.rs with diagnostic help messages
- Enhanced `new` command to detect PR references and handle PR workflow
- Smart routing in main.rs automatically routes `#123` patterns to `new` command
- Auto-fetches PR refs if not present locally using `git fetch <remote> +refs/pull/{number}/head:...`
- 10 unit tests for PR parsing + 6 integration tests for remote detection and config
- Configuration already existed: `workon.prFormat` with `{number}` placeholder validation

### 4.2 Move Command

- **Priority**: Medium
- **Description**: Rename branch and worktree directory atomically
- **Examples**:
  - `git workon move old-name new-name`
  - `git workon move feature/foo bugfix/foo`
- **Use Cases**:
  - Change branch naming convention
  - Reorganize namespaces
  - Fix typos in branch names
- **Tasks**:
  - [ ] Implement atomic operation: rename branch + move worktree directory
  - [ ] Use `git branch -m` for branch rename
  - [ ] Move worktree directory to match new branch name
  - [ ] Update tracking branch configuration if applicable
  - [ ] Handle namespace changes (different directory hierarchy)
  - [ ] Validate new name doesn't conflict with existing worktree
  - [ ] Add safety checks (dirty worktree, unpushed commits)
  - [ ] Add `--allow-dirty` and `--allow-unpushed` override flags
  - [ ] Support `--force` to override all safety checks
  - [ ] Update shell integration cache if active (future: Phase 5)
  - [ ] Write tests for rename scenarios and edge cases
  - [ ] Handle errors gracefully (partial rename recovery)

### 4.3 Smart Worktree Management

- **Priority**: Medium
- **Description**: Intelligent worktree lifecycle and health management
- **Tasks**:
  - [ ] Detect and warn about forgotten worktrees (not touched in X days)
  - [ ] Add `--stale` filter to list worktrees by last activity
  - [ ] Auto-prune merged branches (with confirmation)
  - [ ] Suggest worktree reuse based on activity patterns
  - [ ] Track worktree creation reasons/contexts (stretch goal)
  - [ ] Add worktree notes/descriptions (stretch goal)

### 4.4 Doctor Command

- **Priority**: Medium
- **Description**: Detect and repair workspace issues
- **Foundation**: Use `git worktree repair` for git-level issues
- **Tasks**:
  - [ ] Implement `doctor` command to detect issues:
    - [ ] Orphaned worktrees (directory exists but not in git worktree list)
    - [ ] Missing worktree directories (in git list but directory deleted)
    - [ ] Broken git links (.git file pointing to non-existent location)
    - [ ] Inconsistent administrative files
    - [ ] Worktrees on stale/deleted branches
  - [ ] Report issues with clear descriptions and suggested fixes
  - [ ] Add `--fix` flag to automatically repair detected issues
  - [ ] Use `git worktree repair` for fixable git issues
  - [ ] Add dry-run mode to preview fixes without applying
  - [ ] Handle non-repairable issues gracefully (suggest manual intervention)
  - [ ] Write tests for detection and repair scenarios

---

## Phase 5: Shell Integration & Polish

**Goal**: Polish the user experience and integrate with shell workflows

### 5.1 Shell Integration

- **Priority**: Medium
- **Description**: Fast directory switching like zoxide
- **Reference**: Study zoxide implementation
- **Tasks**:
  - [ ] Design shell hook architecture (bash, zsh, fish)
  - [ ] Implement frequency/recency tracking for smart defaults
  - [ ] Create shell completion scripts
  - [ ] Add `cd` integration for automatic worktree switching
  - [ ] Consider `git workon jump` or `git workon switch` command
  - [ ] Update cache when `move` command is used
  - [ ] Write documentation for shell setup
  - [ ] Provide init scripts for each supported shell

### 5.2 Better Output & Reporting

- **Priority**: Medium
- **Tasks**:
  - [ ] Add colored output for status/errors (respect NO_COLOR)
  - [ ] Improve error messages with actionable suggestions
  - [ ] Add `--json` output for programmatic use
  - [ ] Add `--verbose` flag for debugging
  - [ ] Pretty-print worktree lists with aligned columns
  - [ ] Include branch status details (ahead/behind, remote)
  - [ ] Add status indicators (symbols/colors for dirty, ahead, behind)
  - [ ] Support `--porcelain` for stable script-friendly output
  - [ ] Add `--force` flag to prune command to override protection (deferred from Phase 2.3)

### 5.3 Interactive Configuration Management

- **Priority**: Low
- **Description**: Interactive command for managing git-workon configuration
- **Depends On**: Configuration System (Phase 1, completed)
- **Tasks**:
  - [ ] Implement `git workon config` command
  - [ ] Interactive prompts for setting common config keys
  - [ ] Show current config values
  - [ ] Validate input before writing to git config
  - [ ] Support both global and local config scopes
  - [ ] Provide helpful examples and defaults for each setting
  - [ ] Write tests for interactive config flows

### 5.4 Documentation

- **Priority**: Medium
- **Tasks**:
  - [ ] Write comprehensive user guide
  - [ ] Add workflow examples (PR review, feature development, etc.)
  - [x] Document configuration schema and keys (completed in Phase 1 implementation notes)
  - [ ] Document all configuration options with detailed examples and use cases
  - [ ] Document protected branch patterns with recommended defaults (deferred from Phase 2.3)
  - [ ] Create troubleshooting guide
  - [ ] Add architecture documentation for contributors
  - [ ] Document security considerations (hooks, PR fetching)
  - [ ] Record demo videos/screencasts (optional)

---

## Phase 6: Stacked Diffs (Stretch Goal)

**Goal**: Support stacked diff workflows

### 6.1 Stacked Diffs Support

- **Priority**: Low (Future)
- **Description**: Compatibility with stacked diff workflows (git-branchless, Graphite, Sapling, spr, etc.)
- **Research Needed**:
  - [x] Study git-branchless workflows
  - [x] Study Graphite CLI patterns
  - [x] Study sapling workflows
  - [x] Study spr workflows
  - [x] Identify worktree-specific challenges
- **Potential Tasks**:
  - [ ] Support creating worktrees for stack levels
  - [ ] Handle branch dependencies in metadata
  - [ ] Add `--parent` flag to `new` for stacked branches
  - [ ] Visualize branch stacks across worktrees

---

## Implementation Notes

### Dependencies Between Features

```
Phase 1: Configuration System
  ↓
Phase 2: Post-Creation Hooks, Enhanced File Copying, Protected Branches
  ↓
Phase 3: Status Filtering, Interactive Modes
  ↓
Phase 4: PR Support, Move, Doctor
  ↓
Phase 5: Shell Integration, Polish
```

### Git Config Multi-Value Pattern

Git config naturally supports multi-value entries, perfect for our use cases:

```bash
# Set multiple values
git config --add workon.copyPattern '.env*'
git config --add workon.copyPattern '.vscode/'

# Read all values
git config --get-all workon.copyPattern
```

### Hooks: Hybrid Approach

**Why both git hooks and config?**

Git's native `post-checkout` hook fires on `git worktree add`, but has limitations:

- Only one script per hook (requires manual multiplexing for multiple behaviors)
- Fires for ALL checkouts, requiring conditional logic to detect worktree creation
- Requires shell scripting knowledge

The `workon.postCreateHook` config provides a simpler alternative:

- Only runs for `git workon new` (explicit, no detection needed)
- No scripting required: `git config --add workon.postCreateHook "npm install"`
- Doesn't conflict with existing post-checkout hooks
- Multi-value config allows multiple commands

**Implementation notes**:

- Git's post-checkout runs first (standard git behavior)
- Then workon.postCreateHook commands run (if configured)
- `--no-hooks` flag skips both (respects user intent)
- Document both approaches, let users choose based on their needs

**Example post-checkout for worktree detection**:

```bash
#!/bin/bash
# .git/hooks/post-checkout
if [ "$1" = "0000000000000000000000000000000000000000" ]; then
    echo "New worktree created at $PWD"
    # Your setup commands here
fi
```

### Security Considerations

- **Post-creation hooks**: Execute arbitrary commands - document risks, consider requiring explicit opt-in
- **PR support**: Fetches from remote - validate URLs, handle authentication properly
- **File copying**: Respect gitignore, avoid copying sensitive files by default

### Performance Considerations

- Status filtering with many worktrees needs optimization
- Consider caching for shell integration
- Lazy evaluation where possible (don't check all worktree status unless needed)

---

## Design Principles

1. **Git-like**: Use git config, follow git conventions, feel like a native git command
2. **Safe by default**: Protect against accidents, require explicit confirmation for destructive actions
3. **Composable**: Features work well together (hooks + PR support, filters + interactive mode)
4. **Scriptable**: Provide `--json` output, clear exit codes, `--porcelain` for scripts
5. **Progressive enhancement**: Core features work without optional dependencies (gh CLI, shell integration)
