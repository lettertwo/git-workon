# Doctor Command

`doctor` (alias: `check`) inspects the workspace for problems and optionally fixes them. It runs three check categories — worktrees, dependencies, and configuration — and reports results inline as they are discovered.

```mermaid
flowchart TD
    START([git workon doctor]) --> SETUP["get_repo()\nget_worktrees()\nWorkonConfig::new()"]

    subgraph WT_CHECKS["Worktree checks (per worktree)"]
        WC_LOOP["for each worktree"]
        WC_LOOP --> VALIDATE["repo.find_worktree(name).validate()"]
        VALIDATE -->|err + no dir| MISSING["IssueKind::MissingDirectory\n✗ check_fail()\nfixable = true"]
        VALIDATE -->|err + dir exists| BROKEN["IssueKind::BrokenGitLink\n✗ check_fail()\nfixable = false\n(manual: git worktree repair)"]
        VALIDATE -->|ok| GONE_CHECK["has_gone_upstream()?"]
        GONE_CHECK -->|yes| GONE_ISSUE["IssueKind::GoneUpstream\n⚠ check_warn()\nfixable = false\n(suggest: prune --gone)"]
        GONE_CHECK -->|no| PASS["✓ check_pass()"]
    end

    subgraph DEP_CHECKS["Dependency checks (once)"]
        DEP1["gh CLI in PATH?"]
        DEP1 -->|no| GH_ISSUE["IssueKind::GhNotFound\n✗ check_fail()"]
        DEP1 -->|yes| GH_PASS["✓ check_pass('gh')"]

        DEP2["for each postCreateHook:\ncheck first token in PATH"]
        DEP2 -->|missing| HOOK_ISSUE["IssueKind::HookNotFound\n{ hook, command }\n✗ check_fail()"]
        DEP2 -->|found| HOOK_PASS["✓ check_pass('<cmd> (hook)')"]
    end

    subgraph CONFIG_CHECKS["Configuration display (informational)"]
        CONF["read_config_entries():\nall workon.* keys with\nvalues and source file paths"]
        CONF --> CONF_PRINT["✓ check_pass() for each key\n(even defaults are shown)"]
    end

    SETUP --> WT_CHECKS
    WT_CHECKS --> DEP_CHECKS
    DEP_CHECKS --> CONFIG_CHECKS
    CONFIG_CHECKS --> OUTPUT_MODE

    OUTPUT_MODE{json mode?}

    OUTPUT_MODE -->|yes| JSON_OUT["serialize issues + config\nto JSON:\n{ issues, fixed, dry_run,\n  configuration }"]
    JSON_OUT --> JSON_FIX{fix + !dry_run?}
    JSON_FIX -->|yes| FIX_JSON["fix_issues(): prune\nMissingDirectory worktrees"]
    JSON_FIX -->|no| DONE(["Ok(None)"])

    OUTPUT_MODE -->|no| SUMMARY{any issues?}
    SUMMARY -->|no| ALL_OK["print: all checks passed"]
    SUMMARY -->|yes| FIXABLE_COUNT["count fixable issues"]

    FIXABLE_COUNT --> DRY_RUN{--dry-run?}
    DRY_RUN -->|yes| DRY_MSG["print: would fix N issue(s)\nor: no issues can be fixed"]

    DRY_RUN -->|no| FIX_FLAG{--fix?}
    FIX_FLAG -->|yes| DO_FIX["fix_issues():\nfor each MissingDirectory:\n  worktree.prune(valid=true)"]
    DO_FIX --> FIX_REPORT["print: ✓ Pruned: <name>"]
    FIX_FLAG -->|no| SUGGEST["if fixable_count > 0:\nprint: run with --fix to apply"]

    ALL_OK --> DONE
    DRY_MSG --> DONE
    FIX_REPORT --> DONE
    SUGGEST --> DONE
    FIX_JSON --> DONE
```

## Issue types

| Kind | Fixable | Condition | Action |
|---|---|---|---|
| `MissingDirectory` | yes | `validate()` fails + path doesn't exist | `worktree.prune(valid=true)` |
| `BrokenGitLink` | no | `validate()` fails + path exists | user must run `git worktree repair` |
| `GoneUpstream` | no | `validate()` ok + `has_gone_upstream()` | suggest `git workon prune --gone` |
| `HookNotFound` | no | hook command not in PATH | user must install the tool |
| `GhNotFound` | no | `gh` not in PATH | PR features unavailable |

## Configuration display

`read_config_entries()` shows all 8 `workon.*` keys with their current value and the config file that set it (abbreviated as `~/<path>`). Keys that are at default (not set in any file) show `None` as source.

## Key files

- `git-workon/src/cmd/doctor.rs` — `Run` impl, all check logic, `fix_issues()`, `read_config_entries()`
- `git-workon-lib/src/worktree.rs` — `WorktreeDescriptor::has_gone_upstream()`, `validate()` via git2
- `git-workon-lib/src/config.rs` — `WorkonConfig` for reading hook and config data
