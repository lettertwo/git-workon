//! Interactive configuration management command.
//!
//! Provides an interactive TUI for managing git-workon configuration,
//! as an alternative to manually editing git config.
//!
//! ### Planned Features:
//! - Interactive prompts for setting common config keys
//! - Display current config values with descriptions
//! - Input validation before writing to git config
//! - Support both global (~/.gitconfig) and local (.git/config) scopes
//! - Provide helpful examples and defaults for each setting

use crate::cli::Config;
use crate::Result;
use workon::WorktreeDescriptor;

use super::Run;

impl Run for Config {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        unimplemented!(
            "Config command is not yet implemented. \
             See git-workon/src/cmd/config.rs for implementation plan."
        )
    }
}
