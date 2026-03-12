use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use miette::{Result, WrapErr};
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
        let from_path = root.join(&self.from);
        let to_path = root.join(&self.to);

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
                self.to,
                to_path
            ));
        }

        let patterns = determine_patterns(self, &config)?;
        let excludes = config.copy_excludes()?;
        let include_ignored =
            config.copy_include_ignored(Some(self.include_ignored).filter(|&v| v))?;

        let json_mode = output::is_json_mode();
        let pb = ProgressBar::new_spinner();
        if json_mode {
            pb.set_draw_target(ProgressDrawTarget::hidden());
        } else {
            pb.set_style(
                ProgressStyle::with_template("  {spinner:.green} {msg}")
                    .unwrap()
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]),
            );
            pb.enable_steady_tick(Duration::from_millis(80));
            pb.set_message("Copying files...");
        }

        let show_skipped = log::log_enabled!(log::Level::Debug);
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
            self.from, self.to
        ))?;

        pb.finish_and_clear();
        if !json_mode {
            println!("\nCopied {} file(s)", copied.len());
        }

        Ok(Some(WorktreeDescriptor::new(&repo, &self.to)?))
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
