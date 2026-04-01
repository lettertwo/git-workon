# AI Coding Agents and git-workon: Integration Research

## Executive Summary

Worktree-capable AI coding agents (Claude Code, Cursor, Cline, and others) are increasingly using git worktrees for isolated parallel work. These agents roll their own worktree management — creating worktrees outside the project root or in non-standard locations, on auto-generated branches, with agent-specific setup scripts — completely bypassing git-workon's opinionated layout, hooks, config, and lifecycle management.

This represents a missed opportunity in both directions: agents don't benefit from git-workon's features (consistent layout, post-create hooks, copy-untracked, pruning with safety checks), and git-workon's `list`, `find`, and `prune` commands don't see agent-managed worktrees. The two systems coexist as strangers.

The research identifies three viable integration models, ordered by leverage:

1. **Hook delegation** (highest leverage, least agent friction): Claude Code's `WorktreeCreate` hook and Cursor's `setup-worktree` script are designed for exactly this — delegating worktree lifecycle to an external tool. A short wrapper script can make git-workon the backend for both without any changes to git-workon itself.

2. **MCP server** (new capability, most agent-native): The official `mcp-server-git` has no worktree tools. A `git-workon` MCP server would fill this gap, exposing `worktree_create`, `worktree_find`, `worktree_list`, and `worktree_remove` as first-class tools that any MCP-capable agent can use with zero agent-specific configuration.

3. **Direct CLI** (usable today, requires agent setup): git-workon's existing `--json` / `--no-interactive` interface is already suitable for scripted agent use. The `new`, `find`, `list`, and `prune` commands cover the full lifecycle. The primary gap is structured error output (errors are currently Miette human-readable text, not machine-parseable JSON).

The MCP model is the most compelling long-term direction because it is agent-agnostic and provides the richest integration surface. Hook delegation is the right recommendation for Claude Code users today, before any new code is written.

---

## How Agents Use Worktrees Today

### Claude Code

Claude Code has first-class worktree support via the `--worktree <name>` CLI flag and an `isolation: "worktree"` subagent mode. When activated:

- **Location**: `<repo>/.claude/worktrees/<name>/` (inside the repo)
- **Branch**: `worktree-<name>`, branched from the default remote branch
- **Auto-naming**: If no name is given, generates a random name like `bright-running-fox`
- **Cleanup**: Worktrees with no changes are removed automatically on session exit; worktrees with commits prompt the user to keep or remove
- **Recommendation**: `.claude/worktrees/` should be added to `.gitignore`

The `EnterWorktree` and `ExitWorktree` internal tools implement this lifecycle. For parallel agents, each subagent with `isolation: "worktree"` gets its own temporary worktree, created and cleaned up automatically.

**The integration point — `WorktreeCreate`/`WorktreeRemove` hooks:**

Claude Code's hooks system (configured in `settings.json`) includes two lifecycle hooks designed specifically for worktree management:

```json
{
  "hooks": {
    "WorktreeCreate": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "my-worktree-script.sh"
          }
        ]
      }
    ]
  }
}
```

When a `WorktreeCreate` hook is configured, it **completely replaces** Claude Code's default `git worktree add` behavior. The hook receives a JSON object via stdin:

```json
{
  "hook_event_name": "WorktreeCreate",
  "session_id": "abc123",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/current/working/directory",
  "permission_mode": "default",
  "name": "feature-auth"
}
```

The hook **must print the absolute path** of the created worktree to stdout. Diagnostic output must go to stderr to avoid interfering with path detection. Non-zero exit code causes creation to fail. Exit code 2 causes stderr to be fed back to Claude as an error message.

`WorktreeRemove` receives `{"hook_event_name": "WorktreeRemove", "worktree_path": "/absolute/path"}` but cannot block removal — it is notification-only.

These hooks were explicitly designed for delegating to alternative VCS systems (SVN, Perforce, Mercurial), but git-workon is an ideal target: same underlying VCS, richer lifecycle management.

### Cursor

Cursor's background agents have first-class worktree support with a different layout philosophy:

- **Location**: `~/.cursor/worktrees/<repo>/<random-id>/` (user's home directory, outside the repo)
- **Configuration**: `.cursor/worktrees.json` in the repo root with per-OS setup scripts:
  ```json
  {
    "setup-worktree-unix": "npm install && cp .env.example .env",
    "setup-worktree-windows": "npm install && copy .env.example .env"
  }
  ```
- **Parallel work**: Cursor can run multiple agents simultaneously in separate worktrees ("Best-of-N" for comparing model outputs)
- **Limit**: Maximum 20 worktrees per workspace; oldest removed automatically

The `setup-worktree` script is analogous to git-workon's `postCreateHook`, but it is agent-specific configuration in `.cursor/worktrees.json` rather than git config. There is no hook mechanism comparable to `WorktreeCreate` — the setup script runs after worktree creation, not instead of it.

### Cline

Cline (VS Code extension) has active worktree development but treats worktrees primarily as a viewing and navigation feature rather than agent isolation infrastructure. PRs for "git worktree view" and "support for git worktree in commit generation" are recent, suggesting worktree-aware tooling is in progress but not yet a first-class isolation primitive.

### Aider

Aider is a single-agent, single-branch tool that does not create or manage worktrees itself. It correctly operates inside an existing worktree (a past bug with `InvalidGitRepositoryError` was fixed in 2023), but offers no mechanism for delegating worktree lifecycle to an external tool.

### Windsurf

No documented worktree isolation strategy comparable to Claude Code or Cursor. Windsurf did not surface in research as having agent-driven worktree creation.

### Common Patterns Across Agents

| Agent       | Creates worktrees? | Location                      | Setup delegation                        | Cleanup           |
| ----------- | ------------------ | ----------------------------- | --------------------------------------- | ----------------- |
| Claude Code | Yes                | `<repo>/.claude/worktrees/`   | `WorktreeCreate` hook replaces creation | Auto on exit      |
| Cursor      | Yes                | `~/.cursor/worktrees/<repo>/` | `setup-worktree` script post-creation   | 20-worktree limit |
| Cline       | Partial            | N/A                           | None documented                         | Manual            |
| Aider       | No                 | N/A                           | N/A                                     | N/A               |

The critical observation: **agents that create worktrees do so entirely outside git-workon's layout**. Claude Code and Cursor each have their own convention for where worktrees live and what branch names to use. None of these worktrees appear in `git workon list`. None benefit from `postCreateHook`. None are considered when `git workon prune` runs.

---

## Integration Surface Analysis

### What git-workon Currently Offers for Programmatic Use

git-workon has a strong foundation for agent integration:

**Machine-readable output**: `--json` flag on all commands. Single-worktree commands output an object; `list` and `prune` output arrays. The stdout/stderr separation is clean — all diagnostic output goes to stderr, all machine-readable output to stdout.

**Non-interactive mode**: `--no-interactive` on `new` and `find` disables the interactive TUI. Combined with `--json`, these commands are fully pipeable.

**Rich filtering**: `--dirty`, `--clean`, `--ahead`, `--behind`, `--gone` filters on `list` and `find` enable programmatic worktree discovery without parsing.

**Hook system**: `workon.postCreateHook` in git config fires after `new`, `clone`, and `init`. Hooks run in the worktree directory with `WORKON_WORKTREE_PATH`, `WORKON_BRANCH_NAME`, and `WORKON_BASE_BRANCH` environment variables. Multiple hooks execute sequentially. Configurable timeout (default 300s).

**WorktreeDescriptor JSON schema**:

```json
{
  "name": "string",
  "path": "string | null",
  "branch": "string | null",
  "head_commit": "string | null",
  "is_dirty": "boolean | null",
  "is_locked": "boolean | null",
  "has_unpushed_commits": "boolean | null",
  "is_behind_upstream": "boolean | null",
  "has_gone_upstream": "boolean | null",
  "remote": "string | null",
  "remote_branch": "string | null",
  "remote_url": "string | null",
  "last_activity": "string | null"
}
```

Fields that error default to `null` rather than failing the command — a correct fail-safe for partial status information.

**Structured error output**: In `--json` mode, errors emit a JSON object to stdout before exiting non-zero. Error codes are derived from Miette's `#[diagnostic(code(...))]` attributes and are stable API surface:

```json
{
  "error": {
    "code": "workon::worktree::not_found",
    "message": "worktree 'foo' does not exist"
  }
}
```

See `docs/adr/021-structured-json-error-protocol.md` for full design rationale.

### Current Gaps for Agent Integration

**1. ~~No machine-readable error output~~ (implemented)**

~~Errors use Miette's human-readable diagnostic format on stderr.~~ In `--json` mode, errors now emit a structured JSON object to stdout before exiting non-zero. Error codes come from Miette's `#[diagnostic(code(...))]` attributes and are stable API surface. Human-readable Miette output on stderr is unchanged in non-JSON mode. See `docs/adr/021-structured-json-error-protocol.md`.

**2. No pre-creation hook**

The hook system only fires _after_ worktree creation. There is no hook to validate or customize the worktree path, branch name, or base before creation. For agent integration via `WorktreeCreate` delegation (where git-workon _is_ the creation hook), this is fine — but for hooks running _within_ git-workon's own lifecycle, pre-creation hooks would be useful for naming validation, branch existence checks, or custom path selection.

**3. Limited hook data**

Hooks receive only `WORKON_WORKTREE_PATH`, `WORKON_BRANCH_NAME`, and `WORKON_BASE_BRANCH`. For agent scenarios, additional context would be useful: the PR number (if created from a PR reference), whether the worktree was created by an agent, the session/request ID. These are not blockers but would improve hook utility in agent contexts.

**4. No session or ownership tracking**

git-workon has no concept of which process or agent created a worktree, whether it is currently active, or when it was last used. `last_activity` in WorktreeDescriptor approximates this from filesystem timestamps but is not authoritative.

The minimum viable session awareness — using git's native lock mechanism — is now implemented: `git workon new --lock` atomically creates and locks a worktree, `prune` skips locked worktrees by default, and `--include-locked` overrides this. `is_locked` is exposed in the JSON schema. Full session/ownership tracking (session IDs, agent identity) remains unimplemented.

**5. `--no-interactive` not universal**

`--no-interactive` applies to `new` and `find` but not to all commands. `shell-init` returns interactive shell code (by design). Some subcommand behaviors (e.g., `prune` confirmation) use `--yes` instead of `--no-interactive` for consistency, which is fine but requires callers to know the right flag per command.

---

## Integration Models

### Model A: Hook Delegation

**How it works**: When Claude Code's `WorktreeCreate` hook fires (via `--worktree` or `isolation: "worktree"`), a hook script calls `git workon new` and prints the resulting path to stdout.

```bash
#!/bin/bash
# .claude/hooks/worktree-create.sh
set -e
NAME=$(jq -r .name)
# Use git-workon to create the worktree with full lifecycle management
PATH=$(git workon new "$NAME" --no-interactive --json | jq -r .path)
echo "$PATH"
```

Settings configuration:

```json
{
  "hooks": {
    "WorktreeCreate": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "bash /path/to/worktree-create.sh"
          }
        ]
      }
    ]
  }
}
```

**What this enables**:

- All Claude Code `--worktree` and `isolation: "worktree"` usage goes through git-workon's layout (siblings under the bare repo root)
- Post-create hooks (dependency install, env setup) fire automatically
- Copy-untracked fires if configured
- Agent-created worktrees appear in `git workon list`
- `git workon prune` can manage agent worktrees

**What this doesn't enable**:

- `WorktreeRemove` cannot use git-workon's `prune` (it's notification-only, not a replacement)
- Cursor's `setup-worktree` script runs _after_ creation, so the worktree is already at `~/.cursor/worktrees/` — there is no creation hook to intercept

**Assessment**: Highest leverage, zero new code required, deployable today as a documentation recipe. The primary audience is users who already use git-workon and want Claude Code agents to operate inside the managed worktree layout.

### Model B: Direct CLI Usage

**How it works**: Agents call `git workon` directly instead of `git worktree add`. This requires agent configuration or customization — telling the agent "use `git workon new` instead of `git worktree add`" via a system prompt, CLAUDE.md instruction, or similar mechanism.

CLAUDE.md approach (already works):

```markdown
## Worktree Management

When creating git worktrees, always use `git workon new <name> --no-interactive`
instead of `git worktree add`. This ensures correct layout and setup.
Use `git workon list --json` to discover existing worktrees.
```

**What this enables**:

- Full git-workon lifecycle for agent-created worktrees
- Works with any agent that follows CLAUDE.md instructions
- No changes to git-workon required

**What this doesn't enable**:

- Agents using `EnterWorktree`/`ExitWorktree` tools internally bypass CLAUDE.md-level instructions
- No guarantee agents won't fall back to direct `git worktree add` calls
- Requires per-project configuration maintenance

**Assessment**: Usable today for agents that follow instructions (Claude Code with CLAUDE.md, Aider with `--instructions`). Not reliable for agents that use built-in worktree tooling that bypasses user instructions. Best suited as a complement to Model A or C, not a standalone strategy.

### Model C: MCP Server

**How it works**: git-workon exposes an MCP server that provides worktree management as first-class tools. Any MCP-capable agent adds `git-workon-mcp` to its MCP server configuration and gets rich, structured worktree operations without any CLAUDE.md instructions or hook configuration.

The official `mcp-server-git` has 12 tools for git operations but **no worktree tools**. A git-workon MCP server would fill this gap.

Proposed tool set (following MCP naming conventions):

| Tool                      | Parameters                                      | Returns                |
| ------------------------- | ----------------------------------------------- | ---------------------- |
| `worktree_list`           | `repo_path?`, filters                           | `WorktreeDescriptor[]` |
| `worktree_create`         | `repo_path`, `name`, `base?`, `copy_untracked?` | `WorktreeDescriptor`   |
| `worktree_find`           | `repo_path`, `name`                             | `WorktreeDescriptor`   |
| `worktree_remove`         | `repo_path`, `name`, `force?`                   | `{removed: true}`      |
| `worktree_create_from_pr` | `repo_path`, `pr_ref`                           | `WorktreeDescriptor`   |

MCP server configuration (in `~/.claude/settings.json` or project `.claude/mcp.json`):

```json
{
  "mcpServers": {
    "git-workon": {
      "command": "git-workon-mcp",
      "args": ["--repository", "/path/to/repo"]
    }
  }
}
```

**Implementation options**:

- **Standalone binary** (`git-workon-mcp`): A separate crate that depends on `git-workon-lib` and implements the MCP stdio protocol. Clean separation, independent versioning, focused scope.
- **Subcommand** (`git workon mcp`): Adds MCP server mode as a subcommand of the main binary. Simpler distribution (one binary), slightly larger binary size.
- **Library** (`workon` crate): Export MCP handler functions from git-workon-lib for embedding in other tools.

**What this enables**:

- Agent-agnostic: any MCP client works (Claude Code, Cursor if it adds MCP support, etc.)
- Structured tool calls and responses — agents get typed results, not text parsing
- Richer operation context: agents can specify intent (base branch, copy patterns) per operation
- Discoverable: agents can introspect available tools without documentation
- Composable with `mcp-server-git`: both can be active simultaneously for full git + worktree coverage

**What this requires**:

- New code: MCP server implementation (the MCP protocol is a JSON-RPC stdio protocol — not trivial but well-documented)
- Dependencies: An MCP Rust crate or custom JSON-RPC implementation
- The structured error gap becomes important here — MCP tools must return structured errors

**Assessment**: The highest long-term leverage. Creates a clean, agent-agnostic integration surface. Requires the most new code but aligns with how agents are evolving toward tool-centric interaction. The gap in `mcp-server-git` is real — worktree tools are missing and there is no other MCP server filling this role.

### Model D: Library API

**How it works**: git-workon-lib (`workon` crate on crates.io) is used as a Rust library dependency in agent tooling or other Rust git tools.

**Assessment**: Very narrow audience. The `workon` crate is published but not prominently positioned as a library. Agents are not typically written in Rust, and the library API is not currently stable enough to advertise as a first-class integration surface. This model is viable as a long-term foundation (the MCP server could be built on top of it) but is not a practical user-facing integration strategy.

---

## Concurrency and Multi-Agent Scenarios

### Git's Built-in Worktree Safety

Git's `--lock` flag on `git worktree add` is specifically designed to prevent a race condition:

> "Keep the worktree locked after creation. This is the equivalent of `git worktree lock` after `git worktree add`, but **without a race condition**."

The race condition in question is: create worktree → window of vulnerability → lock it. The `--lock` flag atomically creates and locks in one step. However, the `locked` state controls _pruning_ (preventing `git worktree prune` from removing the worktree), not _creation serialization_.

For concurrent worktree creation by multiple agents:

- **Different branches, different paths**: Generally safe. Each `git worktree add` writes a new subdirectory under `$GIT_DIR/worktrees/` with a unique ID. POSIX directory creation is atomic.
- **Same branch, different paths**: The second agent will fail with "already checked out" unless `--force` is used. `--force` creates a shared-branch situation that most workflows should avoid.
- **Same path**: The second agent will fail with "path already exists." Path conflicts are the most likely collision in high-concurrency scenarios.

### Naming and Layout Conflict Risks

Claude Code generates names like `worktree-bright-running-fox`. git-workon uses branch names as worktree names. If an agent creates `worktree-feature-auth` and a human runs `git workon new feature-auth`, they get different branches and different paths — they coexist without conflict but are invisible to each other in their respective tool's listings.

The real risk is layout divergence: Claude Code creates worktrees inside the repo at `.claude/worktrees/`, Cursor creates them at `~/.cursor/worktrees/`, and git-workon creates them as siblings under the bare repo root. None of these arrangements is wrong, but they result in `git worktree list` showing a heterogeneous set of paths that no single tool manages holistically.

### Pruning Safety with Active Agents

`git workon prune` checks for dirty working trees and unpushed commits before removing a worktree, but it has no way to know if an agent is actively using a worktree. A worktree that is clean and in sync could still be an agent's active workspace.

Without full session/ownership tracking, the conservative approach is now in place:

- Agent-created worktrees can be `--lock`'d at creation time via `git workon new --lock`
- `git workon prune` skips locked worktrees by default (with `--include-locked` to override)
- Agents should unlock and then call `prune` (or `WorktreeRemove`) on cleanup

Both additions are implemented: `git workon new --lock` passes `--lock` through to `git worktree add`, and `prune` respects the locked state.

### Application-Level Serialization

For high-concurrency scenarios (many parallel agents all calling `git workon new` simultaneously), application-level serialization around worktree creation may be appropriate. The MCP server model naturally provides this: a single MCP server process serializes tool calls, so even if 10 agents call `worktree_create` simultaneously, the server processes them sequentially. This is a benefit of the MCP architecture over raw CLI invocation.

---

## Key Concepts for git-workon

### 1. Layout Compatibility

**What it is**: The tension between git-workon's opinionated bare-repo-sibling layout and agents' default locations (`.claude/worktrees/`, `~/.cursor/worktrees/`).

**Implications for git-workon**:

- Hook delegation solves this for Claude Code — worktrees end up in git-workon's layout because git-workon creates them
- `list` and `find` could optionally scan for agent-created worktrees outside the standard layout (but this is complex and error-prone)
- The simpler answer: document the hook delegation pattern so agent worktrees _start_ in the right place

### 2. Lifecycle Ownership

**What it is**: The question of which tool is responsible for a worktree's creation, maintenance, and cleanup. Currently agents own their worktrees end-to-end; git-workon is unaware of them.

**Implications for git-workon**:

- Hook delegation transfers lifecycle ownership to git-workon at creation time
- Agents still control the session — they decide when to clean up
- A `--lock` / `--unlock` mechanism on `new` and `prune` would make lifecycle boundaries explicit
- `WorktreeDescriptor` could expose whether a worktree is locked (this is already in git's worktree metadata)

### 3. Structured Error Protocol (implemented)

**What it is**: Machine-readable error responses when git-workon is called programmatically by agents or the MCP server.

**Status**: Implemented. In `--json` mode, errors output a JSON error object to stdout (and still exit non-zero). Error codes come from Miette's `#[diagnostic(code(...))]` attributes, making them stable API surface. Non-JSON mode (Miette formatting) is unchanged. See `docs/adr/021-structured-json-error-protocol.md`.

### 4. MCP Tool Surface

**What it is**: The set of operations a git-workon MCP server would expose and their contracts.

**Implications for git-workon**:

- The existing JSON output contract is the right basis for MCP tool responses — no new data model needed
- MCP tool errors must be structured; the structured error protocol (Key Concept 3) is a prerequisite
- The MCP server should be a thin wrapper over the CLI (or lib) — it should not contain business logic
- Start with the four core operations (list, create, find, remove) and add PR creation as a fifth

### 5. Session Awareness (minimum viable — implemented)

**What it is**: The ability to distinguish worktrees that are actively in use by agents from idle or abandoned worktrees.

**Status**: Minimum viable session awareness is implemented using git's native lock mechanism: `git workon new --lock` atomically creates and locks a worktree; `prune` skips locked worktrees by default (`--include-locked` to override); `is_locked` is exposed in the WorktreeDescriptor JSON schema. More sophisticated tracking (session IDs, timestamps, agent identity) remains out of scope.

---

## Design Considerations

### What to Do

**Hook delegation documentation first**: Before writing any new code, document the `WorktreeCreate` hook pattern for Claude Code users. This is the highest-leverage action — it works today and requires only a README/docs addition.

**~~Structured JSON errors~~** ✓ Done — In `--json` mode, errors emit a JSON error object to stdout before exiting non-zero. See `docs/adr/021-structured-json-error-protocol.md`.

**~~Respect git's lock state in prune~~** ✓ Done — `git workon prune` skips locked worktrees by default; `--include-locked` overrides.

**~~`new --lock` flag~~** ✓ Done — `git workon new --lock` passes `--lock` through to `git worktree add`.

**MCP server as a separate crate**: When implementing the MCP server, make it a standalone binary (`git-workon-mcp`) that depends on `workon` (git-workon-lib). Keep all worktree logic in the library; the MCP server is just protocol translation.

### What Not to Do

**Don't scan for agent-managed worktrees outside the layout**: Trying to discover worktrees at `.claude/worktrees/`, `~/.cursor/worktrees/`, etc. makes git-workon dependent on agent-specific conventions that will change. The right solution is bringing agent worktrees _into_ git-workon's layout at creation time, not adapting git-workon to discover alien layouts.

**Don't become an agent framework**: git-workon's value is worktree management. It should not implement agent session management, agent coordination, or agent-specific configuration formats. Those are each agent's domain.

**Don't invent a new setup script format**: Cursor has `.cursor/worktrees.json`. Claude Code has `WorktreeCreate` hooks. git-workon has `postCreateHook`. These serve the same purpose. Rather than inventing a fourth format, git-workon's hooks should be the implementation target that the agent-specific formats delegate to.

**Don't require the MCP server for the core workflow**: The hook delegation pattern must work without the MCP server. The MCP server is an enhancement for richer agent integration, not a prerequisite for the basic use case.

**Don't break human workflows to support agents**: `--no-interactive` and `--json` are additive flags. The interactive TUI remains the primary human interface. Agent-friendly defaults (quiet mode, JSON output) should not become the global default.

**Don't serialize worktree creation by default**: Application-level locking in the MCP server is appropriate. Adding a global file lock to `git workon new` would harm parallel human+agent workflows where operations are on different branches.

---

## Recommended Approach

### Phase 1: Documentation and Small Additions (No New Features)

1. **Docs: Hook delegation recipe** — Add `docs/recipes/agent-integration.md` documenting the `WorktreeCreate` hook pattern for Claude Code, including the wrapper script and settings.json configuration.

2. **Docs: Direct CLI scripting guide** — Document the `--json --no-interactive` patterns, list the available filters, and show the full WorktreeDescriptor schema.

3. ✓ **`prune`: Respect locked worktrees** — Skips worktrees where `.git/worktrees/<id>/locked` exists. `--include-locked` flag overrides.

4. ✓ **`new --lock`** — Passes `--lock` to `git worktree add` when this flag is set, atomically creating and protecting from pruning.

### ~~Phase 2: Structured Error Output~~ (complete)

5. ✓ **`--json` error protocol** — In JSON mode, emits `{"error": {"code": "...", "message": "..."}}` to stdout on failure. Error codes come from Miette `#[diagnostic(code(...))]` attributes (stable API surface). See `docs/adr/021-structured-json-error-protocol.md`.

### Phase 3: MCP Server (New Crate)

6. **`git-workon-mcp` crate** — Standalone binary implementing the MCP stdio protocol with five tools: `worktree_list`, `worktree_create`, `worktree_find`, `worktree_remove`, `worktree_create_from_pr`. Depends on `workon` (git-workon-lib). Tool responses use the WorktreeDescriptor JSON schema. Errors use the structured error protocol from Phase 2.

---

## References

### Claude Code

- [Claude Code Hooks Documentation](https://docs.anthropic.com/en/docs/claude-code/hooks)
- [Claude Code Common Workflows](https://docs.anthropic.com/en/docs/claude-code/common-workflows)

### Cursor

- [Cursor Worktrees Configuration](https://cursor.com/docs/configuration/worktrees)

### MCP

- [Model Context Protocol Introduction](https://modelcontextprotocol.io/introduction)
- [MCP Git Server (official reference)](https://github.com/modelcontextprotocol/servers/tree/main/src/git)
- [MCP Examples](https://modelcontextprotocol.io/examples)

### Git Worktrees

- [git-worktree man page](https://git-scm.com/docs/git-worktree)

### Related

- [Cline GitHub Repository](https://github.com/cline/cline)
- [Aider GitHub Repository](https://github.com/Aider-AI/aider)
