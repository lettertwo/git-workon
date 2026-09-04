//! `ReviewConfig` — git-native config reader for the review TUI.
//!
//! Mirrors `git-workon-lib/src/config.rs`'s `WorkonConfig` pattern: reads via
//! `repo.config()` (git2's layered config: local `.git/config` > global `~/.gitconfig` >
//! system), typed getters over `get_string`/`get_i64`/[`git2::Config::entries`]. See
//! [ADR-006](../../../docs/adr/006-git-native-config.md) for the git-native config decision
//! this extends, and [ADR-034](../../../docs/adr/034-review-git-native-config-schema.md) for
//! the `workon.review.*` schema this reads.
//!
//! ## Status
//!
//! CS1 of the everyday-usability pass (see `docs/plans/review-usability-pass.md`): reader
//! infrastructure + typed getters only. Nothing here is wired into rendering or dispatch yet
//! — that's CS2 (keymaps), CS4/CS5/CS6 (theming), and CS7 (view settings).
//!
//! ## Configuration keys
//!
//! ```gitconfig
//! [workon "review"]
//!   theme = dark               ; auto | dark | light (default: auto)
//!   icons = nerd               ; nerd | none (default: none)
//!
//! [workon "review.theme"]
//!   base00 = #101010           ; base16 slot override (base00-base0f, lowercase)
//!   base0e = a626a4             ; #rrggbb or bare rrggbb, no 3-digit shorthand
//!   cursor-bg = #1a2b3c        ; diff/cursor tint override (kebab-case, see ThemeOverrides)
//!
//! [workon "review.diff.bind"]
//!   stage-hunk = s x           ; action = key tokens (space-separated)
//!
//! [workon "review.outline.bind"]
//!   open = enter
//!
//! [workon "review.bind"]
//!   quit = q esc               ; bare `review.bind` = global view (active in every view)
//!   reload-config = R          ; re-read this whole tree without restarting (default: R)
//!
//! [workon "review.outline"]
//!   width = 32
//!   mode = tree
//!   order = base-first         ; head-first | base-first (default: head-first)
//!
//! [workon "review.diff"]
//!   layout = split
//!   zoom = combined
//!   text = syntax             ; syntax | tint | edit (default: syntax)
//! ```
//!
//! ## Live reload (`reload-config`)
//!
//! The whole `workon.review.*` tree is re-readable at runtime — `reload-config` (`R` by
//! default) re-runs [`resolve_runtime`] against the live `.git/config` and swaps in the result,
//! no restart needed. One caveat: `theme = auto`'s terminal-derivation probe never re-runs mid-
//! session (it needs the tty, which the TUI owns once the alternate screen is live, and a second
//! conversation there would corrupt input) — switching `theme` to `dark`/`light` on reload takes
//! effect immediately, but switching back TO `auto` reuses the cached startup probe rather than
//! asking the terminal again. See [`crate::theme::PaletteContext`]'s doc comment.
//!
//! ## `icons`
//!
//! Opt-in nerd-font iconography — top-level next to `theme` (`workon.review.icons`), NOT an
//! outline setting: the mode gates the outline's file/dir icons, the summary panel's glyphs,
//! and the winbar's marker/diffstat/file icons alike. There is deliberately NO auto-detection
//! — a terminal cannot report whether the user's font is patched with the nerd-font glyphs, so
//! guessing would silently render tofu/mojibake for anyone without one. Default is `none`
//! (plain text); set `icons = nerd` explicitly once your terminal font supports it. See
//! [`crate::icons`] for the glyph table.

use git2::Repository;
use ratatui::style::Color;

use crate::keymap::Keymap;
use crate::theme::{self, Palette, PaletteContext, ThemeOverrides};

/// Which view a keybinding or view-setting applies to.
///
/// `Global` is the bare `workon.review.bind.<action>` / has no view segment in the config
/// key — active in every view. `Diff` and `Outline` are the per-view namespaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    Global,
    Diff,
    Outline,
}

impl View {
    /// The config key segment for this view, or `None` for [`View::Global`], which has no
    /// segment (`workon.review.bind.<action>`, not `workon.review.global.bind.<action>`).
    fn as_key_segment(self) -> Option<&'static str> {
        match self {
            View::Global => None,
            View::Diff => Some("diff"),
            View::Outline => Some("outline"),
        }
    }

    fn parse_segment(segment: &str) -> Option<View> {
        match segment {
            "diff" => Some(View::Diff),
            "outline" => Some(View::Outline),
            _ => None,
        }
    }
}

/// `workon.review.theme` — see [ADR-035](../../../docs/adr/035-review-theming-base16-hybrid.md).
///
/// `auto` (terminal-derived) is the spec default; the terminal-derivation probe itself is
/// CS6. Until CS6 lands, callers of [`ReviewConfig::theme`] decide how to treat `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Auto,
    Dark,
    Light,
}

/// One decomposed `workon.review.<view>.bind.<action>` (or bare `workon.review.bind.<action>`)
/// config entry: the raw, unparsed value string. Token-grammar parsing (space/reserved-word/
/// modifier/chord) is CS2's job — see [ADR-034](../../../docs/adr/034-review-git-native-config-schema.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBinding {
    pub view: View,
    pub action: String,
    /// Space-separated key tokens, unparsed (e.g. `"j down"`, `"]f"`, `""` for an explicit
    /// unbind).
    pub keys: String,
}

/// The four CS7 view-config settings, read raw (unset → `None`) and owned — see
/// [`ReviewConfig::view_config`]. Validation (range/enum checks) and default fallback are
/// [`crate::app::App::apply_view_config`]'s job, same division as [`RawBinding`]/CS2.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawViewConfig {
    pub outline_width: Option<i64>,
    pub outline_mode: Option<String>,
    pub outline_order: Option<String>,
    pub icons: Option<String>,
    pub diff_layout: Option<String>,
    pub diff_zoom: Option<String>,
    pub diff_text: Option<String>,
}

/// Everything `main.rs`'s startup resolution ladder reads out of `workon.review.*`, resolved in
/// one call so a config reload (the `reload-config` action, see
/// [ADR-034](../../../docs/adr/034-review-git-native-config-schema.md)) reproduces startup's
/// resolution exactly rather than duplicating it — guaranteed drift otherwise. `view_config` is left raw
/// (unvalidated): [`crate::app::App::apply_view_config`]/`reload_view_config` are what apply
/// defaults and range-check it, same division [`RawViewConfig`] already documents.
pub struct RuntimeConfig {
    pub keymap: Keymap,
    pub palette: Palette,
    pub view_config: RawViewConfig,
    /// `workon.review.theme.*` override warnings, plus unknown-key warnings for the rest of the
    /// `workon.review.*` tree (see [`ReviewConfig::unknown_key_warnings`]) — keymap warnings
    /// still come off [`Keymap::warnings`], not bundled in here, so a caller that only cares
    /// about one doesn't have to pick them back apart.
    pub warnings: Vec<String>,
}

/// Resolve the whole `workon.review.*` tree into a [`RuntimeConfig`], reproducing `main.rs`'s
/// startup ladder exactly: keymap (bindings, defaulting on a read error) → palette (selection →
/// base, `Auto` from `ctx.auto_base` rather than re-probing, `Dark`/`Light` via
/// [`Palette::for_theme`], a read error to [`Palette::dark`]) → `theme.*` overrides applied on
/// top → `ctx.no_color`'s mono override, applied last so it always wins. Every getter here
/// degrades to a default on a config-read error rather than propagating one — the same posture
/// every getter in this module already has, so a reload can never abort mid-session over a
/// transient/malformed `.git/config`.
///
/// Shared by both the startup resolution (`main.rs`) and a live `reload-config` (`tui.rs`) so the
/// two can never drift apart.
pub fn resolve_runtime(repo: &Repository, ctx: &PaletteContext) -> RuntimeConfig {
    let config = ReviewConfig::new(repo);

    let keymap = match config.bindings() {
        Ok(bindings) => Keymap::from_bindings(&bindings),
        Err(_) => Keymap::defaults(),
    };

    let mut palette = match config.theme() {
        Ok(Theme::Auto) => ctx.auto_base.clone(),
        Ok(selection) => Palette::for_theme(selection),
        Err(_) => Palette::dark(),
    };

    let mut warnings = match config.theme_overrides() {
        Ok((overrides, warnings)) => {
            palette.apply_overrides(&overrides);
            warnings
        }
        Err(_) => Vec::new(),
    };
    if let Ok(unknown) = config.unknown_key_warnings() {
        warnings.extend(unknown);
    }

    if ctx.no_color {
        palette = Palette::mono(theme::is_light_background(palette.background));
    }

    RuntimeConfig {
        keymap,
        palette,
        view_config: config.view_config(),
        warnings,
    }
}

/// Decompose a fully-qualified config variable name (as returned by
/// [`git2::ConfigEntry::name`]) into its (view, action) components, per ADR-034's grammar:
/// bare `workon.review.bind.<action>` is the global keymap; `workon.review.<view>.bind.<action>`
/// is a per-view keymap entry. Returns `None` for anything else under `workon.review.*`
/// (`theme`, a view setting, or an unrecognized shape) — not this reader's job to
/// validate/warn on unknown bind shapes; that's CS2's collision/unknown-action validation
/// pass. View settings and `theme` are read directly by their own getters, not through this.
fn parse_bind_key(name: &str) -> Option<(View, String)> {
    let rest = name.strip_prefix("workon.review.")?;
    let parts: Vec<&str> = rest.split('.').collect();
    match parts.as_slice() {
        ["bind", action] => Some((View::Global, (*action).to_string())),
        [view, "bind", action] => {
            View::parse_segment(view).map(|view| (view, (*action).to_string()))
        }
        _ => None,
    }
}

/// Decode a `workon.review.theme.<key>` key segment (everything after the `theme.` prefix) into
/// a base16 slot index `0..16` (`base00`–`base0f`), or `None` if it isn't a slot key. Git
/// lowercases config variable names, so `key` always arrives lowercase already (`BASE0A` in
/// git config reads back as `base0a`) — no case-folding needed here.
fn slot_index(key: &str) -> Option<usize> {
    let hex = key.strip_prefix("base")?;
    if hex.len() != 2 {
        return None;
    }
    let index = u8::from_str_radix(hex, 16).ok()? as usize;
    (index < 16).then_some(index)
}

/// Resolve a `workon.review.theme.<key>` tint key (kebab-case, mirroring
/// [`crate::theme::Palette`]'s tint field names) to the matching mutable slot on `overrides`, or
/// `None` if `key` isn't a recognized tint key.
fn tint_slot<'a>(overrides: &'a mut ThemeOverrides, key: &str) -> Option<&'a mut Option<Color>> {
    Some(match key {
        "del-line-bg" => &mut overrides.del_line_bg,
        "del-edit-bg" => &mut overrides.del_edit_bg,
        "add-line-bg" => &mut overrides.add_line_bg,
        "add-edit-bg" => &mut overrides.add_edit_bg,
        "del-staged-line-bg" => &mut overrides.del_staged_line_bg,
        "del-staged-edit-bg" => &mut overrides.del_staged_edit_bg,
        "add-staged-line-bg" => &mut overrides.add_staged_line_bg,
        "add-staged-edit-bg" => &mut overrides.add_staged_edit_bg,
        "add-fg" => &mut overrides.add_fg,
        "del-fg" => &mut overrides.del_fg,
        "add-staged-fg" => &mut overrides.add_staged_fg,
        "del-staged-fg" => &mut overrides.del_staged_fg,
        "cursor-bg" => &mut overrides.cursor_bg,
        "selection-bg" => &mut overrides.selection_bg,
        "cursor-unfocused-bg" => &mut overrides.cursor_unfocused_bg,
        "pane-header-focused-fg" => &mut overrides.pane_header_focused_fg,
        "filler-fg" => &mut overrides.filler_fg,
        _ => return None,
    })
}

/// The warning message for a `workon.review.theme.<key>` value that didn't parse as a color —
/// shared by the slot and tint branches of [`ReviewConfig::theme_overrides`]. Names no fallback
/// (unlike the view-setting warnings in `App::apply_view_config`): an ignored override has
/// none, the underlying scheme's value stands.
fn invalid_color_warning(key: &str, raw: &str) -> String {
    format!(
        "workon.review.theme.{key}: invalid color {raw:?} (expected #rrggbb or rrggbb); ignoring"
    )
}

// ── Unknown-key registry (config validation completeness) ──────────────────────────────────
//
// ADR-034's "Revised (config validation completeness)" section: `theme.*` and bind actions
// already warn on an unrecognized name; every other `workon.review.*` key was read by an
// explicit getter and nothing else, so a typo (`workon.review.diff.laoyut`) was silently
// dropped — no warning, no effect, indistinguishable from a setting that simply did nothing.
// `ReviewConfig::unknown_key_warnings` closes that gap with one pass over
// `entries("workon.review.*")`, driven by [`KNOWN_SCALAR_KEYS`] plus the two open-ended
// subspaces (`theme.<slot-or-tint>`, reusing [`slot_index`]/[`tint_slot`]'s existing key
// lists; `<view>.bind.<action>`, reusing [`parse_bind_key`]'s existing decomposition) — see
// [`is_claimed`].

/// Exact `workon.review.<key>` scalar names the registry recognizes — every non-pattern
/// getter's key, suffixed the same way [`ReviewConfig::scalar_key`] builds it.
///
/// This is the drift-prone half of the registry (the pattern arms can't drift: they reuse
/// [`slot_index`]/[`tint_slot`]/[`parse_bind_key`] directly, so there is nothing to keep in
/// sync). Every getter that reads one of these keys builds its query through
/// [`ReviewConfig::scalar_key`], which `debug_assert!`s the suffix is listed here — so adding
/// a new scalar getter without adding its key to this array doesn't silently drop the key
/// (the pre-existing failure mode this whole pass exists to close); it panics the first time
/// ANY test exercises the new getter, in every debug build (which `cargo test` always is),
/// not just a dedicated registry test. See
/// `scalar_getters_route_every_key_through_the_known_key_registry` below for the explicit
/// enumeration this backstops.
const KNOWN_SCALAR_KEYS: &[&str] = &[
    "theme",
    "icons",
    "outline.width",
    "outline.mode",
    "outline.order",
    "diff.layout",
    "diff.zoom",
    "diff.text",
];

/// Whether `key` (a `workon.review.<key>` suffix, e.g. `"outline.width"` or `"theme.base00"`)
/// is claimed by some existing part of the schema, so [`ReviewConfig::unknown_key_warnings`]
/// should stay silent on it. `name` is the fully-qualified key, needed for
/// [`parse_bind_key`]'s own prefix-stripping.
///
/// Claimed does NOT mean valid. `theme.frob` and `diff.bind.made-up-action` are both
/// claimed — their subspace already owns warning about them (`theme_overrides`'s "unknown
/// theme key" warning, `keymap`'s "unknown review keybinding action" warning) — warning again
/// here would double-warn the same typo. Only a shape no subspace recognizes at all (an
/// unlisted scalar name, or a bind typo like `diff.bnid.stage-hunk` that doesn't even parse as
/// a bind entry) falls through to this pass.
fn is_claimed(name: &str, key: &str) -> bool {
    KNOWN_SCALAR_KEYS.contains(&key) || key.starts_with("theme.") || parse_bind_key(name).is_some()
}

/// How close two [`unknown_key_warning`] candidates need to be (by [`levenshtein`] distance)
/// before the nearer one gets suggested — small enough that `laoyut`/`layout` and
/// `thmee`/`theme` (both distance 2) hit, but an unrelated key suggests nothing.
const SUGGESTION_THRESHOLD: usize = 2;

/// Iterative-DP Levenshtein edit distance between two strings — no new dependency for
/// [`unknown_key_warning`]'s "did you mean" suggestion, which only needs this one comparison.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The closest [`KNOWN_SCALAR_KEYS`] entry to `key`, if any entry is within
/// [`SUGGESTION_THRESHOLD`] edits — `None` if the nearest is still too far to plausibly be a
/// typo of a real key.
fn nearest_known_key(key: &str) -> Option<&'static str> {
    KNOWN_SCALAR_KEYS
        .iter()
        .map(|&candidate| (candidate, levenshtein(key, candidate)))
        .filter(|&(_, dist)| dist <= SUGGESTION_THRESHOLD)
        .min_by_key(|&(_, dist)| dist)
        .map(|(candidate, _)| candidate)
}

/// The warning message for a `workon.review.<key>` name no part of the schema claims — see
/// [`is_claimed`]. Suggests the nearest [`KNOWN_SCALAR_KEYS`] entry when one is close enough
/// ([`nearest_known_key`]) to plausibly be what the user meant.
fn unknown_key_warning(key: &str) -> String {
    match nearest_known_key(key) {
        Some(suggestion) => {
            format!("workon.review.{key}: unknown key, ignoring (did you mean '{suggestion}'?)")
        }
        None => format!("workon.review.{key}: unknown key, ignoring"),
    }
}

/// Configuration reader for `workon.review.*` settings stored in git config.
///
/// Mirrors `git-workon-lib`'s `WorkonConfig`: opens the repository's layered config (local >
/// global > system) and exposes typed getters. Unlike `WorkonConfig`, there is no CLI-override
/// precedence here — `git-workon-review`'s CLI takes no relevant flags yet.
pub struct ReviewConfig<'repo> {
    repo: &'repo Repository,
}

impl<'repo> ReviewConfig<'repo> {
    /// Create a new config reader for the given repository.
    pub fn new(repo: &'repo Repository) -> Self {
        Self { repo }
    }

    /// Get `workon.review.theme`, parsed into a [`Theme`]. Defaults to [`Theme::Auto`] if
    /// unset or unrecognized.
    pub fn theme(&self) -> Result<Theme, git2::Error> {
        let config = self.repo.config()?;
        let theme = match config.get_string(&Self::scalar_key("theme")) {
            Ok(raw) => match raw.as_str() {
                "dark" => Theme::Dark,
                "light" => Theme::Light,
                _ => Theme::Auto,
            },
            Err(_) => Theme::Auto,
        };
        Ok(theme)
    }

    /// Read every `workon.review.theme.*` variable — the CS1 user-configurable colors tier (see
    /// [`crate::theme::ThemeOverrides`]) — into a [`ThemeOverrides`] plus any warnings for
    /// malformed values. Deliberately separate from [`ReviewConfig::theme`]: `theme` and
    /// `theme.*` are different subsections (`[workon "review"] theme = dark` vs.
    /// `[workon "review.theme"] base00 = …`) and coexist fine — see the module doc's example.
    ///
    /// Same posture as [`ReviewConfig::bindings`]'s unknown-action handling (no new error
    /// types): an unrecognized key under `workon.review.theme.*` or an unparseable color value
    /// is collected as a warning and the entry is otherwise ignored, not a hard error. A
    /// config-read error collapses to an empty [`ThemeOverrides`] at the call site (`main.rs`),
    /// same as every other getter here.
    pub fn theme_overrides(&self) -> Result<(ThemeOverrides, Vec<String>), git2::Error> {
        let config = self.repo.config()?;
        // Same collect-names-then-read-values shape as `bindings()`: `entries()` borrows
        // `config` and yields one entry per config LAYER a key is set in, so names are
        // deduped first and the precedence-correct value is read via `get_string` afterward.
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        {
            let mut entries = config.entries(Some("workon.review.theme.*"))?;
            while let Some(entry) = entries.next() {
                let entry = entry?;
                let Ok(name) = entry.name() else {
                    continue;
                };
                if seen.insert(name.to_string()) {
                    names.push(name.to_string());
                }
            }
        }

        let mut overrides = ThemeOverrides::default();
        let mut warnings = Vec::new();
        for name in names {
            // `workon.review.theme.*` matched a name that isn't `workon.review.theme.<key>`
            // (can't happen given the glob above, but keeps this total rather than panicking).
            let Some(key) = name.strip_prefix("workon.review.theme.") else {
                continue;
            };
            let Ok(raw) = config.get_string(&name) else {
                continue;
            };

            if let Some(index) = slot_index(key) {
                match theme::parse_hex_color(&raw) {
                    Some(color) => overrides.set_slot(index, color),
                    None => warnings.push(invalid_color_warning(key, &raw)),
                }
                continue;
            }

            match tint_slot(&mut overrides, key) {
                Some(slot) => match theme::parse_hex_color(&raw) {
                    Some(color) => *slot = Some(color),
                    None => warnings.push(invalid_color_warning(key, &raw)),
                },
                None => warnings.push(format!(
                    "workon.review.theme.{key}: unknown theme key, ignoring"
                )),
            }
        }

        Ok((overrides, warnings))
    }

    /// Read every `workon.review.*.bind.*` (and bare `workon.review.bind.*`) variable, raw and
    /// unparsed — **one [`RawBinding`] per (view, action)**. `git2`'s `entries()` surfaces the
    /// same key once per config layer it's set in (a global default AND a local override BOTH
    /// appear as separate entries — unlike `get_string`, which honors precedence). So we dedup by
    /// (view, action) and read the winning value via `get_string`, giving one binding per pair
    /// with git's native precedence (local > global > system) applied.
    ///
    /// Token-grammar parsing (space/reserved-word/modifier/chord), unknown-action validation,
    /// and collision detection are CS2's job — this is the raw read only.
    pub fn bindings(&self) -> Result<Vec<RawBinding>, git2::Error> {
        let config = self.repo.config()?;
        // Gather each (view, action) once with its fully-qualified key name. The `entries()`
        // iterator borrows `config`, so collect names first (deduping shadowed layers), then read
        // precedence-correct values via `get_string` after the iterator is dropped.
        let mut pairs: Vec<(View, String, String)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        {
            let mut entries = config.entries(Some("workon.review.*"))?;
            while let Some(entry) = entries.next() {
                let entry = entry?;
                let Ok(name) = entry.name() else {
                    continue;
                };
                if let Some((view, action)) = parse_bind_key(name) {
                    if seen.insert((view, action.clone())) {
                        pairs.push((view, action, name.to_string()));
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(pairs.len());
        for (view, action, name) in pairs {
            let keys = config.get_string(&name)?;
            out.push(RawBinding { view, action, keys });
        }
        Ok(out)
    }

    /// Get `workon.review.outline.width`, raw. `None` if unset — callers apply the current
    /// hardcoded default (CS7).
    pub fn outline_width(&self) -> Result<Option<i64>, git2::Error> {
        self.get_view_i64(View::Outline, "width")
    }

    /// Get `workon.review.outline.mode`, raw. `None` if unset.
    pub fn outline_mode(&self) -> Result<Option<String>, git2::Error> {
        self.get_view_string(View::Outline, "mode")
    }

    /// Get `workon.review.outline.order`, raw. `None` if unset.
    pub fn outline_order(&self) -> Result<Option<String>, git2::Error> {
        self.get_view_string(View::Outline, "order")
    }

    /// Get `workon.review.icons`, raw. `None` if unset — callers apply the current default
    /// ([`crate::icons::IconMode::None`]; no auto-detection story exists). Top-level like
    /// `theme`, not a view setting: icon mode gates the outline, summary panel, AND winbar.
    pub fn icons(&self) -> Result<Option<String>, git2::Error> {
        let config = self.repo.config()?;
        match config.get_string(&Self::scalar_key("icons")) {
            Ok(val) => Ok(Some(val)),
            Err(_) => Ok(None),
        }
    }

    /// Get `workon.review.diff.layout`, raw. `None` if unset.
    pub fn diff_layout(&self) -> Result<Option<String>, git2::Error> {
        self.get_view_string(View::Diff, "layout")
    }

    /// Get `workon.review.diff.zoom`, raw. `None` if unset.
    pub fn diff_zoom(&self) -> Result<Option<String>, git2::Error> {
        self.get_view_string(View::Diff, "zoom")
    }

    /// Get `workon.review.diff.text`, raw. `None` if unset — see
    /// [ADR-035](../../../docs/adr/035-review-theming-base16-hybrid.md)'s "Revised (CS11, diff
    /// foreground/background split)" section.
    pub fn diff_text(&self) -> Result<Option<String>, git2::Error> {
        self.get_view_string(View::Diff, "text")
    }

    /// Read all four CS7 view-config settings at once into an owned [`RawViewConfig`],
    /// collapsing a config-read error to `None` — same as every other getter here, `App`'s
    /// resolution (`App::apply_view_config`) treats an unset setting and a failed read
    /// identically (both apply the current hardcoded default). Exists so `main.rs` can read
    /// view config into an owned value BEFORE `repo` moves into `App` (mirroring how the
    /// keymap/theme are resolved before the move), rather than holding a `ReviewConfig<'repo>`
    /// (which borrows `repo`) alongside the `App` that owns it.
    pub fn view_config(&self) -> RawViewConfig {
        RawViewConfig {
            outline_width: self.outline_width().ok().flatten(),
            outline_mode: self.outline_mode().ok().flatten(),
            outline_order: self.outline_order().ok().flatten(),
            icons: self.icons().ok().flatten(),
            diff_layout: self.diff_layout().ok().flatten(),
            diff_zoom: self.diff_zoom().ok().flatten(),
            diff_text: self.diff_text().ok().flatten(),
        }
    }

    /// Build the fully-qualified `workon.review.<suffix>` key for a scalar (non-`bind`,
    /// non-`theme.*`) setting, `debug_assert!`ing `suffix` is listed in [`KNOWN_SCALAR_KEYS`] —
    /// see that constant's doc comment for why this assert is the drift guard for Decision 3's
    /// registry.
    fn scalar_key(suffix: &str) -> String {
        debug_assert!(
            KNOWN_SCALAR_KEYS.contains(&suffix),
            "scalar key {suffix:?} read by a ReviewConfig getter but missing from \
             KNOWN_SCALAR_KEYS (config.rs) — add it there, or the new unknown-key \
             validation pass will warn on a key that actually works"
        );
        format!("workon.review.{suffix}")
    }

    /// Build the `workon.review.<view>.<setting>` key for a view setting (never a `.bind.`
    /// entry — [`View::Global`] has no setting namespace, only callers reading `Diff`/`Outline`
    /// use this).
    fn setting_key(view: View, setting: &str) -> String {
        let segment = view
            .as_key_segment()
            .expect("view settings are only read for Diff/Outline, never Global");
        Self::scalar_key(&format!("{segment}.{setting}"))
    }

    fn get_view_string(&self, view: View, setting: &str) -> Result<Option<String>, git2::Error> {
        let config = self.repo.config()?;
        match config.get_string(&Self::setting_key(view, setting)) {
            Ok(val) => Ok(Some(val)),
            Err(_) => Ok(None),
        }
    }

    fn get_view_i64(&self, view: View, setting: &str) -> Result<Option<i64>, git2::Error> {
        let config = self.repo.config()?;
        match config.get_i64(&Self::setting_key(view, setting)) {
            Ok(val) => Ok(Some(val)),
            Err(_) => Ok(None),
        }
    }

    /// Warn on every `workon.review.*` key no part of the schema claims — see this module's
    /// "Unknown-key registry" section doc for the rationale and [`is_claimed`] for exactly what
    /// counts as claimed. Scoped to `workon.review.*` only: `workon.*` at large
    /// (`workon.autocopy`, `workon.copyexclude`, …) belongs to `git-workon-lib`, and this crate
    /// has no business warning about it.
    ///
    /// Same dedup concern as [`ReviewConfig::bindings`]/[`ReviewConfig::theme_overrides`]:
    /// `entries()` yields one entry per config LAYER a key is set in, so a key set in both local
    /// and global config must warn once, not twice — names are deduped before classifying them.
    /// A config-read error yields an empty warning list (same degrade-not-abort posture as
    /// every other getter here); the caller ([`resolve_runtime`]) already treats that the same
    /// as "nothing to warn about".
    pub fn unknown_key_warnings(&self) -> Result<Vec<String>, git2::Error> {
        let config = self.repo.config()?;
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        {
            let mut entries = config.entries(Some("workon.review.*"))?;
            while let Some(entry) = entries.next() {
                let entry = entry?;
                let Ok(name) = entry.name() else {
                    continue;
                };
                if seen.insert(name.to_string()) {
                    names.push(name.to_string());
                }
            }
        }

        let mut warnings = Vec::new();
        for name in names {
            // `workon.review.*` matched a name that isn't `workon.review.<key>` (can't happen
            // given the glob, but keeps this total rather than panicking).
            let Some(key) = name.strip_prefix("workon.review.") else {
                continue;
            };
            if !is_claimed(&name, key) {
                warnings.push(unknown_key_warning(key));
            }
        }
        Ok(warnings)
    }
}

#[cfg(test)]
mod tests {
    use git_workon_fixture::prelude::*;

    use super::*;

    #[test]
    fn theme_defaults_to_auto_when_unset() {
        let fixture = FixtureBuilder::new().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let config = ReviewConfig::new(repo);
        assert_eq!(config.theme().expect("theme"), Theme::Auto);
    }

    #[test]
    fn theme_reads_dark_and_light() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme", "dark")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        assert_eq!(ReviewConfig::new(repo).theme().expect("theme"), Theme::Dark);

        let fixture = FixtureBuilder::new()
            .config("workon.review.theme", "light")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        assert_eq!(
            ReviewConfig::new(repo).theme().expect("theme"),
            Theme::Light
        );
    }

    #[test]
    fn theme_falls_back_to_auto_on_unrecognized_value() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme", "solarized")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        assert_eq!(ReviewConfig::new(repo).theme().expect("theme"), Theme::Auto);
    }

    #[test]
    fn bindings_is_empty_when_unset() {
        let fixture = FixtureBuilder::new().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");
        assert!(ReviewConfig::new(repo)
            .bindings()
            .expect("bindings")
            .is_empty());
    }

    #[test]
    fn bindings_decomposes_view_and_global_keys() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.diff.bind.stage-hunk", "s x")
            .config("workon.review.outline.bind.open", "enter")
            .config("workon.review.bind.quit", "q esc")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let mut bindings = ReviewConfig::new(repo).bindings().expect("bindings");
        bindings.sort_by(|a, b| a.action.cmp(&b.action));

        assert_eq!(
            bindings,
            vec![
                RawBinding {
                    view: View::Outline,
                    action: "open".to_string(),
                    keys: "enter".to_string(),
                },
                RawBinding {
                    view: View::Global,
                    action: "quit".to_string(),
                    keys: "q esc".to_string(),
                },
                RawBinding {
                    view: View::Diff,
                    action: "stage-hunk".to_string(),
                    keys: "s x".to_string(),
                },
            ]
        );
    }

    #[test]
    fn bindings_ignores_non_binding_keys() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme", "dark")
            .config("workon.review.outline.width", "32")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        assert!(ReviewConfig::new(repo)
            .bindings()
            .expect("bindings")
            .is_empty());
    }

    #[test]
    fn bindings_dedups_a_key_set_in_multiple_layers_to_the_winning_value() {
        // `git2`'s entries() surfaces a key once per config layer it's set in; bindings() must
        // emit ONE RawBinding per (view, action), carrying the precedence-correct (get_string)
        // value — not one-per-layer with a shadowed value. Simulate multiple layers with a
        // multivar on the local config (two values for one key), which entries() likewise yields
        // as two entries and get_string resolves to the last.
        let fixture = FixtureBuilder::new().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let cfg_path = repo.path().join("config");
        for v in ["q", "x"] {
            let status = std::process::Command::new("git")
                .args([
                    "config",
                    "--file",
                    cfg_path.to_str().expect("config path utf8"),
                    "--add",
                    "workon.review.bind.quit",
                    v,
                ])
                .status()
                .expect("git config --add");
            assert!(status.success(), "git config --add failed");
        }

        let bindings = ReviewConfig::new(repo).bindings().expect("bindings");
        let quit: Vec<_> = bindings
            .iter()
            .filter(|b| b.view == View::Global && b.action == "quit")
            .collect();
        assert_eq!(
            quit.len(),
            1,
            "one binding per (view, action), not one per config layer; got {bindings:?}"
        );
        assert_eq!(
            quit[0].keys, "x",
            "get_string resolves the multivar to its last/winning value"
        );
    }

    #[test]
    fn view_settings_read_when_set() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.outline.width", "40")
            .config("workon.review.outline.mode", "tree")
            .config("workon.review.outline.order", "base-first")
            .config("workon.review.icons", "nerd")
            .config("workon.review.diff.layout", "split")
            .config("workon.review.diff.zoom", "staged")
            .config("workon.review.diff.text", "tint")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let config = ReviewConfig::new(repo);

        assert_eq!(config.outline_width().expect("width"), Some(40));
        assert_eq!(
            config.outline_mode().expect("mode"),
            Some("tree".to_string())
        );
        assert_eq!(
            config.outline_order().expect("order"),
            Some("base-first".to_string())
        );
        assert_eq!(config.icons().expect("icons"), Some("nerd".to_string()));
        assert_eq!(
            config.diff_layout().expect("layout"),
            Some("split".to_string())
        );
        assert_eq!(
            config.diff_zoom().expect("zoom"),
            Some("staged".to_string())
        );
        assert_eq!(config.diff_text().expect("text"), Some("tint".to_string()));
    }

    #[test]
    fn view_settings_default_to_none_when_unset() {
        let fixture = FixtureBuilder::new().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let config = ReviewConfig::new(repo);

        assert_eq!(config.outline_width().expect("width"), None);
        assert_eq!(config.outline_mode().expect("mode"), None);
        assert_eq!(config.outline_order().expect("order"), None);
        assert_eq!(config.icons().expect("icons"), None);
        assert_eq!(config.diff_layout().expect("layout"), None);
        assert_eq!(config.diff_zoom().expect("zoom"), None);
        assert_eq!(config.diff_text().expect("text"), None);
    }

    #[test]
    fn theme_overrides_is_empty_when_unset() {
        let fixture = FixtureBuilder::new().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(overrides.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn theme_overrides_reads_slot_and_tint_keys() {
        use crate::theme::Palette;

        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.base00", "#101010")
            .config("workon.review.theme.cursor-bg", "1a2b3c")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(!overrides.is_empty());

        let mut palette = Palette::dark();
        palette.apply_overrides(&overrides);
        assert_eq!(palette.background, Color::Rgb(0x10, 0x10, 0x10));
        assert_eq!(overrides.cursor_bg, Some(Color::Rgb(0x1a, 0x2b, 0x3c)));
    }

    #[test]
    fn theme_overrides_reads_the_cursor_unfocused_bg_tint_key() {
        use crate::theme::Palette;

        // CS1 (`unfocused-cursor-wash`): `cursor-unfocused-bg` replaced the outline-only
        // `outline-cursor-unfocused-bg` key with no backward compatibility — see the sibling
        // rejection test below for the dropped old key.
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.cursor-unfocused-bg", "#1a2b3c")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            overrides.cursor_unfocused_bg,
            Some(Color::Rgb(0x1a, 0x2b, 0x3c))
        );

        let mut palette = Palette::dark();
        palette.apply_overrides(&overrides);
        assert_eq!(palette.cursor_unfocused_bg, Color::Rgb(0x1a, 0x2b, 0x3c));
    }

    #[test]
    fn theme_overrides_rejects_the_dropped_outline_cursor_unfocused_bg_key() {
        // The pre-rename key must NOT resolve as a compat alias — it's just an unknown key now,
        // warned and ignored exactly like any other unrecognized `workon.review.theme.*` name.
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.outline-cursor-unfocused-bg", "#1a2b3c")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(overrides.is_empty(), "dropped key must not set any field");
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(warnings[0].contains("outline-cursor-unfocused-bg"));
    }

    #[test]
    fn theme_overrides_reads_the_pane_header_focused_fg_tint_key() {
        use crate::theme::Palette;

        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.pane-header-focused-fg", "#c0ffee")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            overrides.pane_header_focused_fg,
            Some(Color::Rgb(0xc0, 0xff, 0xee))
        );

        let mut palette = Palette::dark();
        palette.apply_overrides(&overrides);
        assert_eq!(palette.pane_header_focused_fg, Color::Rgb(0xc0, 0xff, 0xee));
    }

    #[test]
    fn theme_overrides_reads_the_filler_fg_tint_key() {
        use crate::theme::Palette;

        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.filler-fg", "#403a48")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(overrides.filler_fg, Some(Color::Rgb(0x40, 0x3a, 0x48)));

        let mut palette = Palette::dark();
        palette.apply_overrides(&overrides);
        assert_eq!(palette.filler_fg, Color::Rgb(0x40, 0x3a, 0x48));
    }

    #[test]
    fn theme_overrides_slot_keys_are_case_insensitive() {
        use crate::theme::Palette;

        // Git lowercases config variable names on write, so `BASE0A` in the fixture's config
        // arrives back as `base0a` — this proves the whole path (fixture write → git2 read →
        // our slot_index) lands in slot 10 (base0A → warn_fg), not that our own code
        // case-folds anything.
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.BASE0A", "#c1c1c1")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

        let mut palette = Palette::dark();
        palette.apply_overrides(&overrides);
        assert_eq!(palette.warn_fg, Color::Rgb(0xc1, 0xc1, 0xc1));
    }

    #[test]
    fn theme_overrides_warns_and_ignores_an_invalid_hex_value() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.base00", "not-a-color")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(overrides.is_empty(), "invalid value must not set the slot");
        assert_eq!(warnings.len(), 1);
        // Full-message pin (config-validation-completeness Decision 5): names the expected
        // format, and `ignoring` with no fallback — an ignored override has none.
        assert_eq!(
            warnings[0],
            "workon.review.theme.base00: invalid color \"not-a-color\" \
             (expected #rrggbb or rrggbb); ignoring"
        );
    }

    #[test]
    fn theme_overrides_warns_on_unknown_keys() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.base10", "#101010") // no slot 16
            .config("workon.review.theme.cursorbg", "#101010") // misspelled tint key
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let (overrides, warnings) = ReviewConfig::new(repo)
            .theme_overrides()
            .expect("theme_overrides");
        assert!(overrides.is_empty());
        assert_eq!(warnings.len(), 2, "got: {warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("base10")));
        assert!(warnings.iter().any(|w| w.contains("cursorbg")));
    }

    // ── `resolve_runtime` (the shared startup/reload structural core) ──────────

    fn auto_ctx() -> PaletteContext {
        PaletteContext {
            auto_base: Palette::dark(),
            no_color: false,
        }
    }

    #[test]
    fn resolve_runtime_applies_theme_overrides_on_top_of_the_auto_base_for_theme_auto() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.base00", "#101010")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let runtime = resolve_runtime(repo, &auto_ctx());
        assert_eq!(runtime.palette.background, Color::Rgb(0x10, 0x10, 0x10));
        // Every other field still traces back to `auto_base` (`Palette::dark()`), not some other
        // base — spot-check one untouched field.
        assert_eq!(runtime.palette.dim, Palette::dark().dim);
        assert!(runtime.warnings.is_empty());
    }

    #[test]
    fn resolve_runtime_honors_dark_and_light_selection() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme", "light")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        // A distinctive `auto_base` proves `theme = light` ignores it entirely, rather than
        // falling through to the cached probe base.
        let runtime = resolve_runtime(
            repo,
            &PaletteContext {
                auto_base: Palette::mono(false),
                no_color: false,
            },
        );
        assert_eq!(runtime.palette.background, Palette::light().background);
    }

    #[test]
    fn resolve_runtime_no_color_yields_a_mono_palette_no_override_can_recolor() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.base00", "#101010")
            .config("workon.review.theme.cursor-bg", "#1a2b3c")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let runtime = resolve_runtime(
            repo,
            &PaletteContext {
                auto_base: Palette::dark(),
                no_color: true,
            },
        );
        assert!(
            runtime.palette.colorless,
            "NO_COLOR must win over any override"
        );
        assert_ne!(
            runtime.palette.background,
            Color::Rgb(0x10, 0x10, 0x10),
            "the base00 override must not survive the mono substitution"
        );
        assert_ne!(
            runtime.palette.cursor_bg,
            Color::Rgb(0x1a, 0x2b, 0x3c),
            "the cursor-bg override must not survive the mono substitution"
        );
    }

    #[test]
    fn resolve_runtime_degrades_on_a_config_read_error_instead_of_panicking() {
        // Corrupt `.git/config` with unparseable syntax so every `repo.config()?` call inside
        // `resolve_runtime`'s ladder fails — the degrade-not-abort posture every getter in this
        // module already has (see the module doc comment), exercised end-to-end here rather than
        // per-getter.
        let fixture = FixtureBuilder::new().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let cfg_path = repo.path().join("config");
        std::fs::write(&cfg_path, "[this is not valid gitconfig\n").expect("corrupt config");

        let runtime = resolve_runtime(repo, &auto_ctx());
        assert!(
            runtime.keymap.warnings().is_empty(),
            "defaults, not warnings"
        );
        assert_eq!(runtime.palette.background, Palette::dark().background);
        assert!(runtime.warnings.is_empty());
        assert_eq!(runtime.view_config, RawViewConfig::default());
    }

    #[test]
    fn theme_overrides_coexists_with_the_theme_selection() {
        // `[workon "review"] theme = dark` and `[workon "review.theme"] base00 = …` are
        // different subsections — both must read fine, independently.
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme", "dark")
            .config("workon.review.theme.base00", "#101010")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let config = ReviewConfig::new(repo);

        assert_eq!(config.theme().expect("theme"), Theme::Dark);
        let (overrides, warnings) = config.theme_overrides().expect("theme_overrides");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(!overrides.is_empty());
    }

    // ── unknown-key registry (config validation completeness) ──────────────────

    #[test]
    fn unknown_key_warnings_is_empty_when_unset() {
        let fixture = FixtureBuilder::new().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");
        assert!(ReviewConfig::new(repo)
            .unknown_key_warnings()
            .expect("unknown_key_warnings")
            .is_empty());
    }

    #[test]
    fn unknown_key_warnings_flags_a_typo_in_a_scalar_key() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.diff.laoyut", "split")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let warnings = ReviewConfig::new(repo)
            .unknown_key_warnings()
            .expect("unknown_key_warnings");
        assert_eq!(
            warnings,
            vec![
                "workon.review.diff.laoyut: unknown key, ignoring (did you mean 'diff.layout'?)"
                    .to_string()
            ]
        );
    }

    #[test]
    fn unknown_key_warnings_suggests_nothing_for_an_unrelated_key() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.zzzzzzzz", "1")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let warnings = ReviewConfig::new(repo)
            .unknown_key_warnings()
            .expect("unknown_key_warnings");
        assert_eq!(
            warnings,
            vec!["workon.review.zzzzzzzz: unknown key, ignoring".to_string()]
        );
    }

    #[test]
    fn unknown_key_warnings_stays_silent_on_an_unrecognized_theme_key() {
        // `theme.*` unknown keys already warn in `theme_overrides` — the registry pass must
        // treat the whole `theme.*` shape as claimed, or a bad theme key double-warns.
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme.cursorbg", "#101010") // misspelled tint key
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let config = ReviewConfig::new(repo);

        assert!(config
            .unknown_key_warnings()
            .expect("unknown_key_warnings")
            .is_empty());
        let (_, theme_warnings) = config.theme_overrides().expect("theme_overrides");
        assert_eq!(
            theme_warnings.len(),
            1,
            "theme_overrides should still be the one place that warns: {theme_warnings:?}"
        );
    }

    #[test]
    fn unknown_key_warnings_stays_silent_on_an_unrecognized_bind_action() {
        // Unknown bind ACTIONS already warn in `keymap` — the registry pass only cares that
        // the shape parses as a bind entry (`parse_bind_key`), not that the action is real.
        let fixture = FixtureBuilder::new()
            .config("workon.review.diff.bind.made-up-action", "x")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        assert!(ReviewConfig::new(repo)
            .unknown_key_warnings()
            .expect("unknown_key_warnings")
            .is_empty());
    }

    #[test]
    fn unknown_key_warnings_flags_a_malformed_bind_shape() {
        // A typo of `bind` itself (`bnid`) doesn't parse as a bind entry at all — it's an
        // unknown key, not a bind-action problem, and `keymap` never sees it (it only iterates
        // `ReviewConfig::bindings()`, which never yields this entry).
        let fixture = FixtureBuilder::new()
            .config("workon.review.diff.bnid.stage-hunk", "s")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let warnings = ReviewConfig::new(repo)
            .unknown_key_warnings()
            .expect("unknown_key_warnings");
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(warnings[0].contains("diff.bnid.stage-hunk"));
    }

    #[test]
    fn unknown_key_warnings_dedups_a_key_set_in_multiple_layers() {
        // Same layering concern as `bindings_dedups_a_key_set_in_multiple_layers_to_the_
        // winning_value`: `entries()` yields one entry per config LAYER, not one per key.
        let fixture = FixtureBuilder::new()
            .config("workon.review.bogus-key", "a")
            .config("workon.review.bogus-key", "b")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let warnings = ReviewConfig::new(repo)
            .unknown_key_warnings()
            .expect("unknown_key_warnings");
        assert_eq!(
            warnings.len(),
            1,
            "one warning per key, not one per config layer; got {warnings:?}"
        );
    }

    #[test]
    fn unknown_key_warnings_is_empty_for_a_fixture_setting_one_of_every_documented_key() {
        // The false-positive gate: a validation pass that cries wolf on working config is worse
        // than no validation at all. Sets every KNOWN_SCALAR_KEYS entry, a theme slot, a theme
        // tint, a global bind, and a per-view bind — none of it should warn.
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme", "dark")
            .config("workon.review.icons", "nerd")
            .config("workon.review.outline.width", "40")
            .config("workon.review.outline.mode", "tree")
            .config("workon.review.outline.order", "base-first")
            .config("workon.review.diff.layout", "split")
            .config("workon.review.diff.zoom", "staged")
            .config("workon.review.diff.text", "tint")
            .config("workon.review.theme.base00", "#101010") // a theme slot
            .config("workon.review.theme.cursor-bg", "#1a2b3c") // a theme tint
            .config("workon.review.bind.quit", "q esc") // a global bind
            .config("workon.review.diff.bind.stage-hunk", "s x") // a per-view bind
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let warnings = ReviewConfig::new(repo)
            .unknown_key_warnings()
            .expect("unknown_key_warnings");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    /// Decision 3's drift test. `KNOWN_SCALAR_KEYS` is a second source of truth alongside the
    /// getters that actually read `workon.review.*` — this enumerates every scalar getter, sets
    /// its documented key on a fixture, and asserts BOTH that the getter reads it back AND that
    /// the same key is claimed by the registry (`is_claimed`), so a getter added without a
    /// matching registry entry fails here (not just "the registry agrees with itself"). The
    /// `scalar_key` `debug_assert!` backs this up structurally: even a getter this list forgets
    /// to enumerate would panic the moment ANY test exercises it, not just this one.
    #[test]
    fn scalar_getters_route_every_key_through_the_known_key_registry() {
        let fixture = FixtureBuilder::new()
            .config("workon.review.theme", "dark")
            .config("workon.review.icons", "nerd")
            .config("workon.review.outline.width", "40")
            .config("workon.review.outline.mode", "tree")
            .config("workon.review.outline.order", "base-first")
            .config("workon.review.diff.layout", "split")
            .config("workon.review.diff.zoom", "staged")
            .config("workon.review.diff.text", "tint")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let config = ReviewConfig::new(repo);

        let probes: Vec<(&str, bool)> = vec![
            ("theme", config.theme().is_ok()),
            ("icons", config.icons().expect("icons").is_some()),
            (
                "outline.width",
                config.outline_width().expect("width").is_some(),
            ),
            (
                "outline.mode",
                config.outline_mode().expect("mode").is_some(),
            ),
            (
                "outline.order",
                config.outline_order().expect("order").is_some(),
            ),
            (
                "diff.layout",
                config.diff_layout().expect("layout").is_some(),
            ),
            ("diff.zoom", config.diff_zoom().expect("zoom").is_some()),
            ("diff.text", config.diff_text().expect("text").is_some()),
        ];

        for (key, getter_saw_it) in &probes {
            assert!(
                *getter_saw_it,
                "getter for {key:?} did not read its own fixture value"
            );
            assert!(
                KNOWN_SCALAR_KEYS.contains(key),
                "{key:?} is read by a getter but missing from KNOWN_SCALAR_KEYS — it would \
                 warn as unknown while working correctly"
            );
        }
        assert_eq!(
            probes.len(),
            KNOWN_SCALAR_KEYS.len(),
            "a scalar getter was added without a matching probe above (or vice versa) — \
             update this list alongside KNOWN_SCALAR_KEYS"
        );
    }
}
