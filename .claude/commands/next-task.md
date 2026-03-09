---
description: Find and prioritize tasks from TODOs, FIXMEs, and unimplemented work
argument-hint: "[category to focus on]"
allowed-tools:
  - Read
  - Glob
  - AskUserQuestion
  - EnterPlanMode
---

# Task Prioritization

1. **Discover tasks** — Call the `list_tasks` MCP tool from the `todo-comments` server with `path` set to the project root directory.

2. **Categorize** by type (bug fix, feature, refactoring), component (crate/module), complexity, and impact.

3. **Present prioritized options** with:
   - Clear description, file:line location
   - Estimated complexity and impact
   - Recommended order (prioritize: unblocking work > user-facing > bug fixes > well-scoped features)

4. **Ask the user** which task to work on using AskUserQuestion.

5. **Enter plan mode** for the selected task.

## Context

This is the git-workon project, a Rust CLI tool for managing git worktrees. See CLAUDE.md for architectural details.
