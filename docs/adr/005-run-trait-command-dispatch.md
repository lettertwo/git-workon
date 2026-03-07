# 005 — `Run` Trait for Command Dispatch

## Context

The CLI has many subcommands (`clone`, `init`, `new`, `find`, `list`, `prune`, `move`, `doctor`, `copy-untracked`). Each needs to be called uniformly from `main.rs` after argument parsing, and each may optionally produce a worktree that `main.rs` needs to print. We needed a dispatch mechanism that is simple to add new commands to and keeps output logic centralized.

## Decision

Each command module in `git-workon/src/cmd/` defines a struct (matching the clap argument struct) and implements the `Run` trait:

```rust
trait Run {
    fn run(&self) -> miette::Result<Option<WorktreeDescriptor>>;
}
```

`main.rs` calls `cmd.run()` on the selected `Cmd` enum variant. If `run()` returns `Ok(Some(worktree))`, `main.rs` prints either the worktree path (text mode) or a JSON object (JSON mode). If it returns `Ok(None)`, the command already printed its own output (e.g. `list`, `prune`).

Command structs in `cli.rs` use clap derive macros. After parsing, `main.rs` may mutate fields on the selected command (e.g. setting `json = true`) before calling `run()`.

## Consequences

- Adding a new command requires: adding a struct to `cli.rs`, adding a variant to `Cmd`, creating `cmd/<name>.rs`, and implementing `Run`. No changes to dispatch logic.
- Output is uniform: path or JSON, controlled by `main.rs` alone.
- Commands that handle their own output return `None`; this is a convention that must be documented and followed.
- The `run()` signature does not take `&mut self`, so argument mutation must happen in `main.rs` before dispatch.

## References

- `docs/diagrams/architecture.md` — Run trait dispatch diagram
- `docs/diagrams/command-dispatch.md` — full dispatch flowchart
- `git-workon/src/main.rs`, `git-workon/src/cli.rs`, `git-workon/src/cmd/`
