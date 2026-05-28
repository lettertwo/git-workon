# 004 — Smart Routing When No Subcommand Is Given

## Context

The most common daily uses of the tool are "switch to a PR" and "switch to an existing worktree". Requiring users to type `git workon new #123` or `git workon find feature` for every invocation adds friction. We wanted `git workon <arg>` to do the right thing based on what the argument looks like.

## Decision

When `git workon` is called without an explicit subcommand, `main.rs` inspects the positional argument and routes to either `new` or `find`:

1. If no argument is given → route to `find` (interactive selection).
2. If the argument matches `is_pr_reference()` (e.g. `#123`, `pr-123`, a GitHub PR URL) AND the corresponding worktree does not already exist → route to `new` with the PR reference pre-filled.
3. If the argument matches `is_pr_reference()` AND the worktree already exists → route to `find` with the formatted name.
4. If a worktree matching the name already exists → route to `find` (let it handle the match).
5. If a local branch or a remote tracking branch (by short name) matches the argument AND no worktree exists yet → route to `new` (auto-attach to the existing branch).
6. Otherwise → route to `find` with the name as the search term.

This allows `git workon #123` to create a PR worktree, `git workon feature` to attach or find the `feature` branch, and `git workon some-name` to find an existing worktree — with no explicit subcommand needed.

## Consequences

- Common operations are one word shorter, reducing daily friction.
- The routing logic is concentrated in `main.rs` and must be kept in sync with `is_pr_reference()` in the library.
- Any ambiguous argument (a name that could be both a branch and a PR) follows the "PR-first" path; the fallback to `find` if the worktree exists handles the overlap.
- JSON mode (`--json`) sets `find.no_interactive = true` to prevent interactive prompts in scripted contexts.

## References

- `docs/diagrams/command-dispatch.md` — full routing flowchart
- `git-workon/src/main.rs` — `route_pr_ref_to_command()`, `route_branch_to_command()`, `branch_exists()`
- `git-workon-lib/src/pr.rs` — `is_pr_reference()`, `parse_pr_reference()`
