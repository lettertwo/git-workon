//! Enhanced file copying with pattern matching and platform optimizations.
//!
//! This module provides pattern-based file copying between worktrees with platform-specific
//! optimizations for efficient copying of large files and directories.
//!
//! ## Design: Two Modes
//!
//! ### 1. Standalone Command (`copy-untracked`)
//! - Default behavior: copies ALL untracked files (`**/*` pattern)
//! - `--pattern` flag: override with specific patterns
//! - Config: uses `workon.copyPattern` if set (convenience)
//! - Priority: `--pattern` > config > default `**/*`
//! - `--force` flag: overwrite existing files at destination
//!
//! ### 2. Automatic Copying (`new` command integration)
//! - Enable with `workon.autoCopyUntracked=true` config
//! - Uses `workon.copyPattern` if configured, otherwise defaults to `**/*`
//! - Always respects `workon.copyExclude` patterns
//! - Flags: `--(no-)copy-untracked` to override config
//! - Copies from base branch's worktree (or HEAD's worktree if no base specified)
//! - Gracefully skips if source worktree doesn't exist
//! - Runs after worktree creation, before post-create hooks
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
//!
//! ## Platform Optimizations
//!
//! Platform-specific copy-on-write optimizations for large files:
//! - **macOS**: `cp -c` (clonefile) - instant CoW copies on APFS
//! - **Linux**: `cp --reflink=auto` - CoW copies on btrfs/XFS when supported
//! - **Other**: Standard `fs::copy` fallback
//!
//! These optimizations make copying large node_modules or build directories nearly instant
//! on supported filesystems.
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
//! # Configure automatic copying
//! git config workon.autoCopyUntracked true
//! git config --add workon.copyPattern '.env.local'
//! git config --add workon.copyPattern 'node_modules/'
//! git config --add workon.copyExclude '.env.production'
//! ```
//!
//! TODO: Add progress reporting for large copies

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CopyError, Result};

/// Copy files from source to destination using glob patterns
///
/// Returns a list of successfully copied file paths
pub fn copy_files(
    from_path: &Path,
    to_path: &Path,
    patterns: &[String],
    excludes: &[String],
    force: bool,
) -> Result<Vec<PathBuf>> {
    let mut copied_files = Vec::new();

    for pattern in patterns {
        // Build full pattern path relative to source
        let pattern_path = from_path.join(pattern);
        let pattern_str = pattern_path
            .to_str()
            .ok_or_else(|| CopyError::InvalidPatternPath {
                path: pattern_path.clone(),
            })?;

        // Find all files matching the pattern
        for entry in glob::glob(pattern_str).map_err(|e| CopyError::InvalidGlobPattern {
            pattern: pattern.clone(),
            source: e,
        })? {
            let src_file = entry.map_err(CopyError::from)?;

            // Skip directories - only copy files
            if src_file.is_dir() {
                continue;
            }

            // Skip if file should be excluded
            if should_exclude(&src_file, from_path, excludes)? {
                continue;
            }

            // Calculate relative path from source
            let rel_path = src_file
                .strip_prefix(from_path)
                .expect("src_file is under from_path")
                .to_path_buf();

            // Build destination path
            let dest_file = to_path.join(&rel_path);

            // Skip if destination exists and force is false
            if dest_file.exists() && !force {
                eprintln!("Skipping (already exists): {}", rel_path.display());
                continue;
            }

            // Create parent directories if needed
            if let Some(parent) = dest_file.parent() {
                fs::create_dir_all(parent)?;
            }

            // Copy the file using platform-specific optimization
            copy_file_platform(&src_file, &dest_file)?;
            copied_files.push(rel_path);
        }
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

/// Copy a file using platform-specific optimizations
///
/// Attempts to use copy-on-write when available, falls back to standard copy
#[cfg(target_os = "macos")]
fn copy_file_platform(src: &Path, dest: &Path) -> Result<()> {
    // Try using cp -c (clonefile) for copy-on-write on macOS
    let result = Command::new("cp")
        .arg("-c")
        .arg(src)
        .arg(dest)
        .status()
        .map_err(|e| CopyError::CopyFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })?;

    if result.success() {
        Ok(())
    } else {
        // Fallback to standard copy
        fs::copy(src, dest).map_err(|e| CopyError::CopyFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn copy_file_platform(src: &Path, dest: &Path) -> Result<()> {
    // Try using cp --reflink=auto for copy-on-write on Linux
    let result = Command::new("cp")
        .arg("--reflink=auto")
        .arg(src)
        .arg(dest)
        .status()
        .map_err(|e| CopyError::CopyFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })?;

    if result.success() {
        Ok(())
    } else {
        // Fallback to standard copy
        fs::copy(src, dest).map_err(|e| CopyError::CopyFailed {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            source: e,
        })?;
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn copy_file_platform(src: &Path, dest: &Path) -> Result<()> {
    // Use standard copy for other platforms
    fs::copy(src, dest).map_err(|e| CopyError::CopyFailed {
        src: src.to_path_buf(),
        dest: dest.to_path_buf(),
        source: e,
    })?;
    Ok(())
}
