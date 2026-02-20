# workon

Core library for [git-workon](https://crates.io/crates/git-workon): an opinionated git worktree workflow tool.

Published on crates.io as `git-workon-lib` (library name: `workon`).

## Usage

```toml
[dependencies]
git-workon-lib = "0.1"
```

```rust
use workon::{get_repo, get_worktrees, add_worktree, BranchType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = get_repo(None)?;

    // List all worktrees
    for wt in get_worktrees(&repo)? {
        println!("{}: {}", wt.name().unwrap_or("?"), wt.path().display());
    }

    // Create a new worktree
    let wt = add_worktree(&repo, "my-feature", BranchType::Normal, None)?;
    println!("Created worktree at {}", wt.path().display());

    Ok(())
}
```

## API docs

Full API documentation is on [docs.rs](https://docs.rs/git-workon-lib).
