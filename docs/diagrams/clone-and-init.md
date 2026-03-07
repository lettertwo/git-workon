# Clone & Init Flows

Both `clone` and `init` produce the same end state: a bare repository at `<path>/.bare` with a `.git` link file, ready for worktrees. They converge at `convert_to_bare`.

```mermaid
sequenceDiagram
    participant User
    participant CloneCmd as cmd/clone.rs
    participant InitCmd as cmd/init.rs
    participant CloneLib as clone.rs
    participant InitLib as init.rs
    participant ConvLib as convert_to_bare.rs
    participant Git2 as git2 / libgit2
    participant NewCmd as cmd/new.rs

    rect rgba(220, 235, 255, 0.25)
        Note over User,Git2: Clone path
        User->>CloneCmd: git workon clone <url> [path]
        CloneCmd->>CloneLib: clone(path, url)
        CloneLib->>CloneLib: derive path (append .bare if needed)
        CloneLib->>Git2: FetchOptions with credential callbacks
        CloneLib->>Git2: RepoBuilder.bare(true).remote_create(callback)
        Note over Git2: remote_create callback:<br/>1. create remote<br/>2. get_default_branch_name()<br/>3. add fetch refspec for default branch<br/>4. return remote
        Git2-->>CloneLib: bare Repository at path/.bare
        CloneLib->>ConvLib: convert_to_bare(repo)
    end

    rect rgba(220, 255, 220, 0.25)
        Note over User,Git2: Init path
        User->>InitCmd: git workon init [path]
        InitCmd->>InitLib: init(path)
        InitLib->>Git2: Repository::init(path)
        Git2-->>InitLib: non-bare Repository
        InitLib->>Git2: empty_commit(repo)
        Note over Git2: creates initial commit<br/>so HEAD is valid
        InitLib->>ConvLib: convert_to_bare(repo)
    end

    rect rgba(255, 245, 220, 0.25)
        Note over ConvLib,Git2: convert_to_bare (shared)
        ConvLib->>Git2: config.set_bool("core.bare", true)
        ConvLib->>ConvLib: rename .git → <root>/.bare
        ConvLib->>ConvLib: write .git file: "gitdir: ./.bare"
        ConvLib->>Git2: Repository::open(<root>/.bare)
        ConvLib->>Git2: remote_add_fetch("origin", "+refs/heads/*:refs/remotes/origin/*")
        ConvLib-->>CloneLib: reopened bare Repository
        ConvLib-->>InitLib: reopened bare Repository
    end

    rect rgba(240, 220, 255, 0.25)
        Note over CloneCmd,NewCmd: Post-conversion (both paths)
        CloneCmd->>NewCmd: add_worktree(repo, default_branch, Normal, None)
        InitCmd->>NewCmd: add_worktree(repo, default_branch, Normal, None)
        Note over NewCmd: creates initial worktree<br/>(see 04-new-worktree.md)
        NewCmd-->>CloneCmd: WorktreeDescriptor
        NewCmd-->>InitCmd: WorktreeDescriptor
        CloneCmd->>CloneCmd: execute_post_create_hooks (unless --no-hooks)
        InitCmd->>InitCmd: execute_post_create_hooks (unless --no-hooks)
    end
```

## Directory structure after clone/init

```
my-project/          ← workon root (workon_root())
├── .bare/           ← actual git repo (core.bare=true)
│   ├── config
│   ├── HEAD
│   ├── refs/
│   ├── worktrees/
│   └── ...
├── .git             ← link file: "gitdir: ./.bare"
└── main/            ← initial worktree (branch: main)
    ├── .git         ← link: "gitdir: ../.bare/worktrees/main"
    └── ... (working files)
```

## workon_root() discovery

`workon_root()` (`git-workon-lib/src/workon_root.rs`) finds the common ancestor between the `.git` directory and the current working directory. This is what makes the sibling-worktrees layout work correctly from any directory.

## Key files

- `git-workon-lib/src/clone.rs` — bare clone with credential callbacks
- `git-workon-lib/src/init.rs` — init + empty commit + convert
- `git-workon-lib/src/convert_to_bare.rs` — shared conversion logic
- `git-workon-lib/src/workon_root.rs` — root directory discovery
- `git-workon-lib/src/default_branch.rs` — remote default branch detection
- `git-workon/src/cmd/clone.rs` — CLI wrapper, hooks
- `git-workon/src/cmd/init.rs` — CLI wrapper, hooks
