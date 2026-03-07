# 010 — Platform-Specific Copy-on-Write for File Copying

## Context

Copying large directories between worktrees (e.g. `node_modules/`, build artifacts) with `std::fs::copy` performs a full byte-for-byte copy, which is slow on modern filesystems that support copy-on-write (CoW). Both APFS on macOS and btrfs/XFS on Linux support CoW clones that are nearly instant for large directories. We wanted to take advantage of these optimizations without requiring a runtime filesystem detection step.

## Decision

`copy.rs` selects the copy implementation at compile time using `cfg` attributes:

- **macOS**: `cp -c` — uses the `clonefile(2)` syscall on APFS, falls back to a normal copy on HFS+.
- **Linux**: `cp --reflink=auto` — uses CoW on btrfs/XFS when the filesystem supports it, falls back to a normal copy otherwise.
- **Other**: `std::fs::copy` — standard byte-for-byte copy.

The platform-specific path runs `cp` as a subprocess. If it fails, the error is reported through `CopyError`.

## Consequences

- Copying `node_modules/` between worktrees on APFS or btrfs is nearly instant rather than taking seconds.
- The implementation forks a process (`cp`) rather than using pure Rust, which adds process-spawn overhead for small files but is negligible for large directories.
- Reflink support on Linux depends on the filesystem; `--reflink=auto` gracefully falls back, so correctness is not affected.
- Windows is not currently a target; the `std::fs::copy` fallback handles any other platform.

## References

- `git-workon-lib/src/copy.rs` — platform-specific copy implementation (module doc)
- [ADR-011](011-file-copy-two-mode-design.md) — two-mode design for when copying is triggered
