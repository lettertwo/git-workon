//! The base16 color-scheme primitive and the colors the renderer resolves against it (ADR-035).
//!
//! This is the theming *primitive* — the resolved palette a frame is painted with — distinct from
//! [`crate::config::Theme`], which is the git-config *selection* (`auto`/`dark`/`light`). CS4 was
//! dark-only and behavior-preserving: [`Palette::dark`] reproduces M3–M5's hardcoded colors exactly.
//! CS5 adds [`Palette::light`] and wires [`crate::config::Theme`] to pick between them; CS6 adds the
//! terminal-derivation probe for `auto`.
//!
//! ## Hybrid boundary (ADR-035)
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
    /// from (`highlight.rs`'s `C_*` consts ARE these slots; see ADR-035). Reproduced here in full
    /// so `Palette::dark` is a faithful re-expression of the shipped dark colors.
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

    /// base16-one-light (Daniel Pfeifer, http://github.com/purpleKarrot) — a published base16
    /// LIGHT scheme (tinted-theming/schemes, `base16/one-light.yaml`), pasted verbatim per
    /// ADR-035 ("do NOT hand-invent accent colors"). base00 is near-white (the light background);
    /// base07 is the darkest ramp step (high-contrast fg on a light bg — base16's ramp direction
    /// is background→foreground, and "foreground" on a light scheme means dark).
    const ONE_LIGHT: Base16 = Base16 {
        slots: [
            Color::Rgb(0xfa, 0xfa, 0xfa), // base00 background
            Color::Rgb(0xf0, 0xf0, 0xf1), // base01
            Color::Rgb(0xe5, 0xe5, 0xe6), // base02
            Color::Rgb(0xa0, 0xa1, 0xa7), // base03 comments
            Color::Rgb(0x69, 0x6c, 0x77), // base04
            Color::Rgb(0x38, 0x3a, 0x42), // base05 foreground
            Color::Rgb(0x20, 0x22, 0x27), // base06
            Color::Rgb(0x09, 0x0a, 0x0b), // base07
            Color::Rgb(0xca, 0x12, 0x43), // base08 red / diff deleted
            Color::Rgb(0xd7, 0x5f, 0x00), // base09 orange
            Color::Rgb(0xc1, 0x84, 0x01), // base0A yellow
            Color::Rgb(0x50, 0xa1, 0x4f), // base0B green / diff inserted
            Color::Rgb(0x01, 0x84, 0xbc), // base0C cyan
            Color::Rgb(0x40, 0x78, 0xf2), // base0D blue
            Color::Rgb(0xa6, 0x26, 0xa4), // base0E purple / keyword
            Color::Rgb(0x98, 0x68, 0x01), // base0F brown
        ],
    };

    fn slot(&self, i: usize) -> Color {
        self.slots[i]
    }
}

/// Blend `color` toward `base` by `ratio` (`0.0` = `color` unchanged, `1.0` = `base`) — linear
/// interpolation per RGB channel. This is the "convex blend toward base00" derivation ADR-035
/// describes for a LIGHT base00: blending an accent toward a light background yields a pale,
/// correctly-hued wash (the dark scheme can't use this — see [`Palette::dark`]'s doc comment for
/// why dark tints are held explicit instead). Non-RGB colors pass through unblended.
fn tint_toward(color: Color, base: Color, ratio: f32) -> Color {
    match (color, base) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let lerp =
                |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * ratio).round() as u8 };
            Color::Rgb(lerp(r1, r2), lerp(g1, g2), lerp(b1, b2))
        }
        _ => color,
    }
}

/// Per-capture syntax template: each entry is the base16 slot index that the parallel
/// [`crate::highlight::HIGHLIGHT_NAMES`] capture maps to, per the base16 role conventions
/// (ADR-035). Palette-invariant — every scheme applies this same template to its own slots — so it
/// lives with the primitive, not on any one [`Palette`]. A theme switch re-colors by re-rendering:
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

/// The resolved on-tint palette a frame is painted with (ADR-035's theme-controlled half).
///
/// Syntax foreground is looked up per capture index via [`Palette::syntax`]; the diff-background
/// gradient, its staged variants, and the cursor/selection/outline washes are read directly. All
/// values in [`Palette::dark`] reproduce the M3–M5 hardcoded colors exactly (CS4 is a
/// behavior-preserving refactor).
pub struct Palette {
    /// Per-capture syntax fg, indexed by the same capture index as
    /// [`crate::highlight::HIGHLIGHT_NAMES`] (see [`SYNTAX_SLOTS`]).
    syntax: Vec<Color>,

    /// Whole-line subtle / word-level strong background for an unstaged (bright) Del cell.
    pub del_subtle: Color,
    pub del_strong: Color,
    /// Bright Add-cell background pair (counterpart of [`Palette::del_subtle`]).
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
    /// [`Palette::cursor_bg`].
    pub selection_bg: Color,
    /// Cursor wash for the outline pane while OPEN but NOT focused — dimmer than [`Palette::cursor_bg`].
    pub outline_cursor_unfocused_bg: Color,
}

impl Palette {
    /// The curated dark scheme: base16-eighties.dark accents + the M3–M5 hand-tuned diff/cursor
    /// tints, reproduced byte-for-byte (the pixel-identity gate — see the module doc and ADR-035).
    ///
    /// The diff-bg tints are held explicit rather than derived: a clean base08/base0B → base00
    /// blend cannot reproduce these particular hand-tuned constants (their green/blue channels sit
    /// *below* base00, so no convex blend toward base00 reaches them). ADR-035's derivation is
    /// therefore deferred to CS5, where the light scheme defines its own tints; dark keeps the
    /// shipped values verbatim.
    pub fn dark() -> Self {
        let base = Base16::EIGHTIES_DARK;
        Palette {
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

    /// The curated light scheme: base16-one-light accents (see [`Base16::ONE_LIGHT`]) with the
    /// diff/cursor tints DERIVED via [`tint_toward`], per ADR-035's corrected primitive section —
    /// blending an accent toward a *light* base00 gives the correct pale wash (unlike dark, which
    /// must hold its tints explicit; see [`Palette::dark`]'s doc comment).
    ///
    /// Ratios were hand-tuned against four requirements: subtle vs strong must read as visibly
    /// distinct steps, add vs green must be distinguishable at a glance, staged must read dimmer
    /// (more washed-out) than unstaged, and every wash must stay legible under the scheme's dark
    /// base05 foreground and accent text. The del/add pair (base08/base0B → base00) derived
    /// cleanly at those ratios; cursor/selection reuse the same mechanism against base0D/base0C
    /// (blue/cyan) for a cool wash appropriate on a light background.
    pub fn light() -> Self {
        let base = Base16::ONE_LIGHT;
        let base00 = base.slot(0);
        let red = base.slot(8); // base08
        let green = base.slot(11); // base0B
        let blue = base.slot(13); // base0D
        let cyan = base.slot(12); // base0C

        // Unstaged: a light wash (subtle) and a more saturated wash (strong) a reader's eye can
        // pick out at a glance; staged pushes further toward base00 (less saturated → dimmer).
        const SUBTLE: f32 = 0.88;
        const STRONG: f32 = 0.65;
        const STAGED_SUBTLE: f32 = 0.94;
        const STAGED_STRONG: f32 = 0.80;
        const CURSOR: f32 = 0.82;
        const OUTLINE_CURSOR_UNFOCUSED: f32 = 0.90;

        Palette {
            syntax: SYNTAX_SLOTS.iter().map(|&s| base.slot(s)).collect(),
            del_subtle: tint_toward(red, base00, SUBTLE),
            del_strong: tint_toward(red, base00, STRONG),
            add_subtle: tint_toward(green, base00, SUBTLE),
            add_strong: tint_toward(green, base00, STRONG),
            del_staged_subtle: tint_toward(red, base00, STAGED_SUBTLE),
            del_staged_strong: tint_toward(red, base00, STAGED_STRONG),
            add_staged_subtle: tint_toward(green, base00, STAGED_SUBTLE),
            add_staged_strong: tint_toward(green, base00, STAGED_STRONG),
            cursor_bg: tint_toward(blue, base00, CURSOR),
            selection_bg: tint_toward(cyan, base00, CURSOR),
            outline_cursor_unfocused_bg: tint_toward(blue, base00, OUTLINE_CURSOR_UNFOCUSED),
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

    /// Resolve the on-tint palette for a `workon.review.theme` selection (ADR-035/CS5). `Auto`
    /// falls back to dark for now — CS6 adds the terminal-derivation probe that gives `Auto` its
    /// real meaning. A config-read error is the caller's concern (see `main.rs`): this function
    /// only handles a successfully-parsed selection.
    pub fn for_theme(theme: crate::config::Theme) -> Self {
        match theme {
            crate::config::Theme::Light => Self::light(),
            crate::config::Theme::Dark => Self::dark(),
            crate::config::Theme::Auto => Self::dark(), // CS6: terminal-derive
        }
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
        let theme = Palette::dark();
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
        // The pixel-identity gate: `Palette::dark` must reproduce M3–M5's hand-tuned tints exactly.
        // Pinned to the literals so a future refactor can't silently drift dark.
        let t = Palette::dark();
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

    fn rgb(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected an RGB color, got {other:?}"),
        }
    }

    fn luminance(color: Color) -> u32 {
        let (r, g, b) = rgb(color);
        r as u32 + g as u32 + b as u32
    }

    /// Euclidean-ish distance (sum of absolute channel deltas) from `base00` — used as a proxy for
    /// "how washed-out toward the background is this tint," since [`tint_toward`] blends linearly.
    fn distance_from_base00(color: Color) -> u32 {
        let (r, g, b) = rgb(color);
        let (r0, g0, b0) = rgb(Base16::ONE_LIGHT.slot(0));
        r.abs_diff(r0) as u32 + g.abs_diff(g0) as u32 + b.abs_diff(b0) as u32
    }

    #[test]
    fn light_base00_is_high_luminance() {
        // A light scheme's background must be near-white, unlike dark's near-black base00.
        assert!(luminance(Base16::ONE_LIGHT.slot(0)) > luminance(Base16::EIGHTIES_DARK.slot(0)));
        assert!(luminance(Base16::ONE_LIGHT.slot(0)) > 600); // out of a 765 (255*3) max
    }

    #[test]
    fn light_del_and_add_tints_are_distinct_from_each_other() {
        let t = Palette::light();
        assert_ne!(t.del_subtle, t.add_subtle);
        assert_ne!(t.del_strong, t.add_strong);
        assert_ne!(t.del_staged_subtle, t.add_staged_subtle);
        assert_ne!(t.del_staged_strong, t.add_staged_strong);
    }

    #[test]
    fn light_subtle_and_strong_are_visibly_distinct_steps() {
        let t = Palette::light();
        assert_ne!(t.del_subtle, t.del_strong);
        assert_ne!(t.add_subtle, t.add_strong);
        // Strong sits further from base00 (more saturated / less washed-out) than subtle.
        assert!(distance_from_base00(t.del_strong) > distance_from_base00(t.del_subtle));
        assert!(distance_from_base00(t.add_strong) > distance_from_base00(t.add_subtle));
    }

    #[test]
    fn light_staged_reads_dimmer_than_unstaged() {
        // "Dimmer" == more washed toward base00 == closer to base00 than the unstaged pair.
        let t = Palette::light();
        assert!(distance_from_base00(t.del_staged_subtle) < distance_from_base00(t.del_subtle));
        assert!(distance_from_base00(t.del_staged_strong) < distance_from_base00(t.del_strong));
        assert!(distance_from_base00(t.add_staged_subtle) < distance_from_base00(t.add_subtle));
        assert!(distance_from_base00(t.add_staged_strong) < distance_from_base00(t.add_strong));
    }

    #[test]
    fn light_cursor_and_selection_washes_are_distinct_and_outline_cursor_is_dimmer() {
        let t = Palette::light();
        assert_ne!(t.cursor_bg, t.selection_bg);
        // The unfocused outline cursor wash should read dimmer than the focused cursor wash.
        assert!(
            distance_from_base00(t.outline_cursor_unfocused_bg) < distance_from_base00(t.cursor_bg)
        );
    }

    #[test]
    fn light_syntax_resolves_representative_captures_to_the_one_light_accents() {
        let theme = Palette::light();
        let color = |name: &str| theme.syntax(capture_index(name).unwrap());
        assert_eq!(color("keyword"), Color::Rgb(0xa6, 0x26, 0xa4)); // base0E purple
        assert_eq!(color("string"), Color::Rgb(0x50, 0xa1, 0x4f)); // base0B green
        assert_eq!(color("comment"), Color::Rgb(0xa0, 0xa1, 0xa7)); // base03
        assert_eq!(color("function"), Color::Rgb(0x40, 0x78, 0xf2)); // base0D blue
        assert_eq!(color("number"), Color::Rgb(0xd7, 0x5f, 0x00)); // base09 orange
        assert_eq!(color("variable"), Color::Rgb(0x38, 0x3a, 0x42)); // base05 fg
    }

    #[test]
    fn for_theme_selects_light_dark_and_falls_auto_back_to_dark() {
        use crate::config::Theme;

        assert_eq!(
            Palette::for_theme(Theme::Light).del_subtle,
            Palette::light().del_subtle
        );
        assert_ne!(
            Palette::for_theme(Theme::Light).del_subtle,
            Palette::dark().del_subtle
        );
        assert_eq!(
            Palette::for_theme(Theme::Dark).del_subtle,
            Palette::dark().del_subtle
        );
        // CS6: terminal-derive — Auto falls back to dark until the probe lands.
        assert_eq!(
            Palette::for_theme(Theme::Auto).del_subtle,
            Palette::dark().del_subtle
        );
    }
}
