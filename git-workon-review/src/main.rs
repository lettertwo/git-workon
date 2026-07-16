mod tui;

use clap::{CommandFactory, Parser};
use clap_complete::engine::ArgValueCompleter;
use clap_complete::env::CompleteEnv;
use git2::Repository;
use miette::{IntoDiagnostic, Result};
use workon_review::acquire::{diff_changesets, resolve_changesets};
use workon_review::app::{App, ChangesetView, Severity};
use workon_review::config::{self, ReviewConfig};
use workon_review::keymap::{self, Command, Keymap};
use workon_review::source::{complete_source, resolve_source, Source};
use workon_review::terminal_query;
use workon_review::theme::Palette;

/// A TUI for reviewing changesets
#[derive(Debug, Parser)]
#[clap(about, author, bin_name = env!("CARGO_PKG_NAME"), version)]
struct Cli {
    /// What to review: stack, uncommitted, a ref (branch/tag/commit), a..b / a...b range, or
    /// a PR reference
    #[arg(value_name = "SOURCE", add = ArgValueCompleter::new(complete_source))]
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
    // A `[SOURCE]` argument routes through the ADR-030 classifier/resolver instead (M7 CS2/CS3).
    // `source` is kept (not just the resolved changesets) so it can be handed to `App` below —
    // `App::refresh` re-runs THIS same ask on every refresh rather than downgrading to
    // auto-detect (M7 CS2 fix).
    //
    // Everything from here through the theme probe runs BEFORE the terminal is taken (CS5's
    // splash enters the alternate screen further down, for the diff/build phase only). That
    // ordering is deliberate, not incidental:
    // - PR resolution fetches over the network, and auth-git2 may interactively PROMPT for an
    //   ssh passphrase / https credentials — inside raw-mode alternate screen the prompt would
    //   stair-step over the splash and leave the user typing blind.
    // - "nothing to review" exits below without ever needing a tty (CI, test harnesses).
    // - The `theme = auto` probe must own the tty while it converses, and its straggler flush
    //   discards ALL pending input — flushing before the alternate screen appears means nothing
    //   a user types at a visible TUI is ever eaten (a q typed right after the screen flips
    //   must quit, not vanish; see pty_smoke.rs's silent-terminal test).
    // Resolve itself is milliseconds locally, so the splash still appears near-instantly for
    // the launch that matters (a deep stack's diff work, below).
    let source = cli.source.as_deref().map(Source::classify);
    let changesets = match &source {
        None => resolve_changesets(&repo, &branch).into_diagnostic()?,
        Some(source) => resolve_source(&repo, &branch, source.clone()).into_diagnostic()?,
    };

    // A resolved source can legitimately name zero changesets — `stack` on a branch that's
    // caught up with its upstream and has a clean tree hits `assemble_git`'s empty-vec arm
    // (see `git_inference_caught_up_and_clean_returns_empty` in git-workon-lib), same as the
    // single-uncommitted-changeset case with nothing in it. Both are "nothing to review" +
    // exit 0 (ADR-030). The file-count gate needs per-changeset counts, not views — checked
    // against the resolved changesets' diffs only after they're built, so the empty case is
    // detected on the cheap resolve data here first.
    if changesets.is_empty() {
        match cli.source.as_deref() {
            Some(text) => eprintln!("nothing to review in {text}"),
            None => eprintln!("nothing to review"),
        }
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
    // degrades to dark rather than aborting the review (CS5). `Auto` runs the terminal-derivation
    // probe (CS6), which needs the controlling tty and so lives outside the pure `theme.rs`; it is
    // bounded by a hard timeout and always yields a curated fallback on a silent/hostile terminal,
    // never a hang. `Dark`/`Light` stay CS5's I/O-free `for_theme` path.
    // `probed` is whether a real probe conversation happened on the tty this launch — NOT just
    // "theme was auto". `detect_auto_palette` reports `false` on a cached "silent terminal"
    // verdict (see `probe_cache`), since a cache hit writes nothing to the tty and so owes no
    // flush; every other path (an answered probe, a timed-out-uncached probe, a non-auto theme)
    // is `false`/`true` exactly as before.
    let selection = ReviewConfig::new(&repo).theme();
    let (theme, probed) = match selection {
        Ok(config::Theme::Auto) => terminal_query::detect_auto_palette(),
        Ok(selection) => (Palette::for_theme(selection), false),
        Err(_) => (Palette::dark(), false),
    };

    // Resolve the view-config settings (outline width/mode, diff layout/zoom) the same way,
    // before `repo` moves — CS7. `view_config` reads into an owned `RawViewConfig`, so no
    // borrow of `repo` survives past this statement (unlike a bare `ReviewConfig<'repo>`, which
    // would still be borrowing `repo` when `App::from_changesets` tries to move it below).
    let view_config = ReviewConfig::new(&repo).view_config();

    // After a probe, OSC replies from a slow terminal (e.g. one ssh round-trip away) may have
    // straggled in while the theme was being derived above. Discard them now, BEFORE crossterm
    // takes the terminal — parsed as input they become phantom keystrokes (`r` fires refreshes;
    // `d` opens the discard confirm, which then swallows every key until Esc/n: the
    // "unresponsive for ~30s with theme=auto" startup). Un-probed launches skip this so
    // legitimate type-ahead survives. This MUST stay ahead of `Tui::acquire`: once the
    // alternate screen is visible, a user's keystrokes are real input a flush must never eat.
    if probed {
        terminal_query::flush_pending_tty_input();
    }

    // CS5: take the terminal while the diffs build — on a deep stack this used to be the bulk of
    // the launch with the terminal dead the whole time. Everything that could print, prompt, or
    // flush is done (see the block comment above the resolve), so from here the terminal belongs
    // to the TUI. `Tui`'s Drop restores it, so the `?`s below put the shell back before miette
    // prints their error.
    //
    // An acquire FAILURE (no controlling tty — CI, a test harness, a bare pipe) is carried, not
    // propagated here: a clean worktree's "nothing to review" is only detectable AFTER the diff
    // below (resolve always yields at least the uncommitted changeset), and that exit must stay
    // tty-free, exactly as it was when the terminal was only taken inside the run call. The
    // error surfaces at the run call — the same logical point it always did.
    let mut tui = tui::Tui::acquire();

    // ADR-031: `main.rs` forks on `changesets.len()` — streaming's grain is per-changeset, so a
    // lone changeset (the non-Graphite default, a ref/range, a PR) gains nothing from it and
    // keeps today's synchronous path byte-identical (down to the `clean_worktree_prints_
    // nothing_to_review_and_exits_success` canary, which must stay tty-free). A real stack
    // streams instead: the outline appears immediately with every row `Pending`, diffs land
    // as they complete, and the splash — redundant once the first frame IS the live outline —
    // is skipped entirely.
    let repo_path = repo.workdir().unwrap_or_else(|| repo.path()).to_path_buf();

    if changesets.len() == 1 {
        if let Ok(tui) = tui.as_mut() {
            let _ = tui.splash("diffing 1 changeset…");
        }
        let diffs = diff_changesets(&repo, &changesets).into_diagnostic()?;
        let views: Vec<ChangesetView> = changesets
            .into_iter()
            .zip(diffs)
            .map(|(cs, diff)| ChangesetView::from_changeset_diff(cs, diff))
            .collect();

        // The single-uncommitted-changeset case with nothing in it only shows up in the built
        // views' file counts — the mirror of the resolve-level empty check above, and the same
        // "nothing to review" + exit 0 (ADR-030), never a `views` list handed to
        // `App::from_changesets`, which panics on empty input. Restore the terminal BEFORE
        // printing: the message must land on the normal screen, not vanish with the alternate
        // one. A tty-less launch has no terminal to restore — the message prints exactly as
        // before CS5.
        if views.is_empty() || (views.len() == 1 && views[0].file_count() == 0) {
            if let Ok(tui) = tui.as_mut() {
                tui.restore().into_diagnostic()?;
            }
            match cli.source.as_deref() {
                Some(text) => eprintln!("nothing to review in {text}"),
                None => eprintln!("nothing to review"),
            }
            return Ok(());
        }

        // `App` owns its own `Repository` handle (see `app.rs`'s doc comment) — moved in here
        // after acquisition is done borrowing it. `App::from_changesets` opens on whichever
        // changeset the lib marked `current` (locked decision #6).
        let mut app = seat_app(repo, views, source, &view_config, &keymap);

        // A carried acquire failure surfaces HERE — the same logical point (running the TUI) it
        // surfaced at before CS5 moved the terminal takeover ahead of the diff phase.
        tui.into_diagnostic()?
            .run(&mut app, &keymap, &theme, repo_path)
            .into_diagnostic()?;
    } else {
        // Every changeset starts `Pending` (ADR-031's "Slots") — `App` is constructible from
        // resolved-but-undiffed changesets, so the outline's headers render on the FIRST frame,
        // before a single byte has been diffed. No splash: the live outline IS the launch
        // feedback.
        let views: Vec<ChangesetView> = changesets
            .iter()
            .cloned()
            .map(ChangesetView::pending)
            .collect();

        let mut app = seat_app(repo, views, source, &view_config, &keymap);

        tui.into_diagnostic()?
            .run_streamed(&mut app, &keymap, &theme, repo_path, changesets)
            .into_diagnostic()?;
    }

    Ok(())
}

/// The app-seating tail both `changesets.len()` arms of `main` share byte-identically (F5):
/// build `App` from `views`, wire the review source, defer file loads (CS4), apply CS7's
/// view-config settings, open the current file, and surface any keymap/view-config warnings as
/// a startup notice. `open_current` is a no-op on an empty file list — safe for the streamed
/// arm's `Pending` slots (no files yet), which `Tui::run_streamed`'s `ChangesetReady` handling
/// re-runs it for once the active changeset's diff actually lands.
fn seat_app(
    repo: Repository,
    views: Vec<ChangesetView>,
    source: Option<Source>,
    view_config: &config::RawViewConfig,
    keymap: &Keymap,
) -> App {
    let mut app = App::from_changesets(repo, views);
    if let Some(source) = source {
        app.set_review_source(source);
    }
    // Plumb the resolved CycleZoom binding into the "cycle zoom" refusal hint (App has no keymap
    // field of its own — see `App::zoom_key_label`'s doc comment); leaves the "Z" default in
    // place if the command has no bound key.
    if let Some(label) = keymap::primary_key(keymap, Command::CycleZoom) {
        app.set_zoom_key_label(label);
    }
    // CS4: defer file loads to the event loop's input-idle window rather than blocking here (or
    // on any later selection change) — `app.open_current()` below marks the initial open pending
    // instead of loading eagerly; see `tui::run`'s doc comment for the resulting startup
    // contract.
    app.set_defer_loads(true);

    // Apply CS7's view-config settings BEFORE `open_current`: `App::apply_view_config`'s setters
    // only set the raw layout/zoom/mode/width fields, and `open_current` is what derives
    // `cursor`/`scroll` fresh from whichever settings just landed (see each setter's doc
    // comment).
    let view_config_warnings = app.apply_view_config(view_config);
    app.open_current();

    // A misconfigured keybinding or view-config setting is non-fatal: show the collected
    // warnings as a startup notice (cleared on the first keypress, like any notice) and run with
    // the defaults for those keys/settings.
    let mut warnings = keymap.warnings().to_vec();
    warnings.extend(view_config_warnings);
    if !warnings.is_empty() {
        app.notify(warnings.join("; "), Severity::Error);
    }

    app
}
