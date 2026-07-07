mod tui;

use clap::Parser;
use git2::Repository;
use miette::{IntoDiagnostic, Result};
use workon_review::acquire::{diff_changeset, resolve_changesets};
use workon_review::app::{App, ChangesetView};

/// A TUI for reviewing changesets
#[derive(Debug, Parser)]
#[clap(about, author, bin_name = env!("CARGO_PKG_NAME"), version)]
struct Cli {}

fn main() -> Result<()> {
    Cli::parse();

    let repo = Repository::discover(".").into_diagnostic()?;
    let branch = repo
        .head()
        .into_diagnostic()?
        .shorthand()
        .into_diagnostic()?
        .to_string();

    // `resolve_changesets` is the M5 entry point (locked decision #7, auto-detect): the full
    // Graphite stack when one is active, or a single synthetic uncommitted changeset otherwise
    // — the latter keeps a non-Graphite repo byte-identical to M2–M4's `diff_uncommitted` path.
    let changesets = resolve_changesets(&repo, &branch).into_diagnostic()?;

    let mut views = Vec::with_capacity(changesets.len());
    for cs in changesets {
        let diff = diff_changeset(&repo, &cs).into_diagnostic()?;
        views.push(ChangesetView::from_changeset_diff(cs, diff));
    }

    if views.len() == 1 && views[0].file_count() == 0 {
        eprintln!("nothing to review");
        return Ok(());
    }

    // `App` owns its own `Repository` handle (see `app.rs`'s doc comment) — moved in here after
    // acquisition is done borrowing it. `App::from_changesets` opens on whichever changeset the
    // lib marked `current` (locked decision #6).
    let mut app = App::from_changesets(repo, views);
    app.open_current();

    tui::run(&mut app).into_diagnostic()?;

    Ok(())
}
