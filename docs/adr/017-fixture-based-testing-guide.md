# Testing Philosophy and Standards

**Core Principle**: Use the custom testing infrastructure (`git-workon-fixture`) consistently. Extend the fixture library instead of writing ad-hoc test code.

## Testing Infrastructure

The `git-workon-fixture` crate provides three key components:

1. **FixtureBuilder** - Creates temporary git repositories for tests
2. **Custom Predicates** - Declarative assertions for git repository state
3. **FixtureAssert trait** - Chainable `.assert()` method on `Repository`

## Mandatory Testing Patterns

**✅ ALWAYS Use FixtureBuilder**

```rust
use git_workon_fixture::prelude::*;

// Create a bare repo with a worktree
let fixture = FixtureBuilder::new()
    .bare(true)
    .default_branch("main")
    .worktree("main")
    .build()?;

let repo = fixture.repo()?;
```

**❌ NEVER Create Repos Manually**

```rust
// DON'T DO THIS
let temp_dir = TempDir::new()?;
Repository::init_bare(temp_dir.path())?;  // Too ad-hoc!
```

**✅ ALWAYS Use Custom Predicates**

```rust
// Chainable assertions with clear failure messages
repo.assert(predicate::repo::has_branch("main"));
repo.assert(predicate::repo::has_worktree("feature"));
repo.assert(predicate::repo::is_bare());
```

**❌ NEVER Use Manual Assertions**

```rust
// DON'T DO THIS
assert!(repo.find_branch("main", BranchType::Local).is_ok());  // Unclear failures!
let worktrees = repo.worktrees()?;
assert!(worktrees.iter().any(|w| w == "feature"));  // Verbose and unclear!
```

## Available Predicates

Located in `git-workon-fixture/src/predicates/`:

- `is_bare()` - Repository is bare
- `is_empty()` - Repository has no commits
- `is_worktree()` - Path is a worktree (not main repo)
- `has_branch(name)` - Branch exists
- `has_worktree(name)` - Worktree exists
- `has_remote(name)` - Remote exists
- `has_remote_url(name, url)` - Remote has specific URL
- `has_remote_branch(remote, branch)` - Remote tracking branch exists
- `has_upstream(branch)` - Branch has upstream configured
- `has_config(key, value)` - Config entry exists with value
- `head_matches(refname)` - HEAD points to specific ref
- `head_commit_message_contains(text)` - HEAD commit message contains text
- `head_commit_parent_count(n)` - HEAD commit has n parents
- `branch_points_to(branch, commit)` - Branch points to commit OID

## When to Extend the Fixture Library

**If you find yourself writing complex test setup or assertions, STOP and extend the fixture library instead.**

**Extend FixtureBuilder when:**

- Creating the same repo configuration repeatedly across tests
- Using complex setup with multiple git commands
- Tests have duplicated initialization logic

**Example**: If multiple tests need a repo with remotes:

1. Add `.remote(name, url)` to FixtureBuilder
2. Update all tests to use the new builder method

**Extend Predicates when:**

- Writing complex assertions with multiple git2 API calls
- Repeating the same assertion pattern across tests
- Assertions have unclear failure messages

**Example**: If tests check "branch is merged into main":

1. Create `git-workon-fixture/src/predicates/is_merged_into.rs`
2. Implement predicate with clear failure messages
3. Export from `prelude.rs`
4. Use in tests: `repo.assert(predicate::repo::is_merged_into("feature", "main"))`

## Testing Workflow

**Before Writing a Test:**

1. Check existing predicates in `git-workon-fixture/src/predicates/`
2. Check FixtureBuilder capabilities in `git-workon-fixture/src/fixture_builder.rs`
3. If needed functionality exists, use it
4. If not, extend the fixture library FIRST, then write the test

**When Reviewing Tests:**

1. Ensure FixtureBuilder is used for all repo creation
2. Ensure custom predicates are used for all assertions
3. Check for repeated patterns that should be abstracted
4. Verify failure messages would be helpful for debugging

## Example: Good Test Structure

```rust
#[test]
fn test_add_worktree_orphan() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create fixture with clear, declarative configuration
    let fixture = FixtureBuilder::new()
        .bare(true)
        .default_branch("main")
        .build()?;

    let repo = fixture.repo()?;

    // 2. Execute the function under test
    let worktree = add_worktree(repo, "docs", BranchType::Orphan)?;

    // 3. Assert with custom predicates (chainable, clear failures)
    repo.assert(predicate::repo::has_worktree("docs"));
    repo.assert(predicate::repo::has_branch("docs"));

    // 4. Additional checks only when predicates don't exist yet
    // (Consider adding a custom predicate if this pattern repeats)
    let orphan_repo = Repository::open(worktree.path())?;
    let head = orphan_repo.head()?;
    assert_eq!(head.name(), Some("refs/heads/docs"));

    Ok(())
}
```

## Fixture Library File Locations

- **Builder**: `git-workon-fixture/src/fixture_builder.rs`
- **Predicates**: `git-workon-fixture/src/predicates/*.rs`
- **Prelude**: `git-workon-fixture/src/prelude.rs` (exports everything)
- **Assert trait**: `git-workon-fixture/src/assert.rs`
