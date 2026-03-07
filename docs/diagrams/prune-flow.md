# Prune Command (3-Phase)

`prune` removes stale worktrees in three sequential phases: candidate collection, safety filtering, and execution. Phases are independent — safety checks always run even for explicitly named worktrees.

```mermaid
flowchart TD
    START([git workon prune]) --> SETUP["get_repo()\nget_worktrees()\nload WorkonConfig\nload pruneProtectedBranches"]

    subgraph PHASE1["Phase 1 — Candidate Collection"]
        P1A["explicit names\n(positional args)"]
        P1B["filter-based discovery\n(worktrees not in explicit list)"]

        P1A --> EXP_SCAN["for each name:\nfind worktree by name or branch\n→ PruneReason::Explicit"]
        EXP_SCAN --> EXP_WARN["warn if not found"]

        P1B --> BRANCH_EXISTS{branch\nstill exists?}
        BRANCH_EXISTS -->|no| CAND_DELETED["PruneReason::BranchDeleted\n(always candidate)"]
        BRANCH_EXISTS -->|yes + --gone| UPSTREAM_GONE{upstream\ngone?}
        UPSTREAM_GONE -->|yes| CAND_GONE["PruneReason::RemoteGone"]
        UPSTREAM_GONE -->|no| SKIP_P1["skip (not a candidate)"]
        BRANCH_EXISTS -->|yes + --merged| MERGED_CHECK{is_merged_into\ntarget?}
        MERGED_CHECK -->|yes| CAND_MERGED["PruneReason::Merged(target)"]
        MERGED_CHECK -->|no| SKIP_P1
        BRANCH_EXISTS -->|"yes, no flags"| SKIP_P1
    end

    SETUP --> PHASE1
    PHASE1 --> PHASE2

    subgraph PHASE2["Phase 2 — Safety Checks (per candidate)"]
        S1{protected\nbranch?}
        S1 -->|"yes + !force"| SKIP_PROTECTED["skip: protected\nby pruneProtectedBranches"]
        S1 -->|no or force| S2

        S2{is default\nbranch?}
        S2 -->|"yes + !force"| SKIP_DEFAULT["skip: is default worktree"]
        S2 -->|no or force| S3

        S3{is_dirty?}
        S3 -->|"yes + !force + !allow_dirty"| SKIP_DIRTY["skip: uncommitted changes\nuse --allow-dirty"]
        S3 -->|no or overridden| S4

        S4{unmerged commits?\nAND NOT BranchDeleted\nAND NOT Merged reason}
        S4 -->|"yes + !force + !allow_unmerged"| SKIP_UNMERGED["skip: unmerged commits\nuse --allow-unmerged"]
        S4 -->|no or overridden| KEEP["→ to_prune list"]
    end

    PHASE2 --> PHASE3

    subgraph PHASE3["Phase 3 — Execution"]
        J{json mode?}
        J -->|yes| JSON_EXEC["execute (unless --dry-run)\nprint JSON: {pruned, skipped, dry_run}"]
        J -->|no| TEXT_MODE

        TEXT_MODE --> SHOW_SKIP["display skipped worktrees\nwith reasons"]
        SHOW_SKIP --> EMPTY2{to_prune\nempty?}
        EMPTY2 -->|yes| MSG_NONE["print: no worktrees to prune"]
        EMPTY2 -->|no| SHOW_LIST["display worktrees to prune\n(name, branch, reason)"]

        SHOW_LIST --> DRY{--dry-run?}
        DRY -->|yes| MSG_DRY["print: dry run — no changes"]
        DRY -->|no| CONFIRM{--yes?}

        CONFIRM -->|no| DIALOG["dialoguer::Confirm\n'Prune N worktree(s)?'"]
        DIALOG -->|confirmed| EXEC
        DIALOG -->|denied| MSG_CANCEL["print: cancelled"]
        CONFIRM -->|yes| EXEC

        EXEC["for each candidate:\n1. fs::remove_dir_all(path)\n2. repo.find_worktree(name)\n3. worktree.prune(valid=true)"]
        EXEC --> MSG_OK["print: pruned N worktree(s)"]
    end

    PHASE3 --> DONE(["Ok(None)"])
```

## PruneReason variants

| Reason | Source | Unmerged check? |
|---|---|---|
| `Explicit` | user-named argument | yes (checked against default branch) |
| `BranchDeleted` | local branch ref missing | skipped (work already handled) |
| `RemoteGone` | `--gone` flag, upstream ref missing | yes |
| `Merged(target)` | `--merged`, `is_merged_into()` returned true | skipped (already verified) |

## Force flag

`--force` is a single override that disables all four safety checks simultaneously: protected, default-branch, dirty, and unmerged. It does not affect JSON mode or dry-run behavior.

## Key files

- `git-workon/src/cmd/prune.rs` — all three phases, `PruneReason`, `PruneCandidate`, `is_upstream_gone()`, `is_protected()`
- `git-workon-lib/src/worktree.rs` — `WorktreeDescriptor::is_dirty()`, `is_merged_into()`, `has_gone_upstream()`
- `git-workon-lib/src/config.rs` — `prune_protected_branches()`, `is_protected()`
