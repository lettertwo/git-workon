# Crate Architecture

The workspace contains three crates with a clear dependency hierarchy. The CLI binary depends on the core library; test utilities depend on the library; external dependencies are shared via `[workspace.dependencies]`.

```mermaid
flowchart TD
    CLI["**git-workon** (binary)\ngit-workon/src/"]
    LIB["**git-workon-lib** (library)\npublished as `workon`\ngit-workon-lib/src/"]
    FIX["**git-workon-fixture** (test utils)\ngit-workon-fixture/src/"]

    CLI -->|depends on| LIB
    FIX -->|depends on| LIB

    subgraph CLI_MODULES["git-workon modules"]
        MAIN["main.rs\nsmart routing"]
        CLICLI["cli.rs\nclap structs"]
        CMD["cmd/\nRun trait impls"]
        HOOKS["hooks.rs\npost-create hooks"]
        COPY_CMD["copy.rs\nfile copy CLI"]
        DISPLAY["display.rs\nstatus indicators"]
        OUTPUT["output.rs\nprinting helpers"]
    end

    subgraph LIB_MODULES["git-workon-lib modules"]
        CLONE["clone.rs"]
        INIT["init.rs"]
        CONV["convert_to_bare.rs"]
        WORKTREE["worktree.rs\nWorktreeDescriptor\nBranchType\nadd_worktree()"]
        CONFIG["config.rs\nWorkonConfig"]
        PR["pr.rs\nPullRequest\nPrMetadata"]
        MOVE["move.rs\nmove_worktree()"]
        COPY_LIB["copy.rs\ncopy_files()"]
        ERROR["error.rs\nWorkonError + sub-enums"]
        ROOT["workon_root.rs"]
        GET_REPO["get_repo.rs"]
        DEFAULT_BRANCH["default_branch.rs"]
    end

    subgraph FIX_MODULES["git-workon-fixture modules"]
        BUILDER["fixture_builder.rs\nFixtureBuilder"]
        PREDS["predicates/\ncustom assertions"]
        ASSERT["assert.rs\nFixtureAssert trait"]
    end

    subgraph EXT["External dependencies"]
        GIT2["git2\n(libgit2 bindings)"]
        GIT2CREDS["git2_credentials\n(credential UI)"]
        CLAP["clap\n(CLI parsing)"]
        MIETTE["miette\n(diagnostics)"]
        DIALOGUER["dialoguer\n(interactive prompts)"]
        GHCLI["gh CLI\n(external process)"]
    end

    LIB --> GIT2
    LIB --> GIT2CREDS
    LIB --> MIETTE
    LIB --> GHCLI
    CLI --> CLAP
    CLI --> DIALOGUER
    CLI --> MIETTE

    CLI --- CLI_MODULES
    LIB --- LIB_MODULES
    FIX --- FIX_MODULES
```

## Run trait dispatch pattern

Every command struct implements `Run`, which returns the created/found worktree (if any). `main.rs` prints the path or JSON after dispatch.

```mermaid
flowchart LR
    PARSE["Cli::parse()"] --> ROUTE["smart routing\n(main.rs)"]
    ROUTE --> CMD2["Cmd variant"]
    CMD2 --> RUN["cmd.run()\nRun trait"]
    RUN --> RESULT["Result<Option<WorktreeDescriptor>>"]
    RESULT -->|Some| PRINT["print path\nor JSON"]
    RESULT -->|None| DONE["(command already\nprinted output)"]
```

## Key files

- `Cargo.toml` — workspace definition and shared dependencies
- `git-workon/src/main.rs` — entry point, smart routing, output
- `git-workon/src/cli.rs` — all `clap` argument structs
- `git-workon/src/cmd/` — one file per subcommand, each implements `Run`
- `git-workon-lib/src/lib.rs` — public re-exports of the library
- `git-workon-lib/src/error.rs` — all error types
- `git-workon-fixture/src/` — test infrastructure
