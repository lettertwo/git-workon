# 009 — PR Workflow Delegated to `gh` CLI

## Context

Creating a worktree for a pull request requires metadata that is not available from the local git repository: the PR title, author, head branch name, base branch, and whether it is a fork. Options included implementing GitHub API calls directly (requiring OAuth token management and API client code) or shelling out to the existing `gh` CLI tool that users likely already have installed for GitHub workflows.

## Decision

PR metadata is fetched by running `gh pr view <number> --json ...` as a subprocess. The output is parsed into a `PrMetadata` struct. This means `gh` must be installed and authenticated; if it is not, the command returns `PrError::GhNotInstalled`.

The workflow in `prepare_pr_worktree()` (`pr.rs`):

1. Parse the PR reference (supports `#123`, `pr-123`, GitHub URLs, remote refs).
2. Call `gh pr view` to get metadata.
3. Determine the remote: for forks, create or reuse a `pr-<N>-fork` remote; for non-forks, prefer `upstream` → `origin` → first remote.
4. Fetch the branch if not already present locally.
5. Format the worktree name using `workon.prFormat` with placeholder substitution.
6. Return `(worktree_name, remote_ref, base_ref)` to the caller.

## Consequences

- No GitHub API client code or token management in the library; `gh` handles authentication.
- The `gh` CLI is an undeclared runtime dependency; the error message guides users to install it.
- Fork workflows are supported transparently by creating a dedicated remote.
- PR reference parsing is lenient: multiple formats are accepted so users can paste any common PR identifier.

## References

- `docs/diagrams/pr-workflow.md` — full sequence diagram
- `git-workon-lib/src/pr.rs` — `prepare_pr_worktree()`, `parse_pr_reference()`
- `git-workon/src/cmd/new.rs` — upstream tracking, copy/hooks after PR worktree creation
