//! Prune command - remove worktrees whose branches are stale.
//!
//! ## Model
//!
//! Analysis always runs in full: every worktree in scope is evaluated for **all**
//! prunability signals (branch deleted, remote gone, merged into target, PR merged)
//! plus safety state (dirty, unmerged, locked, protected). Nothing is hidden from the
//! analysis; `--gone` and `--merged` only control which signals count as "active" for
//! pre-checking (interactive) and auto-pruning (non-interactive) — they never gate
//! visibility. `BranchDeleted` and `PrMerged` are always active criteria: a merged PR
//! is unambiguous evidence the work landed, so it needs no opt-in flag.
//!
//! - **Bare `prune`**: scope is every worktree except the default one. Only rows
//!   carrying at least one signal are shown/considered.
//! - **Named `prune <name>...`**: scope narrows to exactly those worktrees (matched
//!   by worktree name or branch name). Any unmatched name is a hard error listing all
//!   misses, before anything is touched. A named worktree with no signal at all still
//!   appears — annotated "not prunable" — since naming is how you pull a healthy tree
//!   into view; it is only pruned with `--force`.
//! - **Interactive** (TTY, no `--yes`/`--json`/`--dry-run`): a checkbox picker
//!   ([`crate::picker::multi_select`]) lists every selectable row in the find/list
//!   visual language, pre-checked according to the active-criteria + safety rules
//!   (Enter-Enter reproduces the safe default). Protected/locked rows are locked out
//!   of the picker (shown separately, annotated) unless `--force` /
//!   `--include-locked`. A single summary confirm follows selection.
//! - **Non-interactive** (`--yes`, `--json`, or no TTY): bare mode prunes exactly the
//!   pre-checked set; named mode prunes named worktrees when safe (dirty/unmerged
//!   still require `--allow-dirty`/`--allow-unmerged`; a healthy named worktree is
//!   skipped with a reason unless `--force`).
//! - **`--dry-run`**: prints the annotated analysis (pre-checked / selectable / locked
//!   out, with signals) and exits without touching anything.
//!
//! ## Fetch
//!
//! Fetch stays opt-in (`--fetch` / `workon.pruneFetch`). Without it, gone-upstream
//! annotations use cached refs, which can only under-report (never falsely flag a
//! branch as gone). In named mode, the prune-fetch narrows to remotes tracked by the
//! named worktrees only.
//!
//! The `PrMerged` signal runs unconditionally, gated on the repo having a GitHub
//! remote and `gh` being usable (a single `check_gh_available()` call, not one per
//! row) — `gh` cannot resolve a PR without either. Rows that already carry a signal
//! skip the lookup entirely. A merged PR only counts if its head OID actually covers
//! the branch's current tip (equal, or the tip is an ancestor of it); otherwise a
//! long-lived branch that was once a PR head (gitflow's `develop`/`production`, a
//! `release/*` line) would read as permanently merged even after it moved on. A
//! branch rewritten locally after its PR merged (restack, amend) loses the signal —
//! it still surfaces via `--gone` after a fetch, or by naming the worktree. Any
//! failure — `gh` missing, unauthenticated, or offline — degrades to no signal, with
//! no warning: same under-report-only guarantee as the fetch above. A per-row failure
//! alone doesn't stop the pass (a rate limit or DNS blip on one worktree shouldn't
//! blank the signal for the rest); the pass gives up only after 3 consecutive
//! failures, which bounds the cost of a repo-level failure (unauthenticated `gh`)
//! without paying one failing subprocess per worktree.
//!
//! ## Protected Branch Matching
//!
//! Patterns use standard glob syntax (`*`, `?`, `[...]`) via the [`glob`] crate, with
//! `*` matching across `/` (so `release-*` protects `release-v1` and `release/v1`
//! alike). Invalid patterns are a hard config error, not a silent no-op:
//! - Exact match: `main` protects only "main"
//! - Wildcard: `*` protects all branches, `release-*` protects "release-v1", etc.
//! - Prefix: `release/*` protects "release/v1", "release/v2", etc.
//!
//! ## Safety Leniency
//!
//! A row whose only signal is `RemoteGone` uses `has_tracked_changes()` instead of
//! `is_dirty()` for the dirty check, so untracked files (build artifacts, IDE dirs)
//! don't block pruning. `PrMerged` rows keep `is_dirty()`, same as any other signal.
//! The unmerged check is skipped entirely for any row carrying a signal
//! (`BranchDeleted`, `RemoteGone`, `Merged`, or `PrMerged`) — the signal already
//! implies the work was handled.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use dialoguer::Confirm;
use miette::{IntoDiagnostic, Report, Result, WrapErr};
use serde_json::json;
use workon::{
    check_gh_available, find_merged_pr, get_default_branch, get_repo, get_worktrees,
    has_github_remote, prune_fetch as remote_prune_fetch, remotes_tracked_by_worktrees, PruneError,
    WorktreeDescriptor,
};

use crate::cli::Prune;
use crate::display::{format_aligned_rows_annotated, worktree_display_row, WorktreeDisplayRow};
use crate::output;
use crate::picker;

use super::Run;

impl Run for Prune {
    fn run(&self) -> Result<Option<WorktreeDescriptor>> {
        let repo = get_repo(None)?;
        let config = workon::WorkonConfig::new(&repo)?;
        let protected_patterns: Vec<glob::Pattern> = config
            .prune_protected_branches()?
            .into_iter()
            .map(|p| {
                glob::Pattern::new(&p).into_diagnostic().wrap_err(format!(
                    "Invalid pattern '{}' in workon.pruneProtectedBranches",
                    p
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let effective_gone = config.prune_gone(self.gone_override()).into_diagnostic()?;
        let effective_fetch = config
            .prune_fetch(self.fetch_override())
            .into_diagnostic()?;
        let worktrees = get_worktrees(&repo)?;
        let default_branch = get_default_branch(&repo).ok();

        // Scope resolution: the default worktree never appears. Bare mode considers
        // every other worktree; named mode narrows to exactly the matched names, and
        // any miss is a hard error before anything else runs.
        let pool: Vec<&WorktreeDescriptor> = worktrees
            .iter()
            .filter(|wt| match (&default_branch, wt.branch()) {
                (Some(db), Ok(Some(b))) => &b != db,
                _ => true,
            })
            .collect();

        let named = !self.names.is_empty();
        let mut scope: Vec<&WorktreeDescriptor> = Vec::new();
        if named {
            let mut misses: Vec<String> = Vec::new();
            for name in &self.names {
                match pool.iter().find(|wt| {
                    wt.name() == Some(name.as_str())
                        || matches!(wt.branch(), Ok(Some(ref b)) if b == name)
                }) {
                    // Dedupe: repeated names (or a worktree named once by name and
                    // once by branch) resolve to the same worktree; pruning it twice
                    // would fail after the first pass deregistered it.
                    Some(wt) if scope.iter().any(|s| s.name() == wt.name()) => {}
                    Some(wt) => scope.push(wt),
                    None => misses.push(name.clone()),
                }
            }
            if !misses.is_empty() {
                return Err(Report::from(PruneError::NamesNotFound { names: misses }));
            }
        } else {
            scope = pool.clone();
        }

        let pb = output::create_spinner();

        // Phase 0 (optional): prune-fetch so gone-upstream detection reflects the
        // actual remote state. Named mode narrows the fetch to remotes tracked by the
        // named worktrees only. Failure is non-fatal: a stale fetch can only
        // under-prune, never cause a false prune.
        if effective_fetch {
            let fetch_scope = if named { &scope } else { &pool };
            let remotes = remotes_tracked_by_worktrees(&repo, fetch_scope.iter().copied())
                .into_diagnostic()?;
            for remote_name in &remotes {
                pb.set_message(format!("Fetching {}...", remote_name));
                if let Err(e) = remote_prune_fetch(&repo, remote_name) {
                    pb.suspend(|| {
                        output::warn(&format!(
                            "could not fetch {}: {}; gone status may be based on stale refs",
                            remote_name, e
                        ));
                    });
                }
            }
        }

        pb.set_message("Checking worktree status...");
        let merged_target: Option<String> = match &self.merged {
            Some(m) if !m.is_empty() => Some(m.clone()),
            _ => default_branch.clone(),
        };
        let merged_active = self.merged.is_some();

        let mut rows: Vec<PruneRow> = scope
            .iter()
            .map(|wt| {
                build_row(
                    &repo,
                    wt,
                    default_branch.as_deref(),
                    merged_target.as_deref(),
                    &protected_patterns,
                )
            })
            .collect();

        // Second pass: PR-merged status only comes from a network call, so it can't
        // live in build_row alongside the offline signals. `gh` can't resolve a PR
        // without a GitHub remote, so skip the pass entirely when there isn't one —
        // this also keeps every pre-existing prune test hermetic, since fixture repos
        // have no GitHub remote. Otherwise gate on `gh` being usable at all, so a
        // missing/unauthenticated `gh` costs one check instead of one per row. A
        // bounded run of consecutive per-row failures then stops the pass (see
        // `fill_pr_merged`'s doc comment), rather than one flaky row blanking the
        // signal for every row after it.
        if has_github_remote(&repo) && check_gh_available().is_ok() {
            let mut consecutive_failures = 0;
            for row in rows.iter_mut() {
                if fill_pr_merged(row) {
                    consecutive_failures = 0;
                } else {
                    consecutive_failures += 1;
                    if consecutive_failures >= 3 {
                        break;
                    }
                }
            }
        }
        pb.finish_and_clear();

        // Bare mode only ever shows rows carrying a signal; named mode shows exactly
        // the named worktrees, signal or not.
        let visible: Vec<&PruneRow> = if named {
            rows.iter().collect()
        } else {
            rows.iter().filter(|r| !r.signals.is_empty()).collect()
        };

        let active = ActiveCriteria {
            gone: effective_gone,
            merged: merged_active,
        };
        let overrides = Overrides::from_cmd(self);

        if self.dry_run && !self.json {
            render_dry_run(&visible, active, overrides);
            return Ok(None);
        }

        let interactive =
            !self.json && !self.dry_run && !self.yes && std::io::stdin().is_terminal();
        if interactive {
            return run_interactive(&repo, &visible, active, self);
        }

        let (to_prune, skipped) = classify(&visible, named, active, overrides);

        if self.json {
            return emit_json(
                &repo,
                &to_prune,
                &skipped,
                self.dry_run,
                self.keep_branch,
                self.force,
                self.include_locked,
            );
        }

        if !skipped.is_empty() {
            output::notice("Skipped worktrees (unsafe to prune):");
            for (row, reason) in &skipped {
                output::detail(&format!("  {} ({})", row.wt.path().display(), reason));
            }
            eprintln!();
        }

        if to_prune.is_empty() {
            output::status("No worktrees to prune");
            return Ok(None);
        }

        output::info("Worktrees to prune:");
        for row in &to_prune {
            output::detail(&format!(
                "  {} (branch: {}, reason: {})",
                row.wt.path().display(),
                row.branch,
                reason_display(row)
            ));
        }

        if !self.yes {
            let prompt = if !self.keep_branch {
                format!(
                    "Prune {} worktree(s) and delete their branches?",
                    to_prune.len()
                )
            } else {
                format!("Prune {} worktree(s)?", to_prune.len())
            };
            let confirmed = Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()
                .into_diagnostic()?;

            if !confirmed {
                output::notice("Cancelled");
                return Ok(None);
            }
        }

        let delete_branch = !self.keep_branch;
        let force_locked = self.force || self.include_locked;
        for row in &to_prune {
            let candidate = to_candidate(row);
            for label in collect_orphaned_stashes(&candidate) {
                output::warn(&format!(
                    "pruning '{}' orphans shelved changes: {} (not restorable)",
                    candidate.worktree_name, label
                ));
            }
            prune_worktree(&repo, &candidate, delete_branch, force_locked)?;
        }

        output::success(&format!("Pruned {} worktree(s)", to_prune.len()));
        Ok(None)
    }
}

/// A prune signal detected for a worktree. Signals are always computed for every
/// row in scope; `--gone`/`--merged` only decide whether a signal counts as "active".
#[derive(Debug, Clone, PartialEq, Eq)]
enum Signal {
    BranchDeleted,
    RemoteGone,
    Merged(String),
    PrMerged(u32),
}

impl std::fmt::Display for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Signal::BranchDeleted => write!(f, "branch deleted"),
            Signal::RemoteGone => write!(f, "remote gone"),
            Signal::Merged(target) => write!(f, "merged into {}", target),
            Signal::PrMerged(number) => write!(f, "PR #{} merged", number),
        }
    }
}

/// Which signals currently count as "active" (pre-checked / auto-pruned), per the
/// `--gone`/`--merged` flags. `BranchDeleted` and `PrMerged` are always active, so
/// neither has a field.
#[derive(Debug, Clone, Copy)]
struct ActiveCriteria {
    gone: bool,
    merged: bool,
}

/// Safety-check override flags, bundled to keep function signatures manageable.
#[derive(Debug, Clone, Copy)]
struct Overrides {
    force: bool,
    allow_dirty: bool,
    allow_unmerged: bool,
    include_locked: bool,
}

impl Overrides {
    fn from_cmd(cmd: &Prune) -> Self {
        Self {
            force: cmd.force,
            allow_dirty: cmd.allow_dirty,
            allow_unmerged: cmd.allow_unmerged,
            include_locked: cmd.include_locked,
        }
    }
}

/// A single worktree's full analysis: every signal it carries, plus raw safety state
/// (protected/locked/dirty/unmerged) with no override flags baked in. Override flags
/// (`--force`, `--allow-dirty`, etc.) are applied later, at classification time.
struct PruneRow<'a> {
    wt: &'a WorktreeDescriptor,
    name: String,
    branch: String,
    signals: Vec<Signal>,
    protected: bool,
    locked: bool,
    dirty: bool,
    unmerged: bool,
}

fn build_row<'a>(
    repo: &git2::Repository,
    wt: &'a WorktreeDescriptor,
    default_branch: Option<&str>,
    merged_target: Option<&str>,
    protected_patterns: &[glob::Pattern],
) -> PruneRow<'a> {
    let branch = match wt.branch() {
        Ok(Some(name)) => name,
        Ok(None) => "(detached HEAD)".to_string(),
        Err(_) => "(error reading branch)".to_string(),
    };

    let mut signals = Vec::new();
    if !branch.starts_with('(') {
        let branch_exists = repo.find_branch(&branch, git2::BranchType::Local).is_ok();
        if !branch_exists {
            signals.push(Signal::BranchDeleted);
        } else {
            if wt.has_gone_upstream().unwrap_or(false) {
                signals.push(Signal::RemoteGone);
            }
            if let Some(target) = merged_target {
                if wt.is_merged_into(target).unwrap_or(false) {
                    signals.push(Signal::Merged(target.to_string()));
                }
            }
        }
    }

    let protected = is_protected(&branch, protected_patterns);
    let locked = wt.is_locked().unwrap_or(false);

    // RemoteGone-only rows use has_tracked_changes(): untracked files (build
    // artifacts, IDE dirs) are common and not a safety concern for a gone upstream.
    let remote_gone_only = signals.len() == 1 && signals[0] == Signal::RemoteGone;
    let dirty = if remote_gone_only {
        wt.has_tracked_changes().unwrap_or(true)
    } else {
        wt.is_dirty().unwrap_or(true)
    };

    // Any signal implies the work is already handled (deleted, pushed-and-gone, or
    // merged), so the unmerged check is only meaningful for signal-less rows.
    let unmerged = if !signals.is_empty() {
        false
    } else if let Some(db) = default_branch {
        matches!(wt.is_merged_into(db), Ok(false))
    } else {
        false
    };

    PruneRow {
        wt,
        name: wt.name().unwrap_or("").to_string(),
        branch,
        signals,
        protected,
        locked,
        dirty,
        unmerged,
    }
}

/// Raise `Signal::PrMerged` for a row with no other signal, if `gh` reports a merged
/// PR for its branch whose head covers the worktree's current tip. Skips rows that
/// already carry a signal (no local branch to look up, or the network call would just
/// be redundant), and any `gh` failure degrades silently, matching the rest of
/// prune's under-report-only contract.
///
/// A merged PR alone isn't enough: gitflow-style long-lived branches (`develop`,
/// `production`) are repeatedly a PR's head and never stop being one, so a bare
/// "was this branch ever a merged PR's head" check fires forever on branches that
/// have long since moved on. `is_at_or_behind` confirms the merged PR's head OID
/// actually covers the branch's current tip (equal, or the tip is an ancestor of it)
/// before treating it as evidence the work landed. A branch rewritten locally after
/// its PR merged (restack, amend) loses this signal — it still surfaces via `--gone`
/// after a fetch, or by naming the worktree.
///
/// Returns false when the `gh` call itself failed. A branch with no PR, or a merged
/// PR whose head doesn't cover the tip, is not a failure — only the `gh` lookup
/// itself failing is. The caller tracks consecutive failures and gives up after
/// enough of them in a row that `gh` is probably unusable here, rather than aborting
/// on the first one and blanking the signal for every row after it.
fn fill_pr_merged(row: &mut PruneRow) -> bool {
    if !row.signals.is_empty() || row.branch.starts_with('(') {
        return true;
    }
    match find_merged_pr(&row.branch) {
        Ok(Some(merged)) => {
            if row.wt.is_at_or_behind(&merged.head_oid).unwrap_or(false) {
                row.signals.push(Signal::PrMerged(merged.number));
                // A merged PR is unambiguous evidence the work landed, so the unmerged
                // check (only meaningful for signal-less rows) no longer applies.
                row.unmerged = false;
            }
            true
        }
        Ok(None) => true,
        Err(_) => false,
    }
}

/// True if at least one of the row's signals is currently "active" per the
/// `--gone`/`--merged` flags. `BranchDeleted` and `PrMerged` are always active.
fn is_active(row: &PruneRow, active: ActiveCriteria) -> bool {
    row.signals.iter().any(|s| match s {
        Signal::BranchDeleted => true,
        Signal::RemoteGone => active.gone,
        Signal::Merged(_) => active.merged,
        Signal::PrMerged(_) => true,
    })
}

/// True if the row is locked out of selection entirely (protected or locked, and not
/// overridden). Locked-out rows never appear as picker choices.
fn is_locked_out(row: &PruneRow, overrides: Overrides) -> bool {
    (row.protected && !overrides.force)
        || (row.locked && !overrides.force && !overrides.include_locked)
}

fn lockout_reason(row: &PruneRow, overrides: Overrides) -> String {
    if row.protected && !overrides.force {
        "protected by workon.pruneProtectedBranches".to_string()
    } else if row.locked && !overrides.force && !overrides.include_locked {
        "locked (use --include-locked to override)".to_string()
    } else {
        String::new()
    }
}

/// Reason the row is unsafe to prune right now, or `None` if it's clear. Ordered:
/// protected, locked, dirty, unmerged — the same order the safety checks always ran.
fn blocked_reason(row: &PruneRow, overrides: Overrides) -> Option<String> {
    if !overrides.force && row.protected {
        return Some("protected by workon.pruneProtectedBranches".to_string());
    }
    if !overrides.force && !overrides.include_locked && row.locked {
        return Some("locked (use --include-locked to override)".to_string());
    }
    if !overrides.force && !overrides.allow_dirty && row.dirty {
        return Some("has uncommitted changes, use --allow-dirty to override".to_string());
    }
    if !overrides.force && !overrides.allow_unmerged && row.unmerged {
        return Some("has unmerged commits, use --allow-unmerged to override".to_string());
    }
    None
}

/// True if the row matches an active criterion and passes all safety checks —
/// Enter-Enter on the picker, or the whole set pruned by a bare `--yes` run.
fn is_prechecked(row: &PruneRow, active: ActiveCriteria, overrides: Overrides) -> bool {
    is_active(row, active) && blocked_reason(row, overrides).is_none()
}

fn annotate(row: &PruneRow) -> String {
    let mut parts: Vec<String> = row.signals.iter().map(|s| s.to_string()).collect();
    if row.dirty {
        parts.push("dirty".to_string());
    }
    if row.unmerged {
        parts.push("unmerged".to_string());
    }
    if parts.is_empty() {
        "not prunable".to_string()
    } else {
        parts.join(", ")
    }
}

fn reason_display(row: &PruneRow) -> String {
    if is_healthy(row) {
        "explicitly requested".to_string()
    } else {
        annotate(row)
    }
}

/// True if the row has nothing at all recommending it for pruning: no signal, no
/// uncommitted changes, no unmerged commits. Named mode still shows these rows (that's
/// the point of naming a healthy tree), but requires `--force` to actually prune one —
/// there's no affirmative reason to, only the fact that the user typed its name.
fn is_healthy(row: &PruneRow) -> bool {
    row.signals.is_empty() && !row.dirty && !row.unmerged
}

/// Split rows into "to prune" and "skipped (with reason)" for non-interactive runs.
///
/// Bare mode only considers rows with an active signal. Named mode bypasses the
/// active-criteria gate entirely (naming is itself the selection) but still runs
/// every safety check — a healthy row requires `--force`, while a dirty/unmerged row
/// still only needs `--allow-dirty`/`--allow-unmerged` since it did surface something.
fn classify<'a>(
    rows: &[&'a PruneRow<'a>],
    named: bool,
    active: ActiveCriteria,
    overrides: Overrides,
) -> (Vec<&'a PruneRow<'a>>, Vec<(&'a PruneRow<'a>, String)>) {
    let mut to_prune = Vec::new();
    let mut skipped = Vec::new();
    for row in rows {
        let row = *row;
        if named && is_healthy(row) && !overrides.force {
            skipped.push((
                row,
                "not prunable (no signal); use --force to override".to_string(),
            ));
            continue;
        }
        if !named && !is_active(row, active) {
            continue;
        }
        match blocked_reason(row, overrides) {
            Some(reason) => skipped.push((row, reason)),
            None => to_prune.push(row),
        }
    }
    (to_prune, skipped)
}

/// Render the annotated dry-run analysis (text mode) and exit without deleting.
fn render_dry_run(rows: &[&PruneRow], active: ActiveCriteria, overrides: Overrides) {
    if rows.is_empty() {
        output::status("No worktrees to prune");
        return;
    }

    output::info("Prune analysis:");
    for row in rows {
        let status = if is_locked_out(row, overrides) {
            "locked out"
        } else if is_prechecked(row, active, overrides) {
            "pre-checked"
        } else {
            "selectable"
        };
        output::detail(&format!(
            "  [{}] {} (branch: {}, {})",
            status,
            row.wt.path().display(),
            row.branch,
            annotate(row)
        ));
    }
    output::notice("\nDry run - no changes made");
}

/// Build aligned display rows + prune annotations for a set of picker rows.
///
/// Falls back to a minimal row when the descriptor can't be fully read, so the
/// output stays parallel to the input — picker indices must map 1:1 back to rows.
fn build_picker_rows(
    rows: &[&PruneRow],
    root: &Path,
    current_dir: &Path,
    annotation: impl Fn(&PruneRow) -> String,
) -> (Vec<WorktreeDisplayRow>, Vec<String>) {
    let display_rows = rows
        .iter()
        .map(|row| {
            worktree_display_row(row.wt, root, current_dir).unwrap_or_else(|_| WorktreeDisplayRow {
                is_active: false,
                dir_name: row.name.clone(),
                branch_annotation: (row.branch != row.name).then(|| row.branch.clone()),
                indicators: vec![],
                last_activity: String::new(),
                activity_epoch: None,
            })
        })
        .collect();
    let annotations = rows.iter().map(|row| annotation(row)).collect();
    (display_rows, annotations)
}

/// Interactive picker: a checkbox multi-select of every selectable row (pre-checked
/// per the active-criteria + safety rules), followed by a single summary confirm.
/// Locked-out rows (protected/locked, not overridden) are listed above the picker in
/// the same row format — the picker has no per-item disable, so they simply aren't
/// picker rows.
///
/// Rows render in the find/list visual language: dim `./` + bold dir name, colored
/// status indicators, dim activity time, dim branch annotation — plus a dim trailing
/// prune annotation (signals / "not prunable" / guard reason).
fn run_interactive(
    repo: &git2::Repository,
    visible: &[&PruneRow],
    active: ActiveCriteria,
    cmd: &Prune,
) -> Result<Option<WorktreeDescriptor>> {
    let overrides = Overrides::from_cmd(cmd);
    let locked_out: Vec<&PruneRow> = visible
        .iter()
        .copied()
        .filter(|r| is_locked_out(r, overrides))
        .collect();
    let selectable: Vec<&PruneRow> = visible
        .iter()
        .copied()
        .filter(|r| !is_locked_out(r, overrides))
        .collect();

    let root = workon::workon_root(repo)?;
    let current_dir = std::env::current_dir().into_diagnostic()?;

    if !locked_out.is_empty() {
        output::notice("Locked out (not selectable):");
        let (display_rows, annotations) = build_picker_rows(&locked_out, root, &current_dir, |r| {
            lockout_reason(r, overrides)
        });
        for line in format_aligned_rows_annotated(&display_rows, false, &annotations) {
            output::detail(&format!("  {}", line));
        }
        eprintln!();
    }

    if selectable.is_empty() {
        output::status("No worktrees to prune");
        return Ok(None);
    }

    let (display_rows, annotations) = build_picker_rows(&selectable, root, &current_dir, annotate);
    let items = format_aligned_rows_annotated(&display_rows, false, &annotations);
    let defaults: Vec<bool> = selectable
        .iter()
        .map(|row| is_prechecked(row, active, overrides))
        .collect();

    let chosen = picker::multi_select("Select worktrees to prune", &items, &defaults)?;

    let Some(indices) = chosen else {
        output::notice("Cancelled");
        return Ok(None);
    };

    if indices.is_empty() {
        output::status("No worktrees selected");
        return Ok(None);
    }

    let selected: Vec<&PruneRow> = indices.into_iter().map(|i| selectable[i]).collect();

    output::info(&format!(
        "{} worktree(s) and their branches will be deleted:",
        selected.len()
    ));
    for row in &selected {
        output::detail(&format!(
            "  {} (branch: {})",
            row.wt.path().display(),
            row.branch
        ));
        if row.dirty {
            output::warn(&format!("  '{}' has uncommitted changes", row.name));
        }
        if row.unmerged {
            output::warn(&format!("  '{}' has unmerged commits", row.name));
        }
    }
    for row in &selected {
        let candidate = to_candidate(row);
        for label in collect_orphaned_stashes(&candidate) {
            output::warn(&format!(
                "pruning '{}' orphans shelved changes: {} (not restorable)",
                candidate.worktree_name, label
            ));
        }
    }

    let prompt = if !cmd.keep_branch {
        format!(
            "Prune {} worktree(s) and delete their branches?",
            selected.len()
        )
    } else {
        format!("Prune {} worktree(s)?", selected.len())
    };
    let confirmed = Confirm::new()
        .with_prompt(prompt)
        .default(false)
        .interact()
        .into_diagnostic()?;

    if !confirmed {
        output::notice("Cancelled");
        return Ok(None);
    }

    let delete_branch = !cmd.keep_branch;
    let force_locked = cmd.force || cmd.include_locked;
    for row in &selected {
        let candidate = to_candidate(row);
        prune_worktree(repo, &candidate, delete_branch, force_locked)?;
    }

    output::success(&format!("Pruned {} worktree(s)", selected.len()));
    Ok(None)
}

fn emit_json(
    repo: &git2::Repository,
    to_prune: &[&PruneRow],
    skipped: &[(&PruneRow, String)],
    dry_run: bool,
    keep_branch: bool,
    force: bool,
    include_locked: bool,
) -> Result<Option<WorktreeDescriptor>> {
    let delete_branch = !keep_branch;
    let force_locked = force || include_locked;

    let mut pruned_rows: Vec<(&PruneRow, bool, Vec<String>)> = Vec::new();
    for row in to_prune {
        let candidate = to_candidate(row);
        let orphaned = collect_orphaned_stashes(&candidate);
        let branch_deleted = if dry_run {
            false
        } else {
            prune_worktree(repo, &candidate, delete_branch, force_locked)?
        };
        pruned_rows.push((row, branch_deleted, orphaned));
    }

    let result = json!({
        "pruned": pruned_rows.iter().map(|(row, branch_deleted, orphaned)| json!({
            "name": row.name,
            "path": row.wt.path().to_str(),
            "branch": row.branch,
            "reason": reason_display(row),
            "signals": row.signals.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "branch_deleted": branch_deleted,
            "orphaned_stashes": orphaned,
        })).collect::<Vec<_>>(),
        "skipped": skipped.iter().map(|(row, reason)| json!({
            "name": row.name,
            "path": row.wt.path().to_str(),
            "branch": row.branch,
            "reason": reason,
            "signals": row.signals.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "dry_run": dry_run,
    });

    let output = serde_json::to_string_pretty(&result).into_diagnostic()?;
    println!("{}", output);
    Ok(None)
}

struct PruneCandidate {
    worktree_name: String,
    worktree_path: PathBuf,
    branch_name: String,
    /// True when the branch is already gone (deleted or a detached/error sentinel),
    /// so there's no local branch ref left to clean up.
    skip_branch_delete: bool,
}

fn to_candidate(row: &PruneRow) -> PruneCandidate {
    PruneCandidate {
        worktree_name: row.name.clone(),
        worktree_path: row.wt.path().to_path_buf(),
        branch_name: row.branch.clone(),
        skip_branch_delete: row.signals.contains(&Signal::BranchDeleted)
            || row.branch.starts_with('('),
    }
}

fn prune_worktree(
    repo: &git2::Repository,
    candidate: &PruneCandidate,
    delete_branch: bool,
    force_locked: bool,
) -> Result<bool> {
    // Remove the worktree directory first
    if candidate.worktree_path.exists() {
        std::fs::remove_dir_all(&candidate.worktree_path).into_diagnostic()?;
        remove_empty_parents(repo, &candidate.worktree_path);
    }

    // Now prune the worktree metadata from git
    let worktree = repo
        .find_worktree(&candidate.worktree_name)
        .into_diagnostic()?;
    let mut opts = git2::WorktreePruneOptions::new();
    opts.valid(true); // Allow pruning even if worktree is valid
    if force_locked {
        opts.locked(true); // Allow pruning even if worktree is locked
    }
    worktree.prune(Some(&mut opts)).into_diagnostic()?;

    // Optionally delete the local branch ref.
    let branch_deleted = if delete_branch && !candidate.skip_branch_delete {
        match repo.find_branch(&candidate.branch_name, git2::BranchType::Local) {
            Ok(mut branch) => match branch.delete() {
                Ok(()) => true,
                Err(e) => {
                    output::warn(&format!(
                        "could not delete branch '{}': {}",
                        candidate.branch_name, e
                    ));
                    false
                }
            },
            Err(_) => false,
        }
    } else {
        false
    };

    if branch_deleted {
        output::success(&format!(
            "  Pruned {} (branch '{}' deleted)",
            candidate.worktree_path.display(),
            candidate.branch_name
        ));
    } else {
        output::success(&format!("  Pruned {}", candidate.worktree_path.display()));
    }

    Ok(branch_deleted)
}

/// Best-effort cleanup of namespace directories left empty by a prune: ascend from
/// the removed worktree's parent toward the workon root, removing each directory
/// until one is non-empty or the root is reached. Never fails the prune — a
/// directory that can't be removed (non-empty, permissions) is simply left alone.
fn remove_empty_parents(repo: &git2::Repository, worktree_path: &Path) {
    let Ok(root) = workon::workon_root(repo) else {
        return;
    };
    let mut dir = worktree_path.parent();
    while let Some(path) = dir {
        if path == root || !path.starts_with(root) {
            break;
        }
        if std::fs::remove_dir(path).is_err() {
            break;
        }
        dir = path.parent();
    }
}

/// Check if a branch name matches any of the protection patterns
fn is_protected(branch_name: &str, patterns: &[glob::Pattern]) -> bool {
    let opts = glob::MatchOptions::new();
    patterns
        .iter()
        .any(|pattern| pattern.matches_with(branch_name, opts))
}

fn collect_orphaned_stashes(candidate: &PruneCandidate) -> Vec<String> {
    let Ok(mut wt_repo) = git2::Repository::open(&candidate.worktree_path) else {
        return vec![];
    };
    workon::list_labeled_for_worktree(&mut wt_repo, &candidate.worktree_name).unwrap_or_default()
}
