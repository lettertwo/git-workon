# PR Worktree Creation

When a name is detected as a PR reference (in `new` or via smart routing), the PR workflow fetches metadata via `gh` CLI, sets up the correct remote, fetches the branch, and creates a tracked worktree.

```mermaid
sequenceDiagram
    participant User
    participant Main as main.rs / cmd/new.rs
    participant PR as pr.rs
    participant GH as gh CLI (external)
    participant Git2 as git2 / libgit2
    participant Worktree as worktree.rs

    User->>Main: git workon #123
    Note over Main: is_pr_reference("#123") → true
    Main->>Main: load WorkonConfig
    Main->>PR: parse_pr_reference("#123")
    PR-->>Main: PullRequest { number: 123, remote: None }

    Main->>PR: prepare_pr_worktree(repo, 123, pr_format)

    rect rgba(220, 235, 255, 0.25)
        Note over PR,GH: Step 1 — Fetch metadata
        PR->>GH: gh pr view 123 --json number,title,author,headRefName,baseRefName,isCrossRepository,headRepository
        GH-->>PR: JSON response
        PR->>PR: parse PrMetadata { number, title, author, head_ref, base_ref, is_fork, fork_owner, fork_url }
    end

    rect rgba(220, 255, 220, 0.25)
        Note over PR,Git2: Step 2 — Setup remote
        alt is_fork == true
            PR->>Git2: repo.remotes() — check for "pr-123-fork"
            alt fork remote missing
                PR->>Git2: repo.remote("pr-123-fork", fork_url)
            end
            Note over PR: remote_name = "pr-123-fork"
        else is_fork == false
            PR->>Git2: repo.remotes()
            Note over PR: priority: "upstream" → "origin" → first remote
            Note over PR: remote_name = detected remote
        end
    end

    rect rgba(255, 245, 220, 0.25)
        Note over PR,Git2: Step 3 — Fetch branch
        PR->>Git2: check refs/remotes/<remote>/<head_ref> exists?
        alt ref already exists
            Note over PR: skip fetch
        else ref missing
            PR->>Git2: remote.fetch(["+refs/heads/<head_ref>:refs/remotes/<remote>/<head_ref>"])
        end
    end

    rect rgba(240, 220, 255, 0.25)
        Note over PR,Main: Step 4 — Format name
        PR->>PR: format_pr_name_with_metadata(pr_format, metadata)
        Note over PR: replaces {number}, {title}, {author}, {branch}<br/>sanitizes title/author/branch for branch names
        PR-->>Main: (worktree_name, remote_ref, base_ref)
        Note over Main: e.g. ("pr-123", "origin/fix-auth", "main")
    end

    rect rgba(255, 220, 220, 0.25)
        Note over Main,Worktree: Step 5 — Create worktree + track
        Main->>Worktree: add_worktree(repo, worktree_name, Normal, Some(remote_ref))
        Worktree-->>Main: WorktreeDescriptor
        Main->>Worktree: set_upstream_tracking(worktree, remote_name, branch_ref)
        Note over Worktree: sets branch.<name>.remote and branch.<name>.merge<br/>in worktree's git config
    end

    rect rgba(220, 255, 240, 0.25)
        Note over Main: Step 6 — Copy + hooks
        Main->>Main: copy_untracked_files (if configured)
        Main->>Main: execute_post_create_hooks (unless --no-hooks)
        Main-->>User: prints worktree path
    end
```

## PR reference formats

`parse_pr_reference()` in `pr.rs` recognises these input patterns:

| Format | Example | Notes |
|---|---|---|
| `#N` | `#123` | GitHub shorthand, most common |
| `pr#N` | `pr#123` | Explicit PR prefix |
| `pr-N` | `pr-123` | Dash variant |
| GitHub URL | `https://github.com/owner/repo/pull/123` | Full URL |
| Remote ref | `origin/pull/123/head` | Direct remote ref |

`is_pr_reference()` wraps `parse_pr_reference()` for quick boolean routing checks.

## prFormat placeholders

`workon.prFormat` (default: `pr-{number}`) supports these placeholders. Title, author, and branch values are sanitized (lowercased, non-alphanumeric chars → `-`, consecutive dashes collapsed):

| Placeholder | Source |
|---|---|
| `{number}` | PR number (required) |
| `{title}` | PR title (sanitized) |
| `{author}` | GitHub login of PR author (sanitized) |
| `{branch}` | Head branch name (sanitized) |

## Key files

- `git-workon-lib/src/pr.rs` — all PR parsing, metadata fetching, remote setup, `prepare_pr_worktree()`
- `git-workon/src/cmd/new.rs` — PR detection in `run()`, upstream tracking, copy/hooks after PR worktree
- `git-workon/src/main.rs` — `route_pr_ref_to_command()` for smart routing
