# Error Types

All errors in `git-workon-lib` implement both `std::error::Error` (via `thiserror`) and `miette::Diagnostic` for rich terminal output. The CLI propagates library errors unchanged and converts external library errors with `.into_diagnostic()`.

```mermaid
classDiagram
    class WorkonError {
        +Git(git2.Error)
        +Io(io.Error)
        +Repo(RepoError)
        +Worktree(WorktreeError)
        +Config(ConfigError)
        +DefaultBranch(DefaultBranchError)
        +Pr(PrError)
        +Copy(CopyError)
    }

    class RepoError {
        +NotBare(String)
    }

    class WorktreeError {
        +InvalidGitFile
        +NotFound(String)
        +NotInWorktree
        +NoBranchTarget
        +NoCurrentBranchTarget
        +NoLocalBranchTarget
        +NoParent
        +InvalidName
        +NonEmptyIndex
        +TargetExists(to)
        +CannotMoveDetached
        +ProtectedBranchMove(String)
        +DirtyWorktree
        +UnpushedCommits
    }

    class ConfigError {
        +InvalidPrFormat(format, reason)
        +NoValue
    }

    class DefaultBranchError {
        +NoRemoteDefault(remote)
        +NotConnected
        +NoDefaultBranch
    }

    class PrError {
        +InvalidReference(input)
        +PrNotFound(number, remote)
        +NoRemoteConfigured
        +FetchFailed(remote, message)
        +GhNotInstalled
        +GhFetchFailed(message)
        +GhJsonParseFailed(message)
        +MissingForkOwner
    }

    class CopyError {
        +InvalidPatternPath(path)
        +InvalidGlobPattern(pattern, source)
        +InvalidPath(path)
        +GlobEntry(glob.GlobError)
        +CopyFailed(src, dest, source)
    }

    WorkonError --> RepoError : from
    WorkonError --> WorktreeError : from
    WorkonError --> ConfigError : from
    WorkonError --> DefaultBranchError : from
    WorkonError --> PrError : from
    WorkonError --> CopyError : from
```

## Diagnostic codes

Each variant carries a `#[diagnostic(code(workon::...))]` attribute. Code namespaces follow the module structure:

| Namespace | Error type |
|---|---|
| `workon::git_error` | `WorkonError::Git` |
| `workon::io_error` | `WorkonError::Io` |
| `workon::repo::*` | `RepoError` variants |
| `workon::worktree::*` | `WorktreeError` variants |
| `workon::config::*` | `ConfigError` variants |
| `workon::default_branch::*` | `DefaultBranchError` variants |
| `workon::pr::*` | `PrError` variants |
| `workon::copy::*` | `CopyError` variants |

## Conversion flow

```mermaid
flowchart LR
    GIT2["git2::Error"] -->|"#[from]"| WO_GIT["WorkonError::Git"]
    IO["std::io::Error"] -->|"#[from]"| WO_IO["WorkonError::Io"]
    RE["RepoError"] -->|"#[from]"| WO_REPO["WorkonError::Repo"]
    WTE["WorktreeError"] -->|"#[from]"| WO_WT["WorkonError::Worktree"]
    CE["ConfigError"] -->|"#[from]"| WO_CFG["WorkonError::Config"]
    DBE["DefaultBranchError"] -->|"#[from]"| WO_DB["WorkonError::DefaultBranch"]
    PRE["PrError"] -->|"#[from]"| WO_PR["WorkonError::Pr"]
    CPE["CopyError"] -->|"#[from]"| WO_CP["WorkonError::Copy"]

    WO_GIT --> CLI["CLI: miette::Result\nfancy diagnostic output"]
    WO_IO --> CLI
    WO_REPO --> CLI
    WO_WT --> CLI
    WO_CFG --> CLI
    WO_DB --> CLI
    WO_PR --> CLI
    WO_CP --> CLI

    EXT["External libs\n(serde_json, dialoguer, etc)"] -->|".into_diagnostic()"| CLI
```

## CLI error handling rules

- Library errors (`WorkonError` and sub-enums) propagate unchanged — they already implement `Diagnostic`
- External library errors use `.into_diagnostic()` to convert to `miette::Report`
- `.wrap_err()` adds user-facing context for operations that might fail (file I/O, URL parsing)
- Library code never uses `.into_diagnostic()` — it defines concrete error types instead

## Key files

- `git-workon-lib/src/error.rs` — `WorkonError` and all sub-enums
- Domain-specific variants are defined in the same file but grouped by sub-enum
