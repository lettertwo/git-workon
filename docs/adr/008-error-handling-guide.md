# Error Handling with Miette — Prescriptive Guide

> For the rationale behind these patterns see [ADR-008](008-error-handling-strategy.md).

## git-workon-lib (Core Library)

**Pattern: Define concrete error enums using `thiserror` + `miette`**

The library defines specific, meaningful error types that represent actual failure modes:

```rust
use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Diagnostic, Debug)]
pub enum WorktreeError {
    #[error(transparent)]
    #[diagnostic(code(workon::git_error))]
    GitError(#[from] git2::Error),

    #[error("Worktree '{name}' already exists at {path}")]
    #[diagnostic(
        code(workon::worktree_exists),
        help("Use a different name or remove the existing worktree first")
    )]
    WorktreeExists {
        name: String,
        path: String
    },

    #[error("Branch '{0}' is protected and cannot be pruned")]
    #[diagnostic(
        code(workon::protected_branch),
        help("Protected branches are configured in workon.pruneProtectedBranches")
    )]
    ProtectedBranch(String),
}
```

**✅ DO for Library Errors:**

- Use `#[derive(Error, Diagnostic, Debug)]` on error enums
- Add diagnostic codes following namespace pattern: `workon::<error_name>`
- Add helpful context in error messages (include relevant names, paths, etc.)
- Use `#[help]` attribute to guide users toward solutions
- Use `#[error(transparent)]` with `#[from]` for wrapping external errors (git2, io, etc.)
- Define error variants for actual failure modes, not generic categories

**❌ DON'T for Library Errors:**

- Don't create generic wrapper types like `LibraryError(Box<dyn Error>)`
- Don't use `.into_diagnostic()` in library code (that's for applications)
- Don't add source code snippets (`#[source_code]`, `#[label]`) unless parsing/validation errors
- Don't use `Report<T>` in library return types
- Don't add errors for conditions that can't happen (validated elsewhere)

**When to add source code snippets:**

Only for parsing or validation errors where showing the problematic input helps:

```rust
#[derive(Error, Diagnostic, Debug)]
#[error("Invalid branch name")]
#[diagnostic(code(workon::invalid_branch_name))]
pub struct InvalidBranchName {
    #[source_code]
    src: NamedSource<String>,
    #[label("Invalid characters here")]
    span: SourceSpan,
}
```

## git-workon (CLI Binary)

**Pattern: Convert external errors and add context using `.into_diagnostic()` and `.wrap_err()`**

The CLI should:

1. Propagate library errors directly (they already implement `Diagnostic`)
2. Convert external errors (from other libraries) using `.into_diagnostic()`
3. Add user-facing context using `.wrap_err()`

```rust
use miette::{IntoDiagnostic, Result, WrapErr};

// In main() - enables automatic pretty-printing
fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.run()
}

// In command implementations
pub fn run(&self) -> Result<Option<WorktreeDescriptor>> {
    // Library errors propagate automatically (already implement Diagnostic)
    let worktree = add_worktree(&repo, &self.name, self.branch_type)?;

    // Convert external errors and add context
    let config = fs::read_to_string(&self.config_path)
        .into_diagnostic()
        .wrap_err("Failed to read configuration file")?;

    // Parse operations that might fail
    let url: Url = self.url.parse()
        .into_diagnostic()
        .wrap_err("Invalid repository URL")?;

    Ok(Some(worktree))
}
```

**✅ DO for CLI Errors:**

- Return `miette::Result<T>` from `main()` and command implementations
- Use `.into_diagnostic()` when converting errors from external libraries
- Use `.wrap_err()` to add user-facing context about what operation failed
- Let library errors (WorktreeError, ConfigError, etc.) propagate unchanged
- Add context that helps users understand what they were trying to do

**❌ DON'T for CLI Errors:**

- Don't define new error types in the CLI (use library types)
- Don't re-wrap library errors that already have good messages
- Don't add context that just repeats the underlying error
- Don't use `.map_err()` when `.wrap_err()` is more appropriate

**Context Guidelines:**

```rust
// GOOD: Adds useful context about the operation
fs::remove_dir_all(&path)
    .into_diagnostic()
    .wrap_err(format!("Failed to prune worktree at {}", path.display()))?;

// BAD: Context doesn't add information
repo.find_branch(&name, BranchType::Local)
    .into_diagnostic()
    .wrap_err("Branch operation failed")?;  // Too vague!

// GOOD: Let library error speak for itself (it already says "Branch 'foo' not found")
let branch = find_branch(&repo, &name)?;  // No .wrap_err() needed
```

## git-workon-fixture (Test Utilities)

**Pattern: Simple error types focused on clear predicate failures**

The test fixture crate doesn't need complex error handling:

```rust
// Simple error types are fine
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// OR simple custom errors when needed
#[derive(Debug)]
pub struct FixtureError(String);

impl std::fmt::Display for FixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FixtureError {}
```

**✅ DO for Test Fixture Errors:**

- Use `Box<dyn std::error::Error>` for test utilities
- Focus on clear predicate failure messages (in `PredicateReflection`)
- Keep error types simple—tests should focus on setup/assertions, not error handling

**❌ DON'T for Test Fixture Errors:**

- Don't use miette's diagnostic features in test utilities
- Don't create complex error hierarchies
- Don't add diagnostic codes to test fixture errors

**Focus on Predicate Messages:**

The real "error reporting" in tests comes from predicate failure messages:

```rust
impl PredicateReflection for HasBranchPredicate {
    fn parameters<'a>(&'a self) -> Box<dyn Iterator<Item = Parameter<'a>> + 'a> {
        let params = vec![Parameter::new("expected_branch", &self.name)];
        Box::new(params.into_iter())
    }
}

// Clear failure output:
// "expected repository to have branch 'feature' but it was not found"
```

## Common Error Patterns

**Pattern: Config parsing errors**

```rust
// Library (git-workon-lib/src/config.rs)
#[derive(Error, Diagnostic, Debug)]
pub enum ConfigError {
    #[error("Invalid value for {key}: {value}")]
    #[diagnostic(
        code(workon::invalid_config_value),
        help("Expected {expected}, got {value}")
    )]
    InvalidValue {
        key: String,
        value: String,
        expected: String,
    },
}

// CLI usage - just propagate
let config = WorkonConfig::load(&repo)?;  // ConfigError bubbles up with nice diagnostics
```

**Pattern: File operations**

```rust
// CLI (git-workon/src/cmd/copy.rs)
fs::write(&dest, &contents)
    .into_diagnostic()
    .wrap_err(format!("Failed to copy {} to {}", src.display(), dest.display()))?;
```

**Pattern: Git operations**

```rust
// Library wraps git2 errors
#[derive(Error, Diagnostic, Debug)]
pub enum WorktreeError {
    #[error(transparent)]
    #[diagnostic(code(workon::git_error))]
    GitError(#[from] git2::Error),
}

// CLI just propagates
let worktree = add_worktree(&repo, &name, BranchType::Normal)?;  // Works automatically
```

## Diagram

See `docs/diagrams/error-hierarchy.md` for the error type hierarchy across crates.
