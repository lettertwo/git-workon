use miette::{IntoDiagnostic, Result, WrapErr};
use workon::{
    copy_untracked, get_repo, workon_root, CopyOptions, WorkonConfig, WorktreeDescriptor,
};

use crate::cli::CopyUntracked;
use crate::output;

use super::Run;

impl Run for CopyUntracked {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;
        let config = WorkonConfig::new(&repo)?;

        let root = workon_root(&repo)?;
        let to_name = self.to.as_deref().unwrap_or(".");
        let from_path = resolve_worktree_arg(&root, &self.from)?;
        let to_path = resolve_worktree_arg(&root, to_name)?;

        if !from_path.exists() {
            return Err(miette::miette!(
                "Source worktree '{}' does not exist at {:?}",
                self.from,
                from_path
            ));
        }
        if !to_path.exists() {
            return Err(miette::miette!(
                "Destination worktree '{}' does not exist at {:?}",
                to_name,
                to_path
            ));
        }

        let patterns = determine_patterns(self, &config)?;
        let excludes = determine_excludes(self, &config)?;
        let include_ignored =
            config.copy_include_ignored(Some(self.include_ignored).filter(|&v| v))?;

        let json_mode = output::is_json_mode();
        let pb = output::create_spinner();
        pb.set_message("Copying files...");

        let show_skipped = log::log_enabled!(log::Level::Trace);
        let mut count = 0usize;
        let pb_copied = pb.clone();
        let pb_skipped = pb.clone();
        let copied = copy_untracked(
            &from_path,
            &to_path,
            CopyOptions {
                patterns: &patterns,
                excludes: &excludes,
                force: self.force,
                include_ignored,
                on_copied: Box::new(move |rel_path| {
                    if !json_mode {
                        count += 1;
                        pb_copied.println(format!(
                            "      {} {}",
                            output::style::green_bold("Copied"),
                            rel_path.display()
                        ));
                        pb_copied.set_message(format!("Copying files... ({} copied)", count));
                    }
                }),
                on_skipped: Box::new(move |reason, rel_path| {
                    if show_skipped && !json_mode {
                        pb_skipped.println(format!(
                            "      {} {} ({})",
                            output::style::dim("Skipped"),
                            rel_path.display(),
                            reason,
                        ));
                    }
                }),
            },
        )
        .wrap_err(format!(
            "Failed to copy files from '{}' to '{}'",
            self.from, to_name
        ))?;

        pb.finish_and_clear();
        if !json_mode {
            println!("\nCopied {} file(s)", copied.len());
        }

        Ok(WorktreeDescriptor::new(&repo, to_name).ok())
    }
}

/// Determine which patterns to use for copying
///
/// Priority: --pattern flag > config > [] (empty = copy all untracked)
fn determine_patterns(cmd: &CopyUntracked, config: &WorkonConfig) -> Result<Vec<String>> {
    if let Some(pattern) = &cmd.pattern {
        return Ok(vec![pattern.clone()]);
    }
    Ok(config.copy_patterns()?)
}

/// Resolve a worktree name argument to a filesystem path.
///
/// The special value `.` resolves to the current worktree by finding the first
/// path component of the CWD relative to root — handles the case where the user
/// is inside a worktree and wants to refer to it without naming it explicitly.
fn resolve_worktree_arg(root: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    if name == "." {
        let cwd = std::env::current_dir()
            .into_diagnostic()
            .wrap_err("Failed to get current directory")?;
        if let Ok(rel) = cwd.strip_prefix(root) {
            if let Some(first) = rel.components().next() {
                return Ok(root.join(first));
            }
        }
        // CWD is at or above root — fall back to CWD itself
        return Ok(cwd);
    }
    Ok(root.join(name))
}

/// Determine which excludes to use for copying
///
/// CLI excludes are additive with config excludes.
fn determine_excludes(cmd: &CopyUntracked, config: &WorkonConfig) -> Result<Vec<String>> {
    let mut excludes = config.copy_excludes()?;
    excludes.extend(cmd.exclude.iter().cloned());
    Ok(excludes)
}
