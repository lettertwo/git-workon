mod tui;

use clap::Parser;
use git2::Repository;
use miette::{IntoDiagnostic, Result};
use workon_review::acquire::diff_uncommitted;
use workon_review::app::App;

/// A TUI for reviewing changesets
#[derive(Debug, Parser)]
#[clap(about, author, bin_name = env!("CARGO_PKG_NAME"), version)]
struct Cli {}

fn main() -> Result<()> {
    Cli::parse();

    let repo = Repository::discover(".").into_diagnostic()?;
    let combined = diff_uncommitted(&repo).into_diagnostic()?.combined;

    if combined.files.is_empty() {
        eprintln!("nothing to review");
        return Ok(());
    }

    // `App` owns its own `Repository` handle (see `app.rs`'s doc comment) — moved in here after
    // `diff_uncommitted` is done borrowing it.
    let mut app = App::new(repo, combined);
    app.open_current();

    tui::run(&mut app).into_diagnostic()?;

    Ok(())
}
