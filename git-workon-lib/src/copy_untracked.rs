//! Enhanced file copying with pattern matching and platform optimizations.
//!
//! This module provides pattern-based file copying between worktrees with platform-specific
//! optimizations for efficient copying of large files and directories.
//!
//! ## Design
//!
//! - Uses `ignore::WalkBuilder` + git index check to enumerate candidate files.
//! - The walker respects `.gitignore` by default (never enters `node_modules/`, `target/`, etc.).
//! - With `include_ignored`, gitignore filtering is disabled so ignored files are visited too.
//! - The git index is checked per file (O(1) binary search) to skip tracked files.
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

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{CopyError, Result};

type SkipCallback = Box<dyn FnMut(&'static str, &Path)>;

/// Options for [`copy_untracked`].
///
/// Callbacks default to no-ops; override them to observe progress.
pub struct CopyOptions<'a> {
    /// Glob patterns to include; empty means match all candidates.
    pub patterns: &'a [String],
    /// Glob patterns to exclude after include matching.
    pub excludes: &'a [String],
    /// Overwrite files that already exist at the destination.
    pub force: bool,
    /// Also copy git-ignored files (e.g., `node_modules/`, `.env.local`).
    pub include_ignored: bool,
    /// Called after each file is successfully copied.
    pub on_copied: Box<dyn FnMut(&Path)>,
    /// Called when a file is skipped, with a reason of `"tracked"` or `"exists"`.
    pub on_skipped: SkipCallback,
}

impl Default for CopyOptions<'_> {
    fn default() -> Self {
        Self {
            patterns: &[],
            excludes: &[],
            force: false,
            include_ignored: false,
            on_copied: Box::new(|_| {}),
            on_skipped: Box::new(|_, _| {}),
        }
    }
}

/// Copy only untracked (and optionally ignored) files from source to destination.
///
/// Uses `ignore::WalkBuilder` to walk `from_path`, skipping gitignored paths by default
/// (so `node_modules/`, `target/`, etc. are never entered). With `include_ignored`, gitignore
/// filtering is disabled and all files are visited. In both cases, tracked files are filtered
/// out via an O(1) git index lookup.
pub fn copy_untracked(
    from_path: &Path,
    to_path: &Path,
    options: CopyOptions<'_>,
) -> Result<Vec<PathBuf>> {
    let CopyOptions {
        patterns,
        excludes,
        force,
        include_ignored,
        mut on_copied,
        mut on_skipped,
    } = options;

    let repo = git2::Repository::open(from_path).map_err(|source| CopyError::RepoOpen {
        path: from_path.to_path_buf(),
        source,
    })?;

    // Build a set of tracked paths for O(1) per-file lookup.
    // Using a HashSet instead of index.get_path() per file avoids a libgit2 quirk:
    // git_index_get_bypath sets the error buffer even when returning NULL (path not
    // found), which poisons the next try_call! error message with a stale value.
    let mut index = repo.index().map_err(|source| CopyError::RepoOpen {
        path: from_path.to_path_buf(),
        source,
    })?;
    index.read(false).map_err(|source| CopyError::RepoOpen {
        path: from_path.to_path_buf(),
        source,
    })?;
    let tracked: std::collections::HashSet<Vec<u8>> = index.iter().map(|e| e.path).collect();

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

    // Compile exclude patterns once (previously compiled per-file — now O(1) per check).
    let exclude_patterns: Vec<glob::Pattern> = excludes
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

    // Build walker. Include hidden files (e.g., .env, .vscode/).
    // By default, respects .gitignore — never descends into node_modules/, target/, etc.
    // With include_ignored, disable all git-based filtering to visit ignored files too.
    let mut builder = ignore::WalkBuilder::new(from_path);
    builder.hidden(false);
    if include_ignored {
        builder
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false);
    }

    let mut copied_files = Vec::new();

    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::debug!("Walk error: {}", e);
                continue;
            }
        };

        // Skip directories
        if entry.file_type().is_none_or(|ft| ft.is_dir()) {
            continue;
        }

        let path = entry.path();

        // Get relative path from from_path
        let rel_path = match path.strip_prefix(from_path) {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };

        let rel_path_str = match rel_path.to_str() {
            Some(s) => s,
            None => continue,
        };

        // Skip .git entry — in worktrees this is a file (not a directory) containing
        // a gitdir pointer, so the directory check above doesn't catch it. Copying it
        // would corrupt the destination worktree's git pointer.
        if rel_path == Path::new(".git") {
            continue;
        }

        // Skip files tracked in the git index (handles `git add -f`'d ignored files correctly)
        if tracked.contains(rel_path_str.as_bytes()) {
            on_skipped("tracked", &rel_path);
            continue;
        }

        // Apply include patterns (empty = match all)
        if !include_patterns.is_empty()
            && !include_patterns
                .iter()
                .any(|p| p.matches_with(rel_path_str, match_opts))
        {
            continue;
        }

        // Apply exclude patterns
        if exclude_patterns
            .iter()
            .any(|p| p.matches_with(rel_path_str, match_opts))
        {
            continue;
        }

        let dest_file = to_path.join(&rel_path);

        // Skip if destination exists and not forcing
        if dest_file.exists() && !force {
            on_skipped("exists", &rel_path);
            continue;
        }

        // Create parent directories if needed
        if let Some(parent) = dest_file.parent() {
            fs::create_dir_all(parent)?;
        }

        copy_file_platform(path, &dest_file)?;
        on_copied(&rel_path);
        copied_files.push(rel_path);
    }

    Ok(copied_files)
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
