//! Action registry, token-grammar parser, and per-view keymap resolution for the review TUI.
//!
//! Replaces `tui.rs`'s formerly-hardcoded `map_key` match with a registry-driven, git-config
//! overridable dispatch (see [ADR-028](../../../docs/adr/028-review-git-native-config-schema.md)).
//!
//! The pieces:
//! - [`Command`] — the enumerable set of *rebindable* actions. This is the compatibility surface
//!   ADR-028 calls out: each variant maps to a stable git-config action name (`stage-hunk`,
//!   `next-file`, …) in a [`View`] namespace, plus a default key-token string and a human
//!   description. The static [`REGISTRY`] table is the single source of truth for all three.
//! - [`parse_value`] — the token-grammar parser: a space-separated config value becomes a set of
//!   alternative key *sequences* (a chord like `]f` is one two-key sequence; `j down` is two
//!   single-key alternatives). Reserved symbolic names win over literals.
//! - [`Keymap`] — resolves the registry defaults against a repo's [`RawBinding`]s (a git entry
//!   overrides that action's default; an empty value unbinds), builds per-view
//!   sequence→command lookup lists, and drives dispatch through [`Keymap::advance`]. Unknown
//!   action names, same-view key collisions, and prefix clashes (a complete binding that's a
//!   strict prefix of another, leaving the shorter one unreachable) are collected as
//!   [`Keymap::warnings`].
//!
//! **Not handled here** (stays hardcoded in `tui.rs`): the confirm modal (`y`/`n`/`Esc`) and the
//! whole `Esc`-precedence cascade (confirm > help > selection-cancel > outline-focused-quit >
//! focus-outline > quit — see `tui::update`'s doc comment). Per ADR-028 those are conventional
//! and safety-sensitive; they are never routed through the registry, so `Esc` is not a registry
//! token.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{RawBinding, View};
use crate::outline::OutlineMode;

/// One rebindable action. The action *identity* — distinct from `tui.rs`'s `Action`, which is the
/// concrete effect applied to the `App` (and carries runtime data like a half-page scroll delta
/// that depends on the pane height at dispatch time). `tui.rs` converts a resolved [`Command`]
/// into its `Action`.
///
/// Every variant appears exactly once in [`REGISTRY`], which pins its config name, view, default
/// keys, and description. Renaming a variant's config name is a breaking change to users' git
/// config (ADR-028's compatibility-surface consequence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    // Global (active in every view).
    Quit,
    ToggleOutline,
    ToggleHelp,
    ReloadConfig,
    // Diff view.
    CursorDown,
    CursorUp,
    HalfPageDown,
    HalfPageUp,
    ScrollTop,
    ScrollBottom,
    ToggleLayout,
    CycleZoom,
    ToggleSplitFocus,
    Refresh,
    StageHunk,
    StageFile,
    DiscardHunk,
    DiscardFile,
    StartSelection,
    NextFile,
    PrevFile,
    NextHunk,
    PrevHunk,
    NextChangeset,
    PrevChangeset,
    ExpandGap,
    ExpandGapAll,
    ResetGaps,
    ExpandAllGaps,
    HscrollLeft,
    HscrollRight,
    Search,
    SearchNext,
    SearchPrev,
    CopyLines,
    CopyLocation,
    // Diff view.
    FocusOutline,
    // Outline view.
    OutlineDown,
    OutlineUp,
    OutlineConfirm,
    OutlineCycleMode,
    FocusDiff,
    OutlineTop,
    OutlineBottom,
    OutlineStage,
    OutlineDiscard,
    OutlineHscrollLeft,
    OutlineHscrollRight,
    OutlineNextChangeset,
    OutlinePrevChangeset,
    OutlineCollapseAll,
    OutlineExpandAll,
    OutlineFilter,
}

/// One row of the action registry: a [`Command`] with its stable config identity (`view` +
/// `name`), the default key tokens that reproduce the pre-config hardcoded binding, and a human
/// description (the help overlay in CS3 renders from this).
#[derive(Debug, Clone, Copy)]
pub struct Registered {
    pub command: Command,
    pub view: View,
    pub name: &'static str,
    pub default_keys: &'static str,
    pub description: &'static str,
}

/// The action registry — the single source of truth for defaults, config names, and help text.
///
/// Order is load-bearing in two ways: **global entries come first** so that on a key collision
/// between a global and a per-view action the global wins (preserving `o`/`q` always working), and
/// within a view earlier entries win later ones (the documented deterministic collision rule). The
/// `default_keys` strings reproduce `tui.rs`'s exact pre-ADR-028 bindings — `Esc` is deliberately
/// absent (it stays hardcoded, see the module doc).
pub static REGISTRY: &[Registered] = &[
    // ── Global ───────────────────────────────────────────────────────────────
    Registered {
        command: Command::Quit,
        view: View::Global,
        name: "quit",
        default_keys: "q",
        description: "Quit the review",
    },
    Registered {
        command: Command::ToggleOutline,
        view: View::Global,
        name: "toggle-outline",
        default_keys: "o",
        description: "Show or hide the outline pane",
    },
    Registered {
        command: Command::ToggleHelp,
        view: View::Global,
        name: "toggle-help",
        default_keys: "?",
        description: "Toggle the help overlay",
    },
    Registered {
        command: Command::ReloadConfig,
        view: View::Global,
        name: "reload-config",
        default_keys: "R",
        description: "Reload config (theme, keys, view settings)",
    },
    // ── Diff view ────────────────────────────────────────────────────────────
    Registered {
        command: Command::CursorDown,
        view: View::Diff,
        name: "cursor-down",
        default_keys: "j down",
        description: "Move cursor down one line",
    },
    Registered {
        command: Command::CursorUp,
        view: View::Diff,
        name: "cursor-up",
        default_keys: "k up",
        description: "Move cursor up one line",
    },
    Registered {
        command: Command::HalfPageDown,
        view: View::Diff,
        name: "half-page-down",
        default_keys: "ctrl-d",
        description: "Scroll down half a page",
    },
    Registered {
        command: Command::HalfPageUp,
        view: View::Diff,
        name: "half-page-up",
        default_keys: "ctrl-u",
        description: "Scroll up half a page",
    },
    Registered {
        command: Command::ScrollTop,
        view: View::Diff,
        name: "scroll-top",
        default_keys: "g",
        description: "Jump to the top",
    },
    Registered {
        command: Command::ScrollBottom,
        view: View::Diff,
        name: "scroll-bottom",
        default_keys: "G",
        description: "Jump to the bottom",
    },
    Registered {
        command: Command::ToggleLayout,
        view: View::Diff,
        name: "toggle-layout",
        default_keys: "L",
        description: "Toggle side-by-side / inline layout",
    },
    Registered {
        command: Command::CycleZoom,
        view: View::Diff,
        name: "cycle-zoom",
        // Rebound from `z` (diff-fold-keys): `z` now anchors the `zM`/`zR` gap fold-all chords in
        // this view, and a bare-key binding can't coexist with a longer chord sharing its prefix
        // (see `shift_z_dispatches_cycle_zoom_with_no_collisions`'s doc comment for the
        // mechanics; `build_context`'s prefix-clash check would now warn on this, not just
        // silently break dispatch). `Z` was free in `View::Diff`.
        default_keys: "Z",
        description: "Cycle the staged/unstaged zoom",
    },
    Registered {
        command: Command::ToggleSplitFocus,
        view: View::Diff,
        name: "toggle-split-focus",
        default_keys: "w",
        description: "Switch focus between split panes",
    },
    Registered {
        command: Command::Refresh,
        view: View::Diff,
        name: "refresh",
        default_keys: "r",
        description: "Refresh the review",
    },
    Registered {
        command: Command::StageHunk,
        view: View::Diff,
        name: "stage-hunk",
        default_keys: "s",
        description: "Stage the hunk (or selection) under the cursor",
    },
    Registered {
        command: Command::StageFile,
        view: View::Diff,
        name: "stage-file",
        default_keys: "S",
        description: "Stage the whole file",
    },
    Registered {
        command: Command::DiscardHunk,
        view: View::Diff,
        name: "discard-hunk",
        default_keys: "d",
        description: "Discard the hunk (or selection) under the cursor",
    },
    Registered {
        command: Command::DiscardFile,
        view: View::Diff,
        name: "discard-file",
        default_keys: "D",
        description: "Discard the whole file",
    },
    Registered {
        command: Command::StartSelection,
        view: View::Diff,
        name: "start-selection",
        default_keys: "v",
        description: "Start a line selection",
    },
    Registered {
        command: Command::NextFile,
        view: View::Diff,
        name: "next-file",
        default_keys: "tab ]f",
        description: "Go to the next file",
    },
    Registered {
        command: Command::PrevFile,
        view: View::Diff,
        name: "prev-file",
        default_keys: "backtab [f",
        description: "Go to the previous file",
    },
    Registered {
        command: Command::NextHunk,
        view: View::Diff,
        name: "next-hunk",
        // `n` moved off this default (M11 CS3, `diff-search`): it's now `search-next`'s default,
        // which itself falls back to this exact action when no search is active — see that row's
        // description. `]h` alone still reaches it directly.
        default_keys: "]h",
        description: "Go to the next hunk",
    },
    Registered {
        command: Command::PrevHunk,
        view: View::Diff,
        name: "prev-hunk",
        default_keys: "[h p",
        description: "Go to the previous hunk",
    },
    Registered {
        command: Command::NextChangeset,
        view: View::Diff,
        name: "next-changeset",
        default_keys: "]c",
        description: "Go to the next changeset",
    },
    Registered {
        command: Command::PrevChangeset,
        view: View::Diff,
        name: "prev-changeset",
        default_keys: "[c",
        description: "Go to the previous changeset",
    },
    Registered {
        command: Command::FocusOutline,
        view: View::Diff,
        name: "focus-outline",
        default_keys: "h left",
        description: "Focus the outline (pans the diff back to column 0 first, if panned)",
    },
    Registered {
        command: Command::HscrollLeft,
        view: View::Diff,
        name: "hscroll-left",
        default_keys: "<",
        description: "Pan the diff content left",
    },
    Registered {
        command: Command::HscrollRight,
        view: View::Diff,
        name: "hscroll-right",
        default_keys: "> l right",
        description: "Pan the diff content right",
    },
    Registered {
        command: Command::ExpandGap,
        view: View::Diff,
        name: "expand-gap",
        default_keys: "enter",
        description: "Reveal more of the collapsed gap under the cursor",
    },
    Registered {
        command: Command::ExpandGapAll,
        view: View::Diff,
        name: "expand-gap-all",
        default_keys: "E",
        description: "Reveal the whole collapsed gap under the cursor",
    },
    Registered {
        command: Command::ResetGaps,
        view: View::Diff,
        name: "reset-gaps",
        default_keys: "zM",
        description: "Collapse all gaps back to the initial view",
    },
    Registered {
        command: Command::ExpandAllGaps,
        view: View::Diff,
        name: "expand-all-gaps",
        default_keys: "zR",
        description: "Reveal every collapsed gap in the file",
    },
    Registered {
        command: Command::Search,
        view: View::Diff,
        name: "search",
        default_keys: "/",
        description: "Search within the current file",
    },
    Registered {
        command: Command::SearchNext,
        view: View::Diff,
        name: "search-next",
        default_keys: "n",
        description: "Next search match (or next hunk, when no search is active)",
    },
    Registered {
        command: Command::SearchPrev,
        view: View::Diff,
        name: "search-prev",
        default_keys: "N",
        description: "Previous search match (or previous hunk, when no search is active)",
    },
    Registered {
        command: Command::CopyLines,
        view: View::Diff,
        name: "copy-lines",
        default_keys: "y",
        description: "Copy the selected (or cursor) lines' text to the clipboard",
    },
    Registered {
        command: Command::CopyLocation,
        view: View::Diff,
        name: "copy-location",
        default_keys: "Y",
        description: "Copy path:line (or path:lo-hi) for the selected rows to the clipboard",
    },
    // ── Outline view ─────────────────────────────────────────────────────────
    Registered {
        command: Command::OutlineDown,
        view: View::Outline,
        name: "cursor-down",
        default_keys: "j down",
        description: "Move the outline cursor down",
    },
    Registered {
        command: Command::OutlineUp,
        view: View::Outline,
        name: "cursor-up",
        default_keys: "k up",
        description: "Move the outline cursor up",
    },
    Registered {
        command: Command::OutlineConfirm,
        view: View::Outline,
        name: "open",
        default_keys: "enter",
        description: "Jump to a file, or fold/unfold a header or directory",
    },
    Registered {
        command: Command::OutlineCycleMode,
        view: View::Outline,
        name: "cycle-mode",
        default_keys: "i",
        description:
            "Cycle the outline mode (stack \u{25b8} stack-tree \u{25b8} flat \u{25b8} tree)",
    },
    Registered {
        command: Command::FocusDiff,
        view: View::Outline,
        name: "focus-diff",
        default_keys: "l right",
        description: "Focus the diff view",
    },
    Registered {
        command: Command::OutlineTop,
        view: View::Outline,
        name: "scroll-top",
        default_keys: "g",
        description: "Jump to the top of the outline",
    },
    Registered {
        command: Command::OutlineBottom,
        view: View::Outline,
        name: "scroll-bottom",
        default_keys: "G",
        description: "Jump to the bottom of the outline",
    },
    Registered {
        command: Command::OutlineStage,
        view: View::Outline,
        name: "stage",
        default_keys: "s",
        description: "Stage or unstage the file/directory under the cursor",
    },
    Registered {
        command: Command::OutlineDiscard,
        view: View::Outline,
        name: "discard",
        default_keys: "d",
        description: "Discard the file/directory under the cursor",
    },
    Registered {
        command: Command::OutlineHscrollLeft,
        view: View::Outline,
        name: "outline-hscroll-left",
        default_keys: "<",
        description: "Pan the outline left",
    },
    Registered {
        command: Command::OutlineHscrollRight,
        view: View::Outline,
        name: "outline-hscroll-right",
        default_keys: ">",
        description: "Pan the outline right",
    },
    Registered {
        command: Command::OutlineNextChangeset,
        view: View::Outline,
        name: "outline-next-changeset",
        default_keys: "n",
        description: "Jump to the next changeset",
    },
    Registered {
        command: Command::OutlinePrevChangeset,
        view: View::Outline,
        name: "outline-prev-changeset",
        default_keys: "p",
        description: "Jump to the previous changeset",
    },
    Registered {
        command: Command::OutlineCollapseAll,
        view: View::Outline,
        name: "outline-collapse-all",
        default_keys: "zM",
        description: "Collapse every changeset/directory in the outline",
    },
    Registered {
        command: Command::OutlineExpandAll,
        view: View::Outline,
        name: "outline-expand-all",
        default_keys: "zR",
        description: "Expand every changeset/directory in the outline",
    },
    Registered {
        command: Command::OutlineFilter,
        view: View::Outline,
        name: "filter",
        default_keys: "/",
        description: "Fuzzy-filter the outline",
    },
];

/// One matchable key press: a [`KeyCode`] plus whether Ctrl/Alt are required. **Shift is
/// deliberately not tracked** — an uppercase literal (`G`, `S`) already carries the shift in its
/// char, and crossterm is inconsistent about setting the modifier for it (and for `BackTab`), so
/// matching ignores it. Ctrl/Alt are the only load-bearing modifiers in the token grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPress {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

impl KeyPress {
    /// Normalize an incoming crossterm [`KeyEvent`] into a matchable [`KeyPress`], dropping Shift
    /// (see the type's doc comment).
    pub fn from_event(event: KeyEvent) -> Self {
        KeyPress {
            code: event.code,
            ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
            alt: event.modifiers.contains(KeyModifiers::ALT),
        }
    }
}

/// A single trigger: one key ([`j`]) or an ordered multi-key chord (`]f` → `]` then `f`). An
/// action may have several alternatives (`j` *or* `down`), each its own [`KeySeq`].
pub type KeySeq = Vec<KeyPress>;

/// Map a reserved symbolic token name to its [`KeyCode`]. These win over literal interpretation
/// (ADR-028: `space` is always the spacebar, never a chord of `s p a c e`). `None` for anything
/// not a reserved name.
fn reserved_code(name: &str) -> Option<KeyCode> {
    let code = match name {
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "backtab" => KeyCode::BackTab,
        _ => {
            // f1..=f12
            if let Some(n) = name.strip_prefix('f').and_then(|d| d.parse::<u8>().ok()) {
                if (1..=12).contains(&n) {
                    return Some(KeyCode::F(n));
                }
            }
            return None;
        }
    };
    Some(code)
}

/// Parse one whitespace-delimited token into a key sequence, per ADR-028's grammar:
/// strip `ctrl-`/`alt-`/`shift-` modifier prefixes, then interpret the remainder as a reserved
/// symbolic name (wins), a single literal char, or — failing both — a multi-char chord (each char
/// one key press). Returns `None` for an empty or all-prefix token (`""`, `ctrl-`).
fn parse_token(token: &str) -> Option<KeySeq> {
    let mut rest = token;
    let mut ctrl = false;
    let mut alt = false;
    loop {
        if let Some(r) = rest.strip_prefix("ctrl-") {
            ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("alt-") {
            alt = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("shift-") {
            // Shift is not tracked in matching (see [`KeyPress`]); accept the prefix so a user's
            // `shift-` token parses, but it contributes no modifier bit.
            rest = r;
        } else {
            break;
        }
    }

    if rest.is_empty() {
        return None;
    }

    // Reserved word wins over any literal interpretation.
    if let Some(code) = reserved_code(rest) {
        return Some(vec![KeyPress { code, ctrl, alt }]);
    }

    let chars: Vec<char> = rest.chars().collect();
    if chars.len() == 1 {
        return Some(vec![KeyPress {
            code: KeyCode::Char(chars[0]),
            ctrl,
            alt,
        }]);
    }
    // A chord: each char becomes one press in the sequence. A modifier prefix (rare on a chord)
    // applies to every press.
    Some(
        chars
            .into_iter()
            .map(|c| KeyPress {
                code: KeyCode::Char(c),
                ctrl,
                alt,
            })
            .collect(),
    )
}

/// Parse a full config value (space-separated tokens) into its alternative key sequences. An empty
/// or whitespace-only value yields no alternatives — an explicit *unbind* (ADR-028). Unparseable
/// tokens are skipped.
pub fn parse_value(value: &str) -> Vec<KeySeq> {
    value.split_whitespace().filter_map(parse_token).collect()
}

/// Render a key sequence back to a human-readable token (for warnings and the help overlay).
pub fn render_seq(seq: &[KeyPress]) -> String {
    seq.iter().map(render_press).collect()
}

fn render_press(press: &KeyPress) -> String {
    let mut out = String::new();
    if press.ctrl {
        out.push_str("ctrl-");
    }
    if press.alt {
        out.push_str("alt-");
    }
    let name = match press.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::BackTab => "backtab".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        other => format!("{other:?}"),
    };
    out.push_str(&name);
    out
}

/// The outcome of feeding one key to the keymap (see [`Keymap::advance`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// The keys so far are a strict prefix of a bound sequence — buffer retained, await more.
    Pending,
    /// A bound sequence matched exactly — buffer cleared.
    Command(Command),
    /// Nothing matched — buffer cleared. `mid_sequence` is `true` when this ended a partial chord
    /// (so the caller does NOT re-process the key, matching the old bracket-drop behavior); `false`
    /// for a fresh single key that matched nothing (where the caller may apply a hardcoded
    /// fallback, e.g. `Esc`).
    Unmatched { mid_sequence: bool },
}

enum MatchResult {
    Pending,
    Fire(Command),
    NoMatch,
}

/// A resolved, per-view keymap: the registry defaults with a repo's git-config overrides applied,
/// inverted into sequence→command lookup lists for the diff and outline contexts (each includes
/// the always-active global bindings).
pub struct Keymap {
    /// Active bindings when the diff has focus: global ∪ diff, in registry order (global first, so
    /// a global binding wins a collision). Scanned by [`Self::match_keys`].
    diff: Vec<(KeySeq, Command)>,
    /// Active bindings when the outline has focus: global ∪ outline.
    outline: Vec<(KeySeq, Command)>,
    /// Resolved key sequences per registry row (parallel to [`REGISTRY`]) — the source for the
    /// help overlay's "current keys for this action" (CS3).
    resolved: Vec<Vec<KeySeq>>,
    /// Config problems collected during resolution (unknown action names, key collisions) — the
    /// caller surfaces these through the footer-notice mechanism at startup.
    warnings: Vec<String>,
}

impl Keymap {
    /// The registry defaults with no config applied — the pre-ADR-028 hardcoded keymap. Used by
    /// the binary when config reading is unavailable, and by dispatch tests.
    pub fn defaults() -> Self {
        Self::from_bindings(&[])
    }

    /// Resolve the registry defaults against `bindings` (from [`crate::config::ReviewConfig::bindings`]).
    /// Each [`RawBinding`] replaces its action's default keys (empty value = unbind); an unknown
    /// action name is collected as a warning rather than panicking. Then invert into the per-view
    /// lookup lists, warning on (and deterministically resolving) any same-context key collision.
    pub fn from_bindings(bindings: &[RawBinding]) -> Self {
        let mut warnings = Vec::new();

        // 1. Seed each registry row with its parsed default keys.
        let mut resolved: Vec<Vec<KeySeq>> = REGISTRY
            .iter()
            .map(|entry| parse_value(entry.default_keys))
            .collect();

        // 2. Apply git-config overrides. `bindings` already carries git's native single-value
        //    precedence per (view, action), so each just replaces that row.
        for rb in bindings {
            match REGISTRY
                .iter()
                .position(|entry| entry.view == rb.view && entry.name == rb.action)
            {
                Some(idx) => resolved[idx] = parse_value(&rb.keys),
                None => warnings.push(format!(
                    "unknown review keybinding action '{}' in {} view (ignored)",
                    rb.action,
                    view_label(rb.view)
                )),
            }
        }

        // 3. Invert into the two context lookup lists, detecting collisions.
        let diff = build_context(&resolved, View::Diff, &mut warnings);
        let outline = build_context(&resolved, View::Outline, &mut warnings);

        warnings.dedup();

        Self {
            diff,
            outline,
            resolved,
            warnings,
        }
    }

    /// Config problems found during resolution (empty for a clean/default config).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The resolved key sequences currently bound to `command` (for the help overlay). Empty when
    /// the action is unbound.
    pub fn keys_for(&self, command: Command) -> &[KeySeq] {
        REGISTRY
            .iter()
            .position(|entry| entry.command == command)
            .map(|idx| self.resolved[idx].as_slice())
            .unwrap_or(&[])
    }

    /// Feed one key press, advancing `buffer` (the in-flight sequence) and reporting the outcome.
    /// Owns all buffer bookkeeping: retained on [`Dispatch::Pending`], cleared otherwise.
    pub fn advance(
        &self,
        outline_focused: bool,
        buffer: &mut Vec<KeyPress>,
        key: KeyEvent,
    ) -> Dispatch {
        buffer.push(KeyPress::from_event(key));
        match self.match_keys(outline_focused, buffer) {
            MatchResult::Pending => Dispatch::Pending,
            MatchResult::Fire(command) => {
                buffer.clear();
                Dispatch::Command(command)
            }
            MatchResult::NoMatch => {
                let mid_sequence = buffer.len() > 1;
                buffer.clear();
                Dispatch::Unmatched { mid_sequence }
            }
        }
    }

    /// Match the current `buffer` against the active context's bindings. A strict-prefix match
    /// takes precedence over an exact one (so a multi-key sequence is never cut short by a shorter
    /// binding — the defaults never have both for the same buffer anyway).
    fn match_keys(&self, outline_focused: bool, buffer: &[KeyPress]) -> MatchResult {
        let list = if outline_focused {
            &self.outline
        } else {
            &self.diff
        };
        let mut exact: Option<Command> = None;
        let mut has_prefix = false;
        for (seq, command) in list {
            if is_strict_prefix(buffer, seq) {
                has_prefix = true;
            } else if seq.as_slice() == buffer {
                exact.get_or_insert(*command);
            }
        }
        if has_prefix {
            MatchResult::Pending
        } else if let Some(command) = exact {
            MatchResult::Fire(command)
        } else {
            MatchResult::NoMatch
        }
    }
}

/// Build one context's active binding list: every global row plus every row of `view`, in
/// registry order (global first). On a key sequence already claimed in this context, the
/// first-seen (registry-order) command wins and a collision warning is recorded. Also flags
/// **prefix clashes**: a complete binding that is a strict prefix of another complete binding in
/// the same context. `match_keys` gives a pending (longer) chord precedence over an exact
/// (shorter) match in the same scan, so the shorter binding's command can never fire — the
/// warning names both actions and the view. Two chords merely sharing a prefix with each other
/// (`zM` vs `zR`) are NOT a clash: neither is a strict prefix of the other since a chord anchor
/// key (`z`) is never itself a complete binding here.
fn build_context(
    resolved: &[Vec<KeySeq>],
    view: View,
    warnings: &mut Vec<String>,
) -> Vec<(KeySeq, Command)> {
    let mut out: Vec<(KeySeq, Command)> = Vec::new();
    for (idx, entry) in REGISTRY.iter().enumerate() {
        if entry.view != View::Global && entry.view != view {
            continue;
        }
        for seq in &resolved[idx] {
            if let Some((_, winner)) = out.iter().find(|(existing, _)| existing == seq) {
                warnings.push(format!(
                    "key '{}' is bound to both {} and {} in the {} view; {} wins",
                    render_seq(seq),
                    command_label(*winner),
                    command_label(entry.command),
                    view_label(view),
                    command_label(*winner),
                ));
            } else {
                for (existing, other) in &out {
                    if is_strict_prefix(existing, seq) {
                        warnings.push(prefix_clash_warning(
                            view,
                            existing,
                            *other,
                            seq,
                            entry.command,
                        ));
                    } else if is_strict_prefix(seq, existing) {
                        warnings.push(prefix_clash_warning(
                            view,
                            seq,
                            entry.command,
                            existing,
                            *other,
                        ));
                    }
                }
                out.push((seq.clone(), entry.command));
            }
        }
    }
    out
}

/// True when `shorter` is strictly shorter than `longer` AND is its leading sub-sequence.
/// The ONE definition of chord-prefix precedence: `match_keys`' pending test and
/// `build_context`'s clash warning both call this, so the "the shorter binding can never
/// fire" claim in [`prefix_clash_warning`] can't drift from what dispatch actually does.
fn is_strict_prefix(shorter: &[KeyPress], longer: &[KeyPress]) -> bool {
    shorter.len() < longer.len() && longer[..shorter.len()] == *shorter
}

/// A prefix-clash warning: `shorter_seq`/`shorter_cmd` is the bare(r) binding rendered
/// unreachable by the longer, chord-taking-precedence `longer_seq`/`longer_cmd`.
fn prefix_clash_warning(
    view: View,
    shorter_seq: &[KeyPress],
    shorter_cmd: Command,
    longer_seq: &[KeyPress],
    longer_cmd: Command,
) -> String {
    format!(
        "key '{}' for {} is unreachable in the {} view: the longer chord '{}' is bound to {}, \
         and a pending chord always takes precedence over a shorter binding sharing its prefix",
        render_seq(shorter_seq),
        command_label(shorter_cmd),
        view_label(view),
        render_seq(longer_seq),
        command_label(longer_cmd),
    )
}

/// One row of the `?` help overlay: an action's resolved key label (space-joined alternatives,
/// e.g. `"tab ]f"`) and its registry description. Built by [`help_sections`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpEntry {
    pub keys: String,
    pub description: &'static str,
}

/// One titled group of [`HelpEntry`] rows in the help overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub title: &'static str,
    pub entries: Vec<HelpEntry>,
}

/// Build the help overlay's content for `focused` (the view with keyboard focus — [`View::Diff`]
/// or [`View::Outline`]; never [`View::Global`]): a "Global" section, then the focused view's own
/// section, each listing only BOUND actions (an action with no resolved keys — user-unbound — is
/// skipped, per CS3). Pure and `Keymap`-driven — the display never hardcodes a key string, so a
/// rebind shows here automatically.
pub fn help_sections(keymap: &Keymap, focused: View) -> Vec<HelpSection> {
    vec![
        HelpSection {
            title: "Global",
            entries: entries_for_view(keymap, View::Global),
        },
        HelpSection {
            title: view_label_title(focused),
            entries: entries_for_view(keymap, focused),
        },
    ]
}

fn entries_for_view(keymap: &Keymap, view: View) -> Vec<HelpEntry> {
    REGISTRY
        .iter()
        .filter(|entry| entry.view == view)
        .filter_map(|entry| {
            let seqs = keymap.keys_for(entry.command);
            if seqs.is_empty() {
                return None;
            }
            let keys = seqs
                .iter()
                .map(|seq| render_seq(seq))
                .collect::<Vec<_>>()
                .join(" ");
            Some(HelpEntry {
                keys,
                description: entry.description,
            })
        })
        .collect()
}

fn view_label_title(view: View) -> &'static str {
    match view {
        View::Global => "Global",
        View::Diff => "Diff",
        View::Outline => "Outline",
    }
}

/// The resolved key label for the FIRST alternative bound to `command` (for the curated footer
/// hint, which only has room for one key per action), or `None` when unbound. `pub` (not
/// `pub(crate)`): `main.rs::seat_app` — the bin target, consuming the lib externally — uses the
/// same first-alternative policy for the refusal hint's zoom key label, so the two can't diverge.
pub fn primary_key(keymap: &Keymap, command: Command) -> Option<String> {
    keymap.keys_for(command).first().map(|seq| render_seq(seq))
}

/// One curated footer entry: a resolved key label paired with a short verb, or an up/down PAIR
/// collapsed to a single `down/up verb` entry (e.g. `j/k move`) when both resolve.
enum HintItem {
    One(Command, &'static str),
    Pair(Command, Command, &'static str),
}

/// CS4 (`outline-mode-cycle`): most hint labels are the static string baked into the `HintItem`,
/// but `OutlineCycleMode`'s label shows the mode `i` would switch TO instead — computed from
/// `outline_mode` (the outline's CURRENT mode, so this is `outline_mode.cycle()`'s label).
fn render_hint_item(keymap: &Keymap, item: &HintItem, outline_mode: OutlineMode) -> Option<String> {
    match item {
        HintItem::One(command, label) => {
            let label = if *command == Command::OutlineCycleMode {
                format!("\u{2192}{}", outline_mode.cycle().label())
            } else {
                (*label).to_string()
            };
            primary_key(keymap, *command).map(|k| format!("{k} {label}"))
        }
        HintItem::Pair(down, up, label) => {
            match (primary_key(keymap, *down), primary_key(keymap, *up)) {
                (Some(d), Some(u)) => Some(format!("{d}/{u} {label}")),
                (Some(d), None) => Some(format!("{d} {label}")),
                (None, Some(u)) => Some(format!("{u} {label}")),
                (None, None) => None,
            }
        }
    }
}

/// The diff view's curated footer hint set (locked design in CS3): nav, stage/discard, outline,
/// help, quit — ~5-7 entries picked to make the tool feel learnable, not an exhaustive list.
const DIFF_HINTS: &[HintItem] = &[
    HintItem::Pair(Command::CursorDown, Command::CursorUp, "move"),
    HintItem::One(Command::StageHunk, "stage"),
    HintItem::One(Command::DiscardHunk, "discard"),
    HintItem::One(Command::ToggleOutline, "outline"),
    HintItem::One(Command::ToggleHelp, "help"),
    HintItem::One(Command::Quit, "quit"),
];

/// The outline view's curated footer hint set (locked design in CS3).
const OUTLINE_HINTS: &[HintItem] = &[
    HintItem::Pair(Command::OutlineDown, Command::OutlineUp, "move"),
    HintItem::One(Command::OutlineConfirm, "open"),
    HintItem::Pair(
        Command::OutlineNextChangeset,
        Command::OutlinePrevChangeset,
        "changeset",
    ),
    HintItem::One(Command::OutlineCycleMode, "mode"),
    // No `o outline` / `q quit` here: with the changeset pair the full set no longer fits 80
    // cols, and both stay discoverable in the diff footer and the help overlay.
    HintItem::One(Command::ToggleHelp, "help"),
];

/// Build the persistent, always-visible footer hint string for `focused` ([`View::Diff`] or
/// [`View::Outline`]; never [`View::Global`]) from the resolved `keymap` — never a hardcoded key
/// string, so a rebind shows here too. A notice temporarily replaces this in the footer (the
/// caller's job, see `render::render_footer`); an unbound curated action is simply dropped from
/// the string rather than leaving a stale/wrong key visible.
pub fn footer_hint(keymap: &Keymap, focused: View, outline_mode: OutlineMode) -> String {
    let items: &[HintItem] = match focused {
        View::Diff => DIFF_HINTS,
        View::Outline => OUTLINE_HINTS,
        View::Global => &[],
    };
    items
        .iter()
        .filter_map(|item| render_hint_item(keymap, item, outline_mode))
        .collect::<Vec<_>>()
        .join("  \u{b7}  ")
}

/// The config action name for a command (for collision warnings) — its registry `name`.
fn command_label(command: Command) -> &'static str {
    REGISTRY
        .iter()
        .find(|entry| entry.command == command)
        .map(|entry| entry.name)
        .unwrap_or("?")
}

fn view_label(view: View) -> &'static str {
    match view {
        View::Global => "global",
        View::Diff => "diff",
        View::Outline => "outline",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(c: char) -> KeyPress {
        KeyPress {
            code: KeyCode::Char(c),
            ctrl: false,
            alt: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Feed a whole sequence of key events, returning the terminal [`Dispatch`].
    fn feed(km: &Keymap, outline_focused: bool, events: &[KeyEvent]) -> Dispatch {
        let mut buffer = Vec::new();
        let mut last = Dispatch::Unmatched {
            mid_sequence: false,
        };
        for &ev in events {
            last = km.advance(outline_focused, &mut buffer, ev);
        }
        last
    }

    // ── Parser ───────────────────────────────────────────────────────────────

    #[test]
    fn parses_a_literal_single_char() {
        assert_eq!(parse_value("s"), vec![vec![press('s')]]);
    }

    #[test]
    fn parses_a_reserved_word() {
        assert_eq!(
            parse_value("enter"),
            vec![vec![KeyPress {
                code: KeyCode::Enter,
                ctrl: false,
                alt: false,
            }]]
        );
    }

    #[test]
    fn space_reserved_word_wins_over_a_chord_of_its_letters() {
        // "space" must be the spacebar, not a five-key chord of s, p, a, c, e.
        assert_eq!(
            parse_value("space"),
            vec![vec![KeyPress {
                code: KeyCode::Char(' '),
                ctrl: false,
                alt: false,
            }]]
        );
    }

    #[test]
    fn parses_a_ctrl_modifier() {
        assert_eq!(
            parse_value("ctrl-d"),
            vec![vec![KeyPress {
                code: KeyCode::Char('d'),
                ctrl: true,
                alt: false,
            }]]
        );
    }

    #[test]
    fn parses_a_two_key_chord() {
        assert_eq!(parse_value("]f"), vec![vec![press(']'), press('f')]]);
    }

    #[test]
    fn parses_multiple_alternatives() {
        assert_eq!(
            parse_value("j down"),
            vec![
                vec![press('j')],
                vec![KeyPress {
                    code: KeyCode::Down,
                    ctrl: false,
                    alt: false,
                }],
            ]
        );
    }

    #[test]
    fn empty_value_is_an_unbind() {
        assert!(parse_value("").is_empty());
        assert!(parse_value("   ").is_empty());
    }

    #[test]
    fn f_keys_and_arrows_parse() {
        assert_eq!(
            parse_value("f5"),
            vec![vec![KeyPress {
                code: KeyCode::F(5),
                ctrl: false,
                alt: false,
            }]]
        );
    }

    // ── Resolution ─────────────────────────────────────────────────────────────

    #[test]
    fn defaults_reproduce_the_hardcoded_bindings() {
        let km = Keymap::defaults();
        assert!(km.warnings().is_empty(), "defaults never collide");
        // A representative spread across token classes.
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('s'))]),
            Dispatch::Command(Command::StageHunk)
        );
        assert_eq!(
            feed(
                &km,
                false,
                &[key(KeyCode::Char(']')), key(KeyCode::Char('f'))]
            ),
            Dispatch::Command(Command::NextFile)
        );
        assert_eq!(
            feed(
                &km,
                false,
                &[KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)]
            ),
            Dispatch::Command(Command::HalfPageDown)
        );
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Tab)]),
            Dispatch::Command(Command::NextFile)
        );
        // Global works from the outline context too.
        assert_eq!(
            feed(&km, true, &[key(KeyCode::Char('o'))]),
            Dispatch::Command(Command::ToggleOutline)
        );
        assert_eq!(
            feed(&km, true, &[key(KeyCode::Char('j'))]),
            Dispatch::Command(Command::OutlineDown)
        );
    }

    // ── diff-hscroll: `hscroll-left`/`hscroll-right` registry rows ─────────────

    #[test]
    fn hscroll_commands_are_registered_with_no_collision_warnings() {
        let km = Keymap::defaults();
        assert!(
            km.warnings().is_empty(),
            "the new commands' defaults must not collide with anything: {:?}",
            km.warnings()
        );
        assert!(
            !km.keys_for(Command::HscrollLeft).is_empty(),
            "hscroll-left must resolve to at least one bound key"
        );
        assert!(
            !km.keys_for(Command::HscrollRight).is_empty(),
            "hscroll-right must resolve to at least one bound key"
        );
    }

    #[test]
    fn less_than_and_greater_than_dispatch_hscroll_in_the_diff_view() {
        let km = Keymap::defaults();
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('<'))]),
            Dispatch::Command(Command::HscrollLeft)
        );
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('>'))]),
            Dispatch::Command(Command::HscrollRight)
        );
    }

    /// `l`/`right` are free in the Diff view (they're only bound in the Outline view, to
    /// `focus-diff`) — the handoff's locked decision #2 reuses them for `hscroll-right` there,
    /// mirroring the Outline view's `l`/`right` = focus-diff.
    #[test]
    fn l_and_right_dispatch_hscroll_right_in_the_diff_view() {
        let km = Keymap::defaults();
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('l'))]),
            Dispatch::Command(Command::HscrollRight)
        );
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Right)]),
            Dispatch::Command(Command::HscrollRight)
        );
    }

    #[test]
    fn n_and_p_dispatch_outline_changeset_nav_with_no_collisions() {
        let km = Keymap::defaults();
        assert!(
            km.warnings().is_empty(),
            "n/p defaults must not collide with anything: {:?}",
            km.warnings()
        );
        assert_eq!(
            feed(&km, true, &[key(KeyCode::Char('n'))]),
            Dispatch::Command(Command::OutlineNextChangeset)
        );
        assert_eq!(
            feed(&km, true, &[key(KeyCode::Char('p'))]),
            Dispatch::Command(Command::OutlinePrevChangeset)
        );
    }

    #[test]
    fn z_m_and_z_r_dispatch_outline_fold_all_with_no_collisions() {
        let km = Keymap::defaults();
        assert!(
            km.warnings().is_empty(),
            "zM/zR defaults must not collide with anything: {:?}",
            km.warnings()
        );
        assert_eq!(
            feed(
                &km,
                true,
                &[key(KeyCode::Char('z')), key(KeyCode::Char('M'))]
            ),
            Dispatch::Command(Command::OutlineCollapseAll)
        );
        assert_eq!(
            feed(
                &km,
                true,
                &[key(KeyCode::Char('z')), key(KeyCode::Char('R'))]
            ),
            Dispatch::Command(Command::OutlineExpandAll)
        );
    }

    /// CS3 (diff-fold-keys) originally bound `n` as an extra default on `next-hunk`, for symmetry
    /// with the outline's `n`/`p` changeset nav. M11 CS3 (`diff-search`) reclaims `n` as
    /// `search-next`'s default instead (falling back to `next-hunk` itself when no search is
    /// active — `App::search_next` — so `n`'s PRACTICAL effect on an unbound-search diff is
    /// unchanged); `p` is untouched, still `prev-hunk`'s extra default. `primary_key` still picks
    /// the first token, so the footer/help keep showing `]h`/`[h` for `next-hunk`/`prev-hunk`
    /// themselves — neither is in `DIFF_HINTS` today, but `primary_key`/`keys_for` (which the help
    /// overlay uses) are exercised by `footer_hint_renders_the_curated_diff_entries` and
    /// `help_sections_groups_global_and_the_focused_view_only`.
    #[test]
    fn n_dispatches_search_next_and_p_still_dispatches_prev_hunk_with_no_collisions() {
        let km = Keymap::defaults();
        assert!(
            km.warnings().is_empty(),
            "n/p defaults must not collide with anything: {:?}",
            km.warnings()
        );
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('n'))]),
            Dispatch::Command(Command::SearchNext)
        );
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('p'))]),
            Dispatch::Command(Command::PrevHunk)
        );
    }

    /// CS3 (diff-fold-keys): `cycle-zoom` was rebound from bare `z` to `Z` to make room for the
    /// `zM`/`zR` gap fold-all chords in `View::Diff`. This wasn't optional bookkeeping —
    /// `match_keys` gives a strict-prefix match precedence over an exact one in the SAME scan
    /// (see its doc comment): had `cycle-zoom` stayed on bare `z` alongside `zM`/`zR`, a lone `z`
    /// press would always report `Pending` instead of firing `CycleZoom` immediately, and any
    /// follow-up key that wasn't `M`/`R` would be swallowed as `Unmatched { mid_sequence: true }`
    /// rather than re-processed — silently breaking `cycle-zoom` with no *runtime* warning; the
    /// matcher's chord-wins precedence never changed. This test pins the resolved state: `Z`
    /// fires `CycleZoom` immediately, and `z` only
    /// ever anchors the `zM`/`zR` chords below — never a bare-key command of its own again.
    /// (`build_context` now also flags this shape as a prefix clash and warns — see
    /// `a_bare_prefix_binding_warns_about_the_chord_that_shadows_it` below — but the resolved
    /// defaults must still be clash-free, hence the empty-warnings assertion here.)
    #[test]
    fn shift_z_dispatches_cycle_zoom_with_no_collisions() {
        let km = Keymap::defaults();
        assert!(
            km.warnings().is_empty(),
            "cycle-zoom's rebind to Z must not collide with anything: {:?}",
            km.warnings()
        );
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('Z'))]),
            Dispatch::Command(Command::CycleZoom)
        );
    }

    /// `zM`/`zR` — reset/expand-all gaps in the diff view (companion to the outline's own
    /// `zM`/`zR` fold-all, `z_m_and_z_r_dispatch_outline_fold_all_with_no_collisions` above).
    /// Coexists cleanly with `Z` (`cycle-zoom`, see the test above) now that `cycle-zoom` no
    /// longer claims the bare `z` prefix.
    #[test]
    fn z_m_and_z_r_dispatch_diff_gap_fold_all_with_no_collisions() {
        let km = Keymap::defaults();
        assert!(
            km.warnings().is_empty(),
            "zM/zR defaults must not collide with anything: {:?}",
            km.warnings()
        );
        assert_eq!(
            feed(
                &km,
                false,
                &[key(KeyCode::Char('z')), key(KeyCode::Char('M'))]
            ),
            Dispatch::Command(Command::ResetGaps)
        );
        assert_eq!(
            feed(
                &km,
                false,
                &[key(KeyCode::Char('z')), key(KeyCode::Char('R'))]
            ),
            Dispatch::Command(Command::ExpandAllGaps)
        );
    }

    /// M11 (`copy-lines`/`copy-location`, the yank split): `y` was free in both `View::Global`
    /// and `View::Diff` (unlike `p`, which `[h`'s extra default and the outline's
    /// `prev-changeset` already claim), so no existing binding needed to move to make room for
    /// it — unlike `cycle-zoom`'s `z` -> `Z` rebind above. `Y` is likewise free (verified against
    /// every `default_keys` entry in this registry when the yank split was designed). Pins both
    /// resolved defaults clash-free the same way those tests do.
    #[test]
    fn y_dispatches_copy_lines_and_shift_y_dispatches_copy_location_with_no_collisions() {
        let km = Keymap::defaults();
        assert!(
            km.warnings().is_empty(),
            "copy-lines/copy-location's default `y`/`Y` must not collide with anything: {:?}",
            km.warnings()
        );
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('y'))]),
            Dispatch::Command(Command::CopyLines)
        );
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('Y'))]),
            Dispatch::Command(Command::CopyLocation)
        );
    }

    #[test]
    fn a_config_rebind_overrides_the_default() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "stage-hunk".to_string(),
            keys: "x".to_string(),
        }]);
        assert!(km.warnings().is_empty());
        // The new key fires the action…
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('x'))]),
            Dispatch::Command(Command::StageHunk)
        );
        // …and the old default no longer does (it's now unbound).
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('s'))]),
            Dispatch::Unmatched {
                mid_sequence: false
            }
        );
    }

    #[test]
    fn an_empty_value_unbinds_the_action() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "stage-hunk".to_string(),
            keys: String::new(),
        }]);
        assert!(km.keys_for(Command::StageHunk).is_empty());
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('s'))]),
            Dispatch::Unmatched {
                mid_sequence: false
            }
        );
    }

    #[test]
    fn reload_config_is_registered_global_with_default_shift_r() {
        let km = Keymap::defaults();
        assert!(km.warnings().is_empty());
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('R'))]),
            Dispatch::Command(Command::ReloadConfig)
        );
        // Global — fires the same whether the outline or the diff has focus.
        assert_eq!(
            feed(&km, true, &[key(KeyCode::Char('R'))]),
            Dispatch::Command(Command::ReloadConfig)
        );
    }

    #[test]
    fn reload_config_is_rebindable_via_workon_review_bind() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Global,
            action: "reload-config".to_string(),
            keys: "ctrl-r".to_string(),
        }]);
        assert!(km.warnings().is_empty());
        assert_eq!(
            feed(
                &km,
                false,
                &[KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)]
            ),
            Dispatch::Command(Command::ReloadConfig)
        );
        // The old default is now unbound.
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('R'))]),
            Dispatch::Unmatched {
                mid_sequence: false
            }
        );
    }

    #[test]
    fn an_unknown_action_warns_without_panicking() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "frobnicate".to_string(),
            keys: "x".to_string(),
        }]);
        assert_eq!(km.warnings().len(), 1);
        assert!(km.warnings()[0].contains("frobnicate"));
    }

    #[test]
    fn a_collision_warns_and_resolves_deterministically() {
        // Rebind stage-hunk onto `j`, which already means cursor-down in the diff view.
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "stage-hunk".to_string(),
            keys: "j".to_string(),
        }]);
        assert_eq!(km.warnings().len(), 1);
        assert!(km.warnings()[0].contains("cursor-down"));
        assert!(km.warnings()[0].contains("stage-hunk"));
        // Registry order: cursor-down is declared before stage-hunk, so it wins.
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('j'))]),
            Dispatch::Command(Command::CursorDown)
        );
    }

    #[test]
    fn a_global_binding_wins_a_collision_with_a_view_binding() {
        // Rebind diff's refresh onto `o`, the global toggle-outline key.
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "refresh".to_string(),
            keys: "o".to_string(),
        }]);
        assert_eq!(km.warnings().len(), 1);
        assert_eq!(
            feed(&km, false, &[key(KeyCode::Char('o'))]),
            Dispatch::Command(Command::ToggleOutline)
        );
    }

    #[test]
    fn a_bare_prefix_binding_warns_about_the_chord_that_shadows_it() {
        // Rebind cycle-zoom back onto bare `z`, which now collides with the `zM`/`zR` gap
        // fold-all chords still on their defaults in `View::Diff`. Each chord sharing the `z`
        // prefix is its own clashing pair — one warning per pair, both naming cycle-zoom.
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "cycle-zoom".to_string(),
            keys: "z".to_string(),
        }]);
        assert_eq!(km.warnings().len(), 2, "warnings: {:?}", km.warnings());
        assert!(km.warnings().iter().all(|w| w.contains("cycle-zoom")));
        assert!(km.warnings().iter().all(|w| w.contains("diff")));
        assert!(km.warnings().iter().any(|w| w.contains("reset-gaps")));
        assert!(km.warnings().iter().any(|w| w.contains("expand-all-gaps")));
    }

    #[test]
    fn z_m_and_z_r_alone_do_not_clash_with_each_other() {
        // zM/zR both anchor on `z` but neither is a strict prefix of the other — no warning.
        let km = Keymap::defaults();
        assert!(
            km.warnings().is_empty(),
            "zM/zR must not warn about each other: {:?}",
            km.warnings()
        );
    }

    // ── Dispatch / sequences ──────────────────────────────────────────────────

    #[test]
    fn a_partial_chord_reports_pending_then_fires() {
        let km = Keymap::defaults();
        let mut buffer = Vec::new();
        assert_eq!(
            km.advance(false, &mut buffer, key(KeyCode::Char(']'))),
            Dispatch::Pending
        );
        assert_eq!(buffer.len(), 1, "the prefix key is retained");
        assert_eq!(
            km.advance(false, &mut buffer, key(KeyCode::Char('h'))),
            Dispatch::Command(Command::NextHunk)
        );
        assert!(buffer.is_empty(), "the buffer clears once the chord fires");
    }

    #[test]
    fn an_unrecognized_chord_suffix_drops_the_buffer() {
        let km = Keymap::defaults();
        let mut buffer = Vec::new();
        km.advance(false, &mut buffer, key(KeyCode::Char(']')));
        assert_eq!(
            km.advance(false, &mut buffer, key(KeyCode::Char('x'))),
            Dispatch::Unmatched { mid_sequence: true }
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_rebound_chord_still_works() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "stage-hunk".to_string(),
            keys: "gs".to_string(),
        }]);
        assert_eq!(
            feed(
                &km,
                false,
                &[key(KeyCode::Char('g')), key(KeyCode::Char('s'))]
            ),
            Dispatch::Command(Command::StageHunk)
        );
    }

    // ── CS3: help overlay / footer hint builders ────────────────────────────

    #[test]
    fn help_sections_groups_global_and_the_focused_view_only() {
        let km = Keymap::defaults();
        let sections = help_sections(&km, View::Diff);

        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "Global");
        assert_eq!(sections[1].title, "Diff");
        // Outline-only actions never leak into the diff-focused overlay.
        assert!(!sections.iter().any(|s| s
            .entries
            .iter()
            .any(|e| e.description.contains("outline cursor"))));

        let outline_sections = help_sections(&km, View::Outline);
        assert_eq!(outline_sections[1].title, "Outline");
    }

    #[test]
    fn help_sections_cycle_mode_entry_spells_out_the_full_order() {
        // CS4: descriptions are static `&'static str`s baked into `REGISTRY`, so the help
        // overlay can't mark the CURRENT mode dynamically without a broader refactor — the
        // locked fallback is a static full-order description, with the dynamic `→next` shown
        // only in the footer hint (see `footer_hint_outline_cycle_label_tracks_the_current_mode`).
        let km = Keymap::defaults();
        let sections = help_sections(&km, View::Outline);
        let outline = &sections[1];
        let entry = outline
            .entries
            .iter()
            .find(|e| e.description.contains("Cycle the outline mode"))
            .expect("cycle-mode row present");
        assert!(
            entry
                .description
                .contains("stack \u{25b8} stack-tree \u{25b8} flat \u{25b8} tree"),
            "got: {:?}",
            entry.description
        );
    }

    #[test]
    fn help_sections_skip_an_unbound_action() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "stage-hunk".to_string(),
            keys: String::new(),
        }]);
        let sections = help_sections(&km, View::Diff);
        let diff = &sections[1];
        assert!(
            !diff
                .entries
                .iter()
                .any(|e| e.description.contains("Stage the hunk")),
            "an unbound action must not appear in the help overlay"
        );
    }

    #[test]
    fn help_sections_render_a_rebound_key_not_the_default() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "stage-hunk".to_string(),
            keys: "x".to_string(),
        }]);
        let sections = help_sections(&km, View::Diff);
        let diff = &sections[1];
        let stage_row = diff
            .entries
            .iter()
            .find(|e| e.description.contains("Stage the hunk"))
            .expect("stage-hunk row present");
        assert_eq!(stage_row.keys, "x", "the overlay must show the REBOUND key");
    }

    #[test]
    fn footer_hint_renders_the_curated_diff_entries() {
        let km = Keymap::defaults();
        let hint = footer_hint(&km, View::Diff, OutlineMode::default());
        assert!(hint.contains("j/k move"), "got: {hint:?}");
        assert!(hint.contains("s stage"), "got: {hint:?}");
        assert!(hint.contains("d discard"), "got: {hint:?}");
        assert!(hint.contains("o outline"), "got: {hint:?}");
        assert!(hint.contains("? help"), "got: {hint:?}");
        assert!(hint.contains("q quit"), "got: {hint:?}");
    }

    #[test]
    fn footer_hint_renders_the_curated_outline_entries() {
        let km = Keymap::defaults();
        let hint = footer_hint(&km, View::Outline, OutlineMode::Stack);
        assert!(hint.contains("j/k move"), "got: {hint:?}");
        assert!(hint.contains("enter open"), "got: {hint:?}");
        assert!(hint.contains("n/p changeset"), "got: {hint:?}");
        assert!(
            hint.contains("i \u{2192}stack-tree"),
            "cycling from Stack must show the next mode, StackTree; got: {hint:?}"
        );
        assert!(hint.contains("? help"), "got: {hint:?}");
        assert!(
            hint.chars().count() <= 80,
            "the curated outline hint must fit an 80-col footer even with the longest \
             next-mode label (stack-tree); got {} chars: {hint:?}",
            hint.chars().count()
        );
    }

    #[test]
    fn footer_hint_outline_cycle_label_tracks_the_current_mode() {
        let km = Keymap::defaults();
        for (mode, next) in [
            (OutlineMode::Stack, "stack-tree"),
            (OutlineMode::StackTree, "flat"),
            (OutlineMode::Flat, "tree"),
            (OutlineMode::Tree, "stack"),
        ] {
            let hint = footer_hint(&km, View::Outline, mode);
            let want = format!("i \u{2192}{next}");
            assert!(
                hint.contains(&want),
                "mode {mode:?} should hint the NEXT mode {next:?}; got: {hint:?}"
            );
        }
    }

    #[test]
    fn footer_hint_renders_a_rebound_key_not_the_default() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "stage-hunk".to_string(),
            keys: "x".to_string(),
        }]);
        let hint = footer_hint(&km, View::Diff, OutlineMode::default());
        assert!(hint.contains("x stage"), "got: {hint:?}");
        assert!(!hint.contains("s stage"), "got: {hint:?}");
    }

    #[test]
    fn footer_hint_drops_an_unbound_curated_action_rather_than_a_stale_key() {
        let km = Keymap::from_bindings(&[RawBinding {
            view: View::Diff,
            action: "stage-hunk".to_string(),
            keys: String::new(),
        }]);
        let hint = footer_hint(&km, View::Diff, OutlineMode::default());
        assert!(!hint.contains("stage"), "got: {hint:?}");
        // The rest of the curated set is unaffected.
        assert!(hint.contains("d discard"), "got: {hint:?}");
    }
}
