# Implementation Principles

## Code Quality Standards - With Examples

**✅ DO: Implement only what's requested**

```rust
// Task: add a --dry-run flag to prune command
// GOOD: Just add the flag and skip deletions
if args.dry_run {
    println!("Would delete: {}", path);
    continue;
}

// BAD: Don't also add --verbose, --confirm, logging framework, etc.
```

**❌ DON'T: Add features not requested**

```rust
// Task: fix the branch name validation
// BAD: Don't refactor surrounding code, add new validation types, or create helper modules
// GOOD: Fix only the specific validation issue
```

**✅ DO: Wait for patterns before abstracting (3+ occurrences)**

```rust
// Seeing this pattern twice? Don't abstract yet.
// Seeing it three times? Now consider a helper.
```

**❌ DON'T: Create utilities for one-time operations**

```rust
// BAD: Creating helpers/utils.rs for a single use case
fn parse_branch_name(s: &str) -> String { /* ... */ }  // Only used once!

// GOOD: Just inline it where needed
let branch = args.name.trim().to_string();
```

**✅ DO: Only validate at system boundaries**

```rust
// GOOD: Validate user input
pub fn run(&self, repo: &Repository) -> Result<Option<WorktreeDescriptor>> {
    if self.name.is_empty() {
        return Err("Branch name cannot be empty".into());
    }
    // ... rest of implementation
}

// BAD: Don't validate internal function parameters that come from trusted code
fn create_worktree_internal(repo: &Repository, name: &str) -> Result<()> {
    if name.is_empty() { /* ... */ }  // Unnecessary! Called from validated code
}
```

**✅ DO: Delete unused code completely**

```rust
// GOOD: Just delete it
// (nothing here)

// BAD: Don't leave commented code, renamed unused vars, or tombstone comments
// fn old_implementation() { ... }
// let _unused_var = compute_thing();
// // Removed: the feature we used to have
```

## Error Handling Rules

- Only validate at system boundaries (user input, external APIs, file I/O)
- Trust internal code and framework guarantees
- Don't add error handling for scenarios that can't happen
- Don't use feature flags or backwards-compatibility shims unnecessarily
- Follow crate-specific error handling patterns (see `docs/adr/008-error-handling-guide.md`)

## Code Removal Rules

- If something is unused, delete it completely
- No backwards-compatibility hacks like renaming `_vars`, re-exporting types, or `// removed` comments
- No "just in case" code that isn't currently used

## Focused Changes

- A bug fix doesn't need surrounding code cleanup
- A simple feature doesn't need extra configurability
- Only add comments where logic isn't self-evident
- Don't add docstrings, comments, or type annotations to unchanged code
