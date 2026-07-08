mod tui;

use clap::{CommandFactory, Parser};
use clap_complete::env::CompleteEnv;
use git2::Repository;
use miette::{IntoDiagnostic, Result};
use workon_review::acquire::{diff_changeset, resolve_changesets};
use workon_review::app::{App, ChangesetView, Severity};
use workon_review::config::ReviewConfig;
use workon_review::keymap::Keymap;
use workon_review::theme::Palette;

/// A TUI for reviewing changesets
#[derive(Debug, Parser)]
#[clap(about, author, bin_name = env!("CARGO_PKG_NAME"), version)]
struct Cli {}

fn main() -> Result<()> {
    // Respond to the `COMPLETE=<shell>` dynamic-completion protocol before anything else — mirrors
    // git-workon's own entry point. Exits early when `COMPLETE` is set; a no-op otherwise. This is
    // what lets git-workon delegate `git workon review <TAB>` completion here (M6 CS3).
    CompleteEnv::with_factory(Cli::command).complete();

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

    // Resolve the keymap from git config once at startup, BEFORE `repo` moves into `App`
    // (ADR-028). A failed config read degrades to the registry defaults rather than aborting the
    // review. Collision/unknown-action warnings surface through the footer notice below.
    let keymap = match ReviewConfig::new(&repo).bindings() {
        Ok(bindings) => Keymap::from_bindings(&bindings),
        Err(_) => Keymap::defaults(),
    };

    // Resolve the palette selection the same way, before `repo` moves — a config-read error
    // degrades to dark rather than aborting the review (CS5); `Palette::for_theme` handles the
    // parsed-selection cases (including `Auto`'s CS6-deferred fallback to dark).
    let theme = ReviewConfig::new(&repo)
        .theme()
        .map(Palette::for_theme)
        .unwrap_or_else(|_| Palette::dark());

    // Resolve the view-config settings (outline width/mode, diff layout/zoom) the same way,
    // before `repo` moves — CS7. `view_config` reads into an owned `RawViewConfig`, so no
    // borrow of `repo` survives past this statement (unlike a bare `ReviewConfig<'repo>`, which
    // would still be borrowing `repo` when `App::from_changesets` tries to move it below).
    let view_config = ReviewConfig::new(&repo).view_config();

    // `App` owns its own `Repository` handle (see `app.rs`'s doc comment) — moved in here after
    // acquisition is done borrowing it. `App::from_changesets` opens on whichever changeset the
    // lib marked `current` (locked decision #6).
    let mut app = App::from_changesets(repo, views);

    // Apply CS7's view-config settings BEFORE `open_current`: `App::apply_view_config`'s setters
    // only set the raw layout/zoom/mode/width fields, and `open_current` is what derives
    // `cursor`/`scroll` fresh from whichever settings just landed (see each setter's doc
    // comment).
    let view_config_warnings = app.apply_view_config(&view_config);
    app.open_current();

    // A misconfigured keybinding or view-config setting is non-fatal: show the collected
    // warnings as a startup notice (cleared on the first keypress, like any notice) and run with
    // the defaults for those keys/settings.
    let mut warnings = keymap.warnings().to_vec();
    warnings.extend(view_config_warnings);
    if !warnings.is_empty() {
        app.notify(warnings.join("; "), Severity::Error);
    }

    tui::run(&mut app, &keymap, &theme).into_diagnostic()?;

    Ok(())
}
