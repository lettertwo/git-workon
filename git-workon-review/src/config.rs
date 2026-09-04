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
//!
//! [workon "review.diff.bind"]
//!   stage-hunk = s x           ; action = key tokens (space-separated)
//!
//! [workon "review.outline.bind"]
//!   open = enter
//!
//! [workon "review.bind"]
//!   quit = q esc               ; bare `review.bind` = global view (active in every view)
//!
//! [workon "review.outline"]
//!   width = 32
//!   mode = tree
//!
//! [workon "review.diff"]
//!   layout = split
//!   zoom = combined
//! ```

use git2::Repository;

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
    pub diff_layout: Option<String>,
    pub diff_zoom: Option<String>,
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
        let theme = match config.get_string("workon.review.theme") {
            Ok(raw) => match raw.as_str() {
                "dark" => Theme::Dark,
                "light" => Theme::Light,
                _ => Theme::Auto,
            },
            Err(_) => Theme::Auto,
        };
        Ok(theme)
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

    /// Get `workon.review.diff.layout`, raw. `None` if unset.
    pub fn diff_layout(&self) -> Result<Option<String>, git2::Error> {
        self.get_view_string(View::Diff, "layout")
    }

    /// Get `workon.review.diff.zoom`, raw. `None` if unset.
    pub fn diff_zoom(&self) -> Result<Option<String>, git2::Error> {
        self.get_view_string(View::Diff, "zoom")
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
            diff_layout: self.diff_layout().ok().flatten(),
            diff_zoom: self.diff_zoom().ok().flatten(),
        }
    }

    /// Build the `workon.review.<view>.<setting>` key for a view setting (never a `.bind.`
    /// entry — [`View::Global`] has no setting namespace, only callers reading `Diff`/`Outline`
    /// use this).
    fn setting_key(view: View, setting: &str) -> String {
        let segment = view
            .as_key_segment()
            .expect("view settings are only read for Diff/Outline, never Global");
        format!("workon.review.{segment}.{setting}")
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
            .config("workon.review.diff.layout", "split")
            .config("workon.review.diff.zoom", "staged")
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
            config.diff_layout().expect("layout"),
            Some("split".to_string())
        );
        assert_eq!(
            config.diff_zoom().expect("zoom"),
            Some("staged".to_string())
        );
    }

    #[test]
    fn view_settings_default_to_none_when_unset() {
        let fixture = FixtureBuilder::new().build().expect("fixture build");
        let repo = fixture.repo().expect("repo");
        let config = ReviewConfig::new(repo);

        assert_eq!(config.outline_width().expect("width"), None);
        assert_eq!(config.outline_mode().expect("mode"), None);
        assert_eq!(config.diff_layout().expect("layout"), None);
        assert_eq!(config.diff_zoom().expect("zoom"), None);
    }
}
