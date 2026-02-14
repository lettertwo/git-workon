//! Structured CLI output with color support.
//!
//! Provides semantic output functions that handle stream selection (stdout vs stderr)
//! and conditional color formatting automatically.
//!
//! ## Output Categories
//!
//! **stderr** (status/diagnostic — never interferes with piping):
//! - [`warn`] — yellow "Warning:" prefix, for non-fatal issues
//! - [`success`] — green text, for completed actions
//! - [`info`] — bold text, for section headers
//! - [`detail`] — dim text, for secondary information
//! - [`notice`] — yellow text, for dry-run/cancelled/skipped status
//! - [`status`] — plain text, for neutral status messages
//!
//! **stdout** (primary data — pipeable to fzf, grep, etc.):
//! - Use `println!()` directly for primary output
//! - Use [`style`] helpers to build inline-colored strings for stdout
//!
//! ## Color Detection
//!
//! Color is enabled per-stream when the stream is a terminal and `NO_COLOR` env var
//! is not set. See <https://no-color.org/>.
//!
//! ### Planned Features
//! - `--json` output for programmatic use
//! - `--verbose` flag for debugging
//! - Pretty-printed worktree lists with aligned columns
//! - `--porcelain` for stable script-friendly output

// TODO: Implement --json output format
// TODO: Add --verbose debugging output
// TODO: Pretty-print worktree lists with column alignment
// TODO: Add --porcelain stable output format

use std::sync::OnceLock;

use owo_colors::OwoColorize;

/// Checks if we should use color (terminal + NO_COLOR not set)
fn use_color() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none()
            && supports_color::on(supports_color::Stream::Stdout).is_some()
    })
}

/// Print a warning to stderr. Formats as "Warning: {msg}".
pub fn warn(msg: &str) {
    if use_color() {
        eprintln!("{} {}", "Warning:".yellow(), msg);
    } else {
        eprintln!("Warning: {}", msg);
    }
}

/// Print a success message to stderr in green.
pub fn success(msg: &str) {
    if use_color() {
        eprintln!("{}", msg.green());
    } else {
        eprintln!("{}", msg);
    }
}

/// Print a header/info line to stderr in bold.
pub fn info(msg: &str) {
    if use_color() {
        eprintln!("{}", msg.bold());
    } else {
        eprintln!("{}", msg);
    }
}

/// Print a detail line to stderr in dim.
pub fn detail(msg: &str) {
    if use_color() {
        eprintln!("{}", msg.dimmed());
    } else {
        eprintln!("{}", msg);
    }
}

/// Print a notice to stderr in yellow (dry run, cancelled, skipped headers).
pub fn notice(msg: &str) {
    if use_color() {
        eprintln!("{}", msg.yellow());
    } else {
        eprintln!("{}", msg);
    }
}

/// Print a plain status message to stderr.
pub fn status(msg: &str) {
    eprintln!("{}", msg);
}

/// Style module for inline string formatting (checks stdout color support).
pub mod style {
    use super::*;

    pub fn bold(s: &str) -> String {
        if use_color() {
            s.bold().to_string()
        } else {
            s.to_string()
        }
    }

    pub fn yellow(s: &str) -> String {
        if use_color() {
            s.yellow().to_string()
        } else {
            s.to_string()
        }
    }

    pub fn green(s: &str) -> String {
        if use_color() {
            s.green().to_string()
        } else {
            s.to_string()
        }
    }

    pub fn red(s: &str) -> String {
        if use_color() {
            s.red().to_string()
        } else {
            s.to_string()
        }
    }

    pub fn red_bold(s: &str) -> String {
        if use_color() {
            s.red().bold().to_string()
        } else {
            s.to_string()
        }
    }
}
