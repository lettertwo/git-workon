//! Enhanced file copying with pattern matching and platform optimizations.
//!
//! This module provides pattern-based file copying between worktrees with platform-specific
//! optimizations for efficient copying of large files and directories.
//!
//! ## Design
//!
//! - Uses `git status` to enumerate candidate files (untracked, not ignored by default).
//! - Patterns filter the candidate list.
//! - Opt-in ignored file support: `--include-ignored` / `workon.copyIncludeIgnored=true`.
//!
//! ## Pattern Matching
//!
//! Uses standard glob patterns via the `glob` crate:
//! - `*.env` - All .env files in current directory
//! - `.env*` - All files starting with .env
//! - `**/*.json` - All JSON files recursively
//! - `.vscode/` - Entire directory and contents
//!
//! Exclude patterns work the same way, checked after include patterns match.
//! An empty include pattern list means "match all candidates".
//!
//! ## Platform Optimizations
//!
//! Platform-specific copy-on-write optimizations for large files:
//! - **macOS**: `clonefile(2)` syscall — instant CoW copies on APFS
//! - **Linux**: `ioctl(FICLONE)` — CoW copies on btrfs/XFS when supported
//! - **Other**: Standard `fs::copy` fallback
//!
//! ## Behavior
//!
//! - Only copies files (directories are skipped, but created as needed for nested files)
//! - Automatic parent directory creation for nested files
//! - Skips files that already exist at destination (unless --force)
//! - Returns list of successfully copied files
//!
//! ## Example Usage
//!
//! ```bash
//! # Copy specific patterns
//! git workon copy-untracked --pattern '.env*' --pattern '.vscode/'
//!
//! # Configure automatic copying with ignored files
//! git config workon.autoCopyUntracked true
//! git config workon.copyIncludeIgnored true
//! git config --add workon.copyPattern '.env.local'
//! git config --add workon.copyPattern 'node_modules/'
//! git config --add workon.copyExclude '.env.production'
//! ```
//!
//! TODO: Add progress reporting for large copies

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{CopyError, Result};

/// Copy only untracked (and optionally ignored) files from source to destination.
///
/// Uses `git status` to enumerate candidate files, then filters by patterns.
/// This is much faster than glob-walking when `patterns` is broad (e.g., `**/*`),
/// and avoids spurious "already exists" messages for tracked files.
///
/// `patterns` filter the git-status candidates. An empty slice matches all candidates.
/// `include_ignored` also includes git-ignored files (e.g., `.env.local`, `node_modules/`).
pub fn copy_untracked(
    from_path: &Path,
    to_path: &Path,
    patterns: &[String],
    excludes: &[String],
    force: bool,
    include_ignored: bool,
) -> Result<Vec<PathBuf>> {
    let repo = git2::Repository::open(from_path).map_err(|source| CopyError::GitStatus {
        path: from_path.to_path_buf(),
        source,
    })?;

    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    if include_ignored {
        opts.include_ignored(true).recurse_ignored_dirs(true);
    }

    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(|source| CopyError::GitStatus {
            path: from_path.to_path_buf(),
            source,
        })?;

    // Compile include patterns once. Empty list = match all.
    let include_patterns: Vec<glob::Pattern> = patterns
        .iter()
        .map(|p| {
            glob::Pattern::new(p).map_err(|e| CopyError::InvalidGlobPattern {
                pattern: p.clone(),
                source: e,
            })
        })
        .collect::<std::result::Result<Vec<_>, CopyError>>()?;

    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    let mut copied_files = Vec::new();

    for entry in statuses.iter() {
        let status = entry.status();

        // Only copy untracked (WT_NEW) or, if opted in, ignored files
        let is_candidate = status.contains(git2::Status::WT_NEW)
            || (include_ignored && status.contains(git2::Status::IGNORED));

        if !is_candidate {
            continue;
        }

        let rel_path = match entry.path() {
            Some(p) => PathBuf::from(p),
            None => continue,
        };

        let rel_path_str = match rel_path.to_str() {
            Some(s) => s,
            None => continue,
        };

        // Apply include patterns (empty = match all)
        if !include_patterns.is_empty()
            && !include_patterns
                .iter()
                .any(|p| p.matches_with(rel_path_str, match_opts))
        {
            continue;
        }

        // Apply exclude patterns
        if should_exclude(&from_path.join(&rel_path), from_path, excludes)? {
            continue;
        }

        let src_file = from_path.join(&rel_path);
        let dest_file = to_path.join(&rel_path);

        // Skip directories (git status shouldn't return dirs with recurse, but be safe)
        if src_file.is_dir() {
            continue;
        }

        // Skip if destination exists and not forcing
        if dest_file.exists() && !force {
            continue;
        }

        // Create parent directories if needed
        if let Some(parent) = dest_file.parent() {
            fs::create_dir_all(parent)?;
        }

        copy_file_platform(&src_file, &dest_file)?;
        copied_files.push(rel_path);
    }

    Ok(copied_files)
}

/// Check if a file should be excluded based on exclusion patterns
fn should_exclude(path: &Path, base: &Path, excludes: &[String]) -> Result<bool> {
    // Get relative path from base
    let rel_path = match path.strip_prefix(base) {
        Ok(p) => p,
        Err(_) => return Ok(false), // If not under base, don't exclude
    };

    let rel_path_str = rel_path.to_str().ok_or_else(|| CopyError::InvalidPath {
        path: rel_path.to_path_buf(),
    })?;

    // Check against each exclusion pattern
    for exclude_pattern in excludes {
        // Simple glob pattern matching
        if glob::Pattern::new(exclude_pattern)
            .map_err(|e| CopyError::InvalidGlobPattern {
                pattern: exclude_pattern.clone(),
                source: e,
            })?
            .matches(rel_path_str)
        {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Copy a file using platform-specific copy-on-write when available.
///
/// Uses direct syscalls to avoid per-file subprocess overhead:
/// - macOS: `clonefile(2)` for instant CoW on APFS; falls back to `fs::copy`
/// - Linux: `ioctl(FICLONE)` for CoW on btrfs/XFS; falls back to `fs::copy`
/// - Other: `fs::copy`
#[cfg(target_os = "macos")]
fn copy_file_platform(src: &Path, dest: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let src_c = CString::new(src.as_os_str().as_bytes()).map_err(|_| CopyError::CopyFailed {
        src: src.to_path_buf(),
        dest: dest.to_path_buf(),
        source: std::io::Error::from(std::io::ErrorKind::InvalidInput),
    })?;
    let dest_c = CString::new(dest.as_os_str().as_bytes()).map_err(|_| CopyError::CopyFailed {
        src: src.to_path_buf(),
        dest: dest.to_path_buf(),
        source: std::io::Error::from(std::io::ErrorKind::InvalidInput),
    })?;

    // clonefile(2): instant CoW copy on APFS; fails on non-APFS or cross-device
    if unsafe { libc::clonefile(src_c.as_ptr(), dest_c.as_ptr(), 0) } == 0 {
        return Ok(());
    }

    // Fall back to standard copy (non-APFS, cross-filesystem, etc.)
    fs::copy(src, dest)
        .map(|_| ())
        .map_err(|e| CopyError::CopyFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })
        .map_err(Into::into)
}

#[cfg(target_os = "linux")]
fn copy_file_platform(src: &Path, dest: &Path) -> Result<()> {
    use std::fs::{File, OpenOptions};
    use std::os::unix::io::AsRawFd;

    // FICLONE ioctl: _IOW(0x94, 9, int) = 0x40049409
    // Performs a reflink copy on btrfs/XFS; fails on unsupported filesystems
    const FICLONE: libc::c_ulong = 0x40049409;

    if let (Ok(src_file), Ok(dest_file)) = (
        File::open(src),
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(dest),
    ) {
        if unsafe { libc::ioctl(dest_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) } == 0 {
            return Ok(());
        }
        // ioctl failed — dest file is open but may be empty, drop before overwriting
        drop(dest_file);
    }

    // Fall back to standard copy (non-btrfs/XFS, cross-filesystem, etc.)
    fs::copy(src, dest)
        .map(|_| ())
        .map_err(|e| CopyError::CopyFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })
        .map_err(Into::into)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn copy_file_platform(src: &Path, dest: &Path) -> Result<()> {
    fs::copy(src, dest)
        .map(|_| ())
        .map_err(|e| CopyError::CopyFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })
        .map_err(Into::into)
}
