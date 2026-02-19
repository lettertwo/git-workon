use std::ffi::OsString;

use clap::CommandFactory;
use clap_complete::engine::complete;
use miette::Result;
use workon::WorktreeDescriptor;

use crate::cli::{Cli, Complete};
use crate::completers;

use super::Run;

impl Run for Complete {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let mut cmd = completers::augment(Cli::command());
        let current_dir = std::env::current_dir().ok();

        // complete() expects the binary name as args[0] and a 0-based arg_index
        // into the full args slice (including that binary name). The shell passes
        // only the words after the wrapper name, so prepend a placeholder and
        // shift the index by 1.
        let mut args = vec![OsString::from(cmd.get_name().to_owned())];
        args.extend(self.args.clone());
        let index = self.index + 1;

        if let Ok(candidates) = complete(&mut cmd, args, index, current_dir.as_deref()) {
            for candidate in &candidates {
                let value = candidate.get_value().to_string_lossy();
                if let Some(help) = candidate.get_help() {
                    println!("{}\t{}", value, help);
                } else {
                    println!("{}", value);
                }
            }
        }

        Ok(None)
    }
}
