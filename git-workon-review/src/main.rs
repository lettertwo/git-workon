mod tui;

use clap::{CommandFactory, Parser};
use clap_complete::env::CompleteEnv;
use git2::Repository;
use miette::{IntoDiagnostic, Result};
use workon_review::acquire::{diff_changeset, resolve_changesets};
use workon_review::app::{App, ChangesetView, Severity};
use workon_review::config::{self, ReviewConfig};
use workon_review::keymap::Keymap;
use workon_review::source::{resolve_source, Source};
use workon_review::terminal_query;
use workon_review::theme::Palette;

/// A TUI for reviewing changesets
#[derive(Debug, Parser)]
#[clap(about, author, bin_name = env!("CARGO_PKG_NAME"), version)]
struct Cli {
    /// What to review: stack, uncommitted, or (later CSes) a ref/range/PR
    #[arg(value_name = "SOURCE")]
    source: Option<String>,
}

fn main() -> Result<()> {
    // Respond to the `COMPLETE=<shell>` dynamic-completion protocol before anything else — mirrors
    // git-workon's own entry point. Exits early when `COMPLETE` is set; a no-op otherwise. This is
    // what lets git-workon delegate `git workon review <TAB>` completion here (M6 CS3).
    CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();

    let repo = Repository::discover(".").into_diagnostic()?;
    let branch = repo
        .head()
        .into_diagnostic()?
        .shorthand()
        .into_diagnostic()?
        .to_string();

    // No `[SOURCE]` argument: the M5 auto-detect entry point (locked decision #7), unchanged —
    // the full Graphite stack when one is active, or a single synthetic uncommitted changeset
    // otherwise (keeps a non-Graphite repo byte-identical to M2–M4's `diff_uncommitted` path).
    // A `[SOURCE]` argument routes through the ADR-036 classifier/resolver instead (M7 CS2).
    let changesets = match cli.source {
        None => resolve_changesets(&repo, &branch).into_diagnostic()?,
        Some(text) => {
            let source = Source::classify(&text);
            resolve_source(&repo, &branch, source).into_diagnostic()?
        }
    };

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
    // (ADR-034). A failed config read degrades to the registry defaults rather than aborting the
    // review. Collision/unknown-action warnings surface through the footer notice below.
    let keymap = match ReviewConfig::new(&repo).bindings() {
        Ok(bindings) => Keymap::from_bindings(&bindings),
        Err(_) => Keymap::defaults(),
    };

    // Resolve the palette selection the same way, before `repo` moves — a config-read error
    // degrades to dark rather than aborting the review (CS5). `Auto` runs the terminal-derivation
    // probe (CS6), which needs the controlling tty and so lives outside the pure `theme.rs`; it is
    // bounded by a hard timeout and always yields a curated fallback on a silent/hostile terminal,
    // never a hang. `Dark`/`Light` stay CS5's I/O-free `for_theme` path.
    let selection = ReviewConfig::new(&repo).theme();
    let probed = matches!(selection, Ok(config::Theme::Auto));
    let theme = match selection {
        Ok(config::Theme::Auto) => terminal_query::detect_auto_palette(),
        Ok(selection) => Palette::for_theme(selection),
        Err(_) => Palette::dark(),
    };

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

    // After a probe, OSC replies from a slow terminal (e.g. one ssh round-trip away) may have
    // straggled in while the changesets were being assembled above. Discard them now, right
    // before crossterm takes the terminal — parsed as input they become phantom keystrokes
    // (`r` fires refreshes; `d` opens the discard confirm, which then swallows every key until
    // Esc/n: the "unresponsive for ~30s with theme=auto" startup). Un-probed launches skip this
    // so legitimate type-ahead survives.
    if probed {
        terminal_query::flush_pending_tty_input();
    }
    tui::run(&mut app, &keymap, &theme).into_diagnostic()?;

    Ok(())
}
