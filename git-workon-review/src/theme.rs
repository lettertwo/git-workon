//! The base16 color-scheme primitive and the colors the renderer resolves against it (ADR-029).
//!
//! This is the theming *primitive* — the resolved palette a frame is painted with — distinct from
//! [`crate::config::Theme`], which is the git-config *selection* (`auto`/`dark`/`light`). CS4 is
//! dark-only and behavior-preserving: [`Theme::dark`] reproduces M3–M5's hardcoded colors exactly.
//! CS5 adds a light instance and wires [`crate::config::Theme`] to pick between them; CS6 adds the
//! terminal-derivation probe for `auto`.
//!
//! ## Hybrid boundary (ADR-029)
//! Colors that sit ON a tinted background — the diff add/del gradient, its staged variants, the
//! cursor/selection washes, and syntax foreground — are theme-controlled base16 truecolor and live
//! here. Chrome that is NOT on a tint (gutter, dividers, footer, dim labels, status markers) stays
//! ANSI-named / const in [`crate::render`] so it self-adapts to the terminal palette and is
//! probe-independent. This module deliberately holds only the on-tint half.

use ratatui::style::Color;

/// A 16-slot base16 palette: `base00`–`base07` are the monochrome ramp (background → foreground),
/// `base08`–`base0F` the accents. Slot roles follow the base16 styling spec (base08 red, base0B
/// green, base0E keyword, …). Indexed 0–15 by slot number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Base16 {
    pub slots: [Color; 16],
}

impl Base16 {
    /// base16-eighties.dark (Chris Kempson) — the scheme M3–M5's syntax accents were already drawn
    /// from (`highlight.rs`'s `C_*` consts ARE these slots; see ADR-029). Reproduced here in full
    /// so `Theme::dark` is a faithful re-expression of the shipped dark colors.
    const EIGHTIES_DARK: Base16 = Base16 {
        slots: [
            Color::Rgb(0x2d, 0x2d, 0x2d), // base00 background
            Color::Rgb(0x39, 0x39, 0x39), // base01
            Color::Rgb(0x51, 0x51, 0x51), // base02
            Color::Rgb(0x74, 0x73, 0x69), // base03 comments
            Color::Rgb(0xa0, 0x9f, 0x93), // base04
            Color::Rgb(0xd3, 0xd0, 0xc8), // base05 foreground
            Color::Rgb(0xe8, 0xe6, 0xdf), // base06
            Color::Rgb(0xf2, 0xf0, 0xec), // base07
            Color::Rgb(0xf2, 0x77, 0x7a), // base08 red / diff deleted
            Color::Rgb(0xf9, 0x91, 0x57), // base09 orange
            Color::Rgb(0xff, 0xcc, 0x66), // base0A yellow
            Color::Rgb(0x99, 0xcc, 0x99), // base0B green / diff inserted
            Color::Rgb(0x66, 0xcc, 0xcc), // base0C cyan
            Color::Rgb(0x66, 0x99, 0xcc), // base0D blue
            Color::Rgb(0xcc, 0x99, 0xcc), // base0E purple / keyword
            Color::Rgb(0xd2, 0x7b, 0x53), // base0F brown
        ],
    };

    fn slot(&self, i: usize) -> Color {
        self.slots[i]
    }
}

/// Per-capture syntax template: each entry is the base16 slot index that the parallel
/// [`crate::highlight::HIGHLIGHT_NAMES`] capture maps to, per the base16 role conventions
/// (ADR-029). Theme-invariant — every scheme applies this same template to its own slots — so it
/// lives with the primitive, not on any one [`Theme`]. A theme switch re-colors by re-rendering:
/// the tree-sitter pass records only the capture index (see [`crate::highlight::FgSpan`]), and the
/// color is resolved here at paint time.
const SYNTAX_SLOTS: [usize; 28] = [
    9,  // attribute            → base09 orange
    3,  // comment              → base03
    9,  // constant             → base09
    9,  // constant.builtin     → base09
    10, // constructor          → base0A yellow
    5,  // embedded             → base05 fg
    12, // escape               → base0C cyan
    13, // function             → base0D blue
    13, // function.builtin     → base0D
    13, // function.macro       → base0D
    13, // function.method      → base0D
    14, // keyword              → base0E purple
    8,  // label                → base08 red
    9,  // number               → base09
    5,  // operator             → base05
    12, // property             → base0C
    5,  // punctuation          → base05
    5,  // punctuation.bracket  → base05
    5,  // punctuation.delimiter→ base05
    12, // punctuation.special  → base0C
    11, // string               → base0B green
    12, // string.special       → base0C
    8,  // tag                  → base08
    10, // type                 → base0A
    10, // type.builtin         → base0A
    5,  // variable             → base05
    8,  // variable.builtin     → base08
    5,  // variable.parameter   → base05
];

/// The number of entries in the per-capture syntax template — must equal
/// [`crate::highlight::HIGHLIGHT_NAMES`]'s length (asserted in `highlight`'s tests). Exposed so
/// that invariant can be checked without making [`SYNTAX_SLOTS`] itself public.
pub fn syntax_slot_count() -> usize {
    SYNTAX_SLOTS.len()
}

/// The resolved on-tint palette a frame is painted with (ADR-029's theme-controlled half).
///
/// Syntax foreground is looked up per capture index via [`Theme::syntax`]; the diff-background
/// gradient, its staged variants, and the cursor/selection/outline washes are read directly. All
/// values in [`Theme::dark`] reproduce the M3–M5 hardcoded colors exactly (CS4 is a
/// behavior-preserving refactor).
pub struct Theme {
    /// Per-capture syntax fg, indexed by the same capture index as
    /// [`crate::highlight::HIGHLIGHT_NAMES`] (see [`SYNTAX_SLOTS`]).
    syntax: Vec<Color>,

    /// Whole-line subtle / word-level strong background for an unstaged (bright) Del cell.
    pub del_subtle: Color,
    pub del_strong: Color,
    /// Bright Add-cell background pair (counterpart of [`Theme::del_subtle`]).
    pub add_subtle: Color,
    pub add_strong: Color,
    /// Dim/desaturated Del pair for staged-ness attribution (locked decision #7) — a staged change
    /// reads as "already handled" without disappearing into plain context.
    pub del_staged_subtle: Color,
    pub del_staged_strong: Color,
    /// Dim Add pair — green-tinted counterpart of the staged Del pair.
    pub add_staged_subtle: Color,
    pub add_staged_strong: Color,

    /// Tint blended into the cursor row's background — a cool slate-blue.
    pub cursor_bg: Color,
    /// Tint blended into a selected (line-selection) row — a muted teal, distinct from
    /// [`Theme::cursor_bg`].
    pub selection_bg: Color,
    /// Cursor wash for the outline pane while OPEN but NOT focused — dimmer than [`Theme::cursor_bg`].
    pub outline_cursor_unfocused_bg: Color,
}

impl Theme {
    /// The curated dark scheme: base16-eighties.dark accents + the M3–M5 hand-tuned diff/cursor
    /// tints, reproduced byte-for-byte (the pixel-identity gate — see the module doc and ADR-029).
    ///
    /// The diff-bg tints are held explicit rather than derived: a clean base08/base0B → base00
    /// blend cannot reproduce these particular hand-tuned constants (their green/blue channels sit
    /// *below* base00, so no convex blend toward base00 reaches them). ADR-029's derivation is
    /// therefore deferred to CS5, where the light scheme defines its own tints; dark keeps the
    /// shipped values verbatim.
    pub fn dark() -> Self {
        let base = Base16::EIGHTIES_DARK;
        Theme {
            syntax: SYNTAX_SLOTS.iter().map(|&s| base.slot(s)).collect(),
            del_subtle: Color::Rgb(60, 24, 24),
            del_strong: Color::Rgb(120, 40, 40),
            add_subtle: Color::Rgb(20, 48, 24),
            add_strong: Color::Rgb(32, 100, 48),
            del_staged_subtle: Color::Rgb(42, 26, 28),
            del_staged_strong: Color::Rgb(64, 38, 40),
            add_staged_subtle: Color::Rgb(24, 34, 26),
            add_staged_strong: Color::Rgb(34, 50, 38),
            cursor_bg: Color::Rgb(45, 50, 90),
            selection_bg: Color::Rgb(30, 66, 66),
            outline_cursor_unfocused_bg: Color::Rgb(35, 38, 55),
        }
    }

    /// The syntax foreground for a capture index (position in
    /// [`crate::highlight::HIGHLIGHT_NAMES`]). This is the render-time resolution the whole
    /// mechanism turns on: [`crate::highlight::FgSpan`] carries the index, the renderer resolves
    /// the color here. Panics on an out-of-range index, exactly as the former direct
    /// `HIGHLIGHT_COLORS[idx]` lookup did — the index always comes from the bound capture space.
    pub fn syntax(&self, capture: usize) -> Color {
        self.syntax[capture]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::{capture_index, HIGHLIGHT_NAMES};

    #[test]
    fn syntax_template_is_parallel_with_the_capture_names() {
        assert_eq!(SYNTAX_SLOTS.len(), HIGHLIGHT_NAMES.len());
    }

    #[test]
    fn dark_syntax_resolves_representative_captures_to_the_historical_colors() {
        let theme = Theme::dark();
        let color = |name: &str| theme.syntax(capture_index(name).unwrap());
        // The exact C_* consts highlight.rs shipped in M3 (base16-eighties.dark accents).
        assert_eq!(color("keyword"), Color::Rgb(0xcc, 0x99, 0xcc)); // C_PURPLE / base0E
        assert_eq!(color("string"), Color::Rgb(0x99, 0xcc, 0x99)); // C_GREEN / base0B
        assert_eq!(color("comment"), Color::Rgb(0x74, 0x73, 0x69)); // C_COMMENT / base03
        assert_eq!(color("function"), Color::Rgb(0x66, 0x99, 0xcc)); // C_BLUE / base0D
        assert_eq!(color("number"), Color::Rgb(0xf9, 0x91, 0x57)); // C_ORANGE / base09
        assert_eq!(color("variable"), Color::Rgb(0xd3, 0xd0, 0xc8)); // C_FG / base05
    }

    #[test]
    fn dark_diff_tints_match_the_historical_constants() {
        // The pixel-identity gate: `Theme::dark` must reproduce M3–M5's hand-tuned tints exactly.
        // Pinned to the literals so a future refactor can't silently drift dark.
        let t = Theme::dark();
        assert_eq!(t.del_subtle, Color::Rgb(60, 24, 24));
        assert_eq!(t.del_strong, Color::Rgb(120, 40, 40));
        assert_eq!(t.add_subtle, Color::Rgb(20, 48, 24));
        assert_eq!(t.add_strong, Color::Rgb(32, 100, 48));
        assert_eq!(t.del_staged_subtle, Color::Rgb(42, 26, 28));
        assert_eq!(t.del_staged_strong, Color::Rgb(64, 38, 40));
        assert_eq!(t.add_staged_subtle, Color::Rgb(24, 34, 26));
        assert_eq!(t.add_staged_strong, Color::Rgb(34, 50, 38));
        assert_eq!(t.cursor_bg, Color::Rgb(45, 50, 90));
        assert_eq!(t.selection_bg, Color::Rgb(30, 66, 66));
        assert_eq!(t.outline_cursor_unfocused_bg, Color::Rgb(35, 38, 55));
    }
}
