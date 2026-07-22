//! The base16 color-scheme primitive and the colors the renderer resolves against it (ADR-029).
//!
//! This is the theming *primitive* — the resolved palette a frame is painted with — distinct from
//! [`crate::config::Theme`], which is the git-config *selection* (`auto`/`dark`/`light`). CS4 was
//! dark-only and behavior-preserving: [`Palette::dark`] reproduces M3–M5's hardcoded colors exactly.
//! CS5 adds [`Palette::light`] and wires [`crate::config::Theme`] to pick between them; CS6 adds the
//! terminal-derivation probe for `auto`.
//!
//! ## Hybrid boundary (ADR-029, twice-revised)
//! Colors that sit ON a tinted background — the diff add/del gradient, its staged variants, the
//! cursor/selection washes, and syntax foreground — are theme-controlled base16 truecolor and live
//! here, as before. The canvas background and chrome FOREGROUND (default text, dim labels, the
//! gutter) are now ALSO palette-ramp-controlled ([`Palette::background`]/[`Palette::foreground`]/
//! [`Palette::dim`]/[`Palette::gutter`]), so a curated (`light`/`dark`) theme fully controls the
//! look instead of bleeding the terminal's own bg/fg through. `auto` ([`Palette::from_terminal`])
//! still derives these four from the probed terminal colors — so it matches the terminal exactly —
//! and leaves [`Palette::paint_canvas`] `false` so a transparent/backgrounded terminal isn't
//! painted over; the curated schemes and the probe's curated fallback set it `true`.
//!
//! **CS2 revision:** semantic chrome — error/warn/current-marker colors — was previously ANSI/const
//! in `crate::render` (`FG_ERROR`/`FG_WARN`/`FG_CURRENT`), deliberately excluded from the palette on
//! the reasoning that these colors never sit on a tint and are never a theme knob. That boundary is
//! now revised: they ARE palette knobs ([`Palette::error_fg`]/[`Palette::warn_fg`]/
//! [`Palette::current_fg`], mapped to base08/base0A/base0B), so a curated or probed theme can shift
//! them too. `dark()` keeps the three shipped RGB values verbatim (the same pixel-identity
//! precedent as its diff/cursor tints); `light()` takes `ONE_LIGHT`'s base08/base0A/base0B;
//! `from_terminal()` takes the probed scheme's base08/base0A/base0B directly (matching the syntax
//! slots' reasoning; its diff washes also derive from the probed accents, while cursor/selection
//! borrow curated washes — see [`Palette::from_terminal`]'s doc comment).
//!
//! **CS1 addition (`outline-header-polish`):** [`Palette::heading_fg`] (base0C, cyan) is a fourth
//! semantic-chrome field, same reasoning and same three-scheme mapping as the CS2 trio above —
//! it's the outline's changeset-header-row accent, used only there (see
//! `render::changeset_title_spans`'s doc comment for the outline-only gating).
//!
//! **CS3 addition (`outline-status-xy`):** [`Palette::modified_fg`] (base09, orange/amber) is a
//! fifth semantic-chrome field, same three-scheme mapping again — the outline's committed-file
//! "modified" tint (M/R/C letters). Deliberately a NEW field rather than reusing
//! [`Palette::warn_fg`]: "this changeset needs restacking" and "this file was modified" are
//! unrelated facts that happen to both want an amber tone, and collapsing them onto one field
//! would make them un-independently themeable.
//!
//! **CS1 addition (`user-configurable colors tier`):** the deferred "user-configurable colors"
//! tier from this module's original doc comment lands as [`ThemeOverrides`] — per-slot
//! (`base00`–`base0f`) and per-tint (`del-subtle`, `cursor-bg`, …) git-config keys under
//! `workon.review.theme.*`, read by `config::ReviewConfig::theme_overrides` and applied via
//! [`Palette::apply_overrides`] on top of whichever base (`dark`/`light`/`auto`'s probe) was
//! already resolved. Named bundled schemes (`theme = solarized`) were explicitly deferred —
//! only the override-key tier landed; see ADR-029's CS1 revision note for the full table.
//!
//! **CS2 addition (`no-color-mono`):** the same tier's other deferred extra, `NO_COLOR` support,
//! lands as [`Palette::mono`] — an achromatic scheme `main.rs` substitutes, after theme
//! resolution AND override application, when `NO_COLOR` is set (env kill-switch: it wins over
//! any override). See [`Palette::mono`]'s doc comment for the fg-vs-wash split and ADR-029's
//! CS2 revision note.

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
    /// ADR-029 ("do NOT hand-invent accent colors"). base00 is near-white (the light background);
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

/// Parse a `workon.review.theme.*` color value: `#rrggbb` or bare `rrggbb`, six hex digits,
/// case-insensitive. Deliberately no 3-digit shorthand (`#fff`) — the config schema names only
/// the 6-digit form, so a shorthand is treated the same as any other malformed value: `None`,
/// which `config::ReviewConfig::theme_overrides` turns into an ignore-and-warn.
pub(crate) fn parse_hex_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?))
}

/// Per-slot base16 and per-tint color overrides, read from `workon.review.theme.*` git config
/// (CS1, user-configurable colors tier) and applied on top of an already-resolved [`Palette`] via
/// [`Palette::apply_overrides`]. `slots` is private — built only through [`ThemeOverrides::set_slot`]
/// so the 0–15 index invariant lives in one place; the 11 tint fields mirror
/// [`Palette`]'s diff/cursor tint fields verbatim (same names, kebab-case in config).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThemeOverrides {
    slots: [Option<Color>; 16],
    pub del_subtle: Option<Color>,
    pub del_strong: Option<Color>,
    pub add_subtle: Option<Color>,
    pub add_strong: Option<Color>,
    pub del_staged_subtle: Option<Color>,
    pub del_staged_strong: Option<Color>,
    pub add_staged_subtle: Option<Color>,
    pub add_staged_strong: Option<Color>,
    pub cursor_bg: Option<Color>,
    pub selection_bg: Option<Color>,
    pub cursor_unfocused_bg: Option<Color>,
    pub pane_header_focused_fg: Option<Color>,
    pub filler_fg: Option<Color>,
}

impl ThemeOverrides {
    /// Set the override for base16 slot `index` (0–15, i.e. `base00`–`base0f`). Panics on an
    /// out-of-range index — callers (`config::ReviewConfig::theme_overrides`) only reach this
    /// after validating the slot name parsed to `0..16`.
    pub fn set_slot(&mut self, index: usize, color: Color) {
        self.slots[index] = Some(color);
    }

    /// Whether no slot or tint override is set — used to skip [`Palette::apply_overrides`]
    /// entirely when `workon.review.theme.*` is unset (the common case). Compared against
    /// `default()` rather than enumerating fields so a future tint field can't be forgotten
    /// here silently.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Blend `color` toward `base` by `ratio` (`0.0` = `color` unchanged, `1.0` = `base`) — linear
/// interpolation per RGB channel. This is the "convex blend toward base00" derivation ADR-029
/// describes for a LIGHT base00: blending an accent toward a light background yields a pale,
/// correctly-hued wash (the dark scheme can't use this — see [`Palette::dark`]'s doc comment for
/// why dark tints are held explicit instead). Non-RGB colors pass through unblended.
pub(crate) fn tint_toward(color: Color, base: Color, ratio: f32) -> Color {
    match (color, base) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let lerp =
                |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * ratio).round() as u8 };
            Color::Rgb(lerp(r1, r2), lerp(g1, g2), lerp(b1, b2))
        }
        _ => color,
    }
}

/// Whether a background color reads as "light" — a sum-of-channels luminance proxy (matching the
/// reasoning in this module's tests) with the midpoint of the `0..=765` range as the threshold.
/// Used to pick which curated scheme's diff/cursor tints a probed or fallback theme borrows
/// (CS6): a probed dark background reuses [`Palette::dark`]'s hand-tuned tints, a light one reuses
/// [`Palette::light`]'s derived washes. A non-RGB color (never produced by the OSC probe) reads as
/// dark. `pub` (not `pub(crate)`) since CS2 (`no-color-mono`) also calls this from `main.rs`,
/// which depends on this lib crate externally, to pick [`Palette::mono`]'s ladder.
pub fn is_light_background(color: Color) -> bool {
    match color {
        Color::Rgb(r, g, b) => r as u32 + g as u32 + b as u32 > 382,
        _ => false,
    }
}

/// Per-capture syntax template: each entry is the base16 slot index that the parallel
/// [`crate::highlight::HIGHLIGHT_NAMES`] capture maps to, per the base16 role conventions
/// (ADR-029). Palette-invariant — every scheme applies this same template to its own slots — so it
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

/// Per-capture italics template, parallel to [`SYNTAX_SLOTS`] (the array length ties the two at
/// compile time): `true` for the captures rendered in italics under every scheme. Only comments
/// today — the near-universal editor convention (and the prototype's look) — carried as a
/// structural style like `render`'s BOLD chrome rather than a palette field: it isn't a color, so
/// it survives [`Palette::mono`]/`NO_COLOR` (where it becomes the only remaining comment marker)
/// and needs no per-theme value.
const SYNTAX_ITALICS: [bool; SYNTAX_SLOTS.len()] = {
    let mut italics = [false; SYNTAX_SLOTS.len()];
    italics[1] = true; // comment (index in `crate::highlight::HIGHLIGHT_NAMES`)
    italics
};

/// Whether a capture index renders in italics (see [`SYNTAX_ITALICS`]). Panics on an
/// out-of-range index, exactly as [`Palette::syntax`] does — the index always comes from the
/// bound capture space.
pub fn syntax_italic(capture: usize) -> bool {
    SYNTAX_ITALICS[capture]
}

/// The resolved on-tint palette a frame is painted with (ADR-029's theme-controlled half).
///
/// Syntax foreground is looked up per capture index via [`Palette::syntax`]; the diff-background
/// gradient, its staged variants, and the cursor/selection/outline washes are read directly. All
/// values in [`Palette::dark`] reproduce the M3–M5 hardcoded colors exactly (CS4 is a
/// behavior-preserving refactor).
#[derive(Clone)]
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
    /// Cursor wash for ANY pane's remembered cursor row while that pane does NOT hold focus —
    /// dimmer than [`Palette::cursor_bg`]. Originally the outline-only field
    /// `outline_cursor_unfocused_bg`; renamed (`unfocused-cursor-wash`) when the diff body and
    /// split halves adopted the same dim-when-unfocused model the outline already had.
    pub cursor_unfocused_bg: Color,

    /// Foreground for the ONE pane header/caption label that currently holds focus (CS1,
    /// `focused-pane-header` — locked decision #2). Defaults to [`Palette::foreground`] (base05);
    /// an unfocused label keeps [`Palette::dim`] instead — there is no separate "unfocused" field.
    /// [`crate::render`] always pairs this color with a structural, unconditional BOLD (locked
    /// decision #3), so under [`Palette::mono`] (where this and [`Palette::dim`] both collapse to
    /// `Color::Reset`) BOLD alone still marks the focused label.
    pub pane_header_focused_fg: Color,

    /// The screen/canvas background (base00) — painted by [`crate::render::render`] when
    /// [`Palette::paint_canvas`] is set, so a curated theme's background actually shows instead of
    /// the terminal's own.
    pub background: Color,
    /// Default text foreground (base05) — resolved by [`crate::render`] wherever text carries no
    /// syntax highlight.
    pub foreground: Color,
    /// Dim/comment-toned foreground (base03) — dim labels, gap markers, split captions.
    pub dim: Color,
    /// Gutter/divider foreground (base04) — line-number gutters and pane dividers.
    pub gutter: Color,
    /// Foreground for the deleted-gap filler hatch (`render`'s `Row::Filler` `╱` runs) — base01,
    /// the ramp slot nearest the background: the hatch is pure texture ("nothing on this side of
    /// the split"), not text, so it recedes behind even dim labels. Previously painted with
    /// [`Palette::dim`], which tied it to the comment tone (base03) and dragged the hatch
    /// brighter whenever comments were retuned; base02 (the next step up) still read too bright
    /// under `auto` on a terminal whose bright-black is a vivid accent rather than a gray (the
    /// probed ramp interpolates toward base03, so 2/3 of a bright accent is a bright hatch).
    pub filler_fg: Color,
    /// Footer text color for an [`crate::app::Severity::Error`] notice, a pending-discard confirm
    /// prompt, and a Failed changeset's marker/message — a clearly-red tone (base08). Promoted
    /// from `render.rs`'s `FG_ERROR` const (CS2, revising ADR-029's hybrid boundary — see this
    /// module's doc comment).
    pub error_fg: Color,
    /// Warning tone for a needs-restack marker (locked decision #9) — an amber (base0A), distinct
    /// from [`Palette::error_fg`]'s red: a stale-parent changeset is a heads-up to `gt restack`,
    /// not a failure. Promoted from `render.rs`'s `FG_WARN` const (CS2).
    pub warn_fg: Color,
    /// Tone for the outline's "this is the lib-marked `current` changeset" marker (locked
    /// decision #9's outline half) — a green (base0B), distinct from every other marker color so
    /// "current" reads unambiguously at a glance. Promoted from `render.rs`'s `FG_CURRENT` const
    /// (CS2).
    pub current_fg: Color,
    /// Accent tone for a changeset header row's label (CS1, `outline-header-polish`) — a cyan
    /// (base0C), distinct from [`Palette::current_fg`]'s green so "this is a section heading"
    /// reads independently of "this is the current changeset." Used ONLY by the outline's Header
    /// rows (`render::changeset_title_spans`'s `counter` param gates it) — the summary panel's
    /// changeset title keeps the plain [`Palette::foreground`] look.
    pub heading_fg: Color,
    /// Tone for a committed changeset's Modified/Renamed/Copied outline file-status letter (CS3,
    /// `outline-status-xy`) — an amber (base09), distinct from [`Palette::warn_fg`]'s amber
    /// (base0A) so "needs restack" and "modified" stay independently themeable even though both
    /// default to the same amber family. Used ONLY by the outline's committed-file status column
    /// (`render::committed_letter_color`).
    pub modified_fg: Color,
    /// Whether [`crate::render::render`] should paint the whole frame with [`Palette::background`]
    /// before drawing panes. `true` for the curated [`Palette::dark`]/[`Palette::light`] schemes
    /// (and the probe's curated fallback); `false` for [`Palette::from_terminal`], so `auto`
    /// preserves the terminal's own background (transparency, images) rather than flattening it.
    pub paint_canvas: bool,
    /// Whether this palette carries no hue (CS2's `NO_COLOR` follow-up, `no-color-mono`
    /// finding). `true` only for [`Palette::mono`]; `false` for every other constructor. Sources
    /// of color OUTSIDE the palette itself — namely [`crate::icons::icon_for_path`]'s hardcoded
    /// per-filetype `Rgb` — can't consult a palette field to know they should go achromatic, so
    /// `render.rs`'s icon paint sites check this flag directly and collapse to
    /// [`Palette::foreground`] when it's set, rather than trusting the icon's own color.
    pub colorless: bool,
}

/// The startup context [`crate::config::resolve_runtime`] needs to resolve a palette but can't
/// derive itself, because it's pure/I/O-free and doesn't own the tty. Built once in `main.rs`
/// (after the `theme = auto` probe, if one ran) and reused by every later `resolve_runtime` call —
/// including a config reload — so `auto` is never re-probed mid-session (re-probing needs the tty,
/// which the TUI owns once the alternate screen is live; a second conversation there would corrupt
/// input).
pub struct PaletteContext {
    /// Base palette to use when `theme = auto`: the startup probe's result, or the startup base for
    /// a non-`auto` launch (no probe ever ran, so this is just whatever `Dark`/`Light`/the
    /// config-read-error fallback resolved to). Never re-probed — a `theme` reload that switches
    /// TO `auto` reuses this cached base rather than asking the terminal again.
    pub auto_base: Palette,
    /// `NO_COLOR` was set in the environment at launch — mono wins over every override, applied
    /// last in [`crate::config::resolve_runtime`]'s ladder. Read once at startup (`main.rs`); a
    /// reload can't change it, since it isn't a config value.
    pub no_color: bool,
}

impl Palette {
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
            cursor_unfocused_bg: Color::Rgb(35, 38, 55),
            pane_header_focused_fg: base.slot(5),
            background: base.slot(0),
            foreground: base.slot(5),
            dim: base.slot(3),
            gutter: base.slot(4),
            filler_fg: base.slot(1),
            // The shipped M3–M5 semantic-chrome colors, reproduced verbatim (the pixel-identity
            // gate — CS2 promotes these from `render.rs` consts without changing a single value).
            error_fg: Color::Rgb(220, 60, 60),
            warn_fg: Color::Rgb(214, 158, 46),
            current_fg: Color::Rgb(96, 200, 128),
            // CS1: brand new (no historical `render.rs` const to reproduce), so this takes the
            // scheme's base0C directly rather than an authored literal.
            heading_fg: base.slot(12),
            // CS3: brand new, same reasoning as `heading_fg` above — takes the scheme's base09
            // directly rather than an authored literal.
            modified_fg: base.slot(9),
            paint_canvas: true,
            colorless: false,
        }
    }

    /// The curated light scheme: base16-one-light accents (see [`Base16::ONE_LIGHT`]) with the
    /// diff/cursor tints DERIVED via [`tint_toward`], per ADR-029's corrected primitive section —
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
        const CURSOR_UNFOCUSED: f32 = 0.90;

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
            cursor_unfocused_bg: tint_toward(blue, base00, CURSOR_UNFOCUSED),
            pane_header_focused_fg: base.slot(5),
            background: base.slot(0),
            foreground: base.slot(5),
            dim: base.slot(3),
            gutter: base.slot(4),
            filler_fg: base.slot(1),
            error_fg: red,
            warn_fg: base.slot(10), // base0A
            current_fg: green,
            heading_fg: cyan,
            modified_fg: base.slot(9), // base09
            paint_canvas: true,
            colorless: false,
        }
    }

    /// A scheme derived from the terminal's own colors (ADR-029's `auto`, CS6). The 16 base16
    /// slots come from the probed [`Base16`] (built from the terminal's ANSI palette + background;
    /// see [`crate::terminal_query`]), so **syntax matches the terminal**.
    ///
    /// The **diff washes are derived from the probed accents** (the ADR-029 derived-washes
    /// addendum, revising the CS6 curated-tints refinement): del washes blend probed base08 (ANSI
    /// red) toward the probed background, add washes probed base0B (ANSI green) — the same
    /// `accent:mix(bg, 90)` arithmetic terminal theme authors use for their own editor diff
    /// backgrounds (laserwave's `BG_DELETE`/`BG_ADD`, the dogfood reference), so `auto`'s washes
    /// carry the terminal theme's hues instead of a generic red/green. Ratios are
    /// luminance-picked: a probed light background reuses [`Palette::light`]'s hand-tuned set; a
    /// dark one uses the dogfood-validated 10%/25% accent mixes (staged pushed further toward the
    /// background, preserving locked decision #7's "staged reads dimmer").
    ///
    /// The **cursor/selection washes stay curated by luminance**: they have no counterpart in a
    /// terminal theme's ANSI palette (deriving them from probed blue/cyan gives, e.g., a teal
    /// cursor row on an aqua-leaning theme), so per-theme judgment there belongs to the override
    /// tier, not derivation.
    pub fn from_terminal(base: Base16) -> Self {
        let background = base.slot(0);
        let light = is_light_background(background);
        let curated = if light {
            Palette::light()
        } else {
            Palette::dark()
        };
        let red = base.slot(8); // base08 — probed ANSI red
        let green = base.slot(11); // base0B — probed ANSI green
                                   // (subtle, strong, staged_subtle, staged_strong): how far each wash blends from the
                                   // accent toward the probed background. Light reuses `Palette::light`'s tuned ratios.
        let (subtle, strong, staged_subtle, staged_strong) = if light {
            (0.88, 0.65, 0.94, 0.80)
        } else {
            (0.90, 0.75, 0.94, 0.85)
        };
        Palette {
            syntax: SYNTAX_SLOTS.iter().map(|&s| base.slot(s)).collect(),
            del_subtle: tint_toward(red, background, subtle),
            del_strong: tint_toward(red, background, strong),
            add_subtle: tint_toward(green, background, subtle),
            add_strong: tint_toward(green, background, strong),
            del_staged_subtle: tint_toward(red, background, staged_subtle),
            del_staged_strong: tint_toward(red, background, staged_strong),
            add_staged_subtle: tint_toward(green, background, staged_subtle),
            add_staged_strong: tint_toward(green, background, staged_strong),
            cursor_bg: curated.cursor_bg,
            selection_bg: curated.selection_bg,
            cursor_unfocused_bg: curated.cursor_unfocused_bg,
            // Derived straight from the probed terminal scheme (NOT the curated fallback) — this
            // is the whole point of `auto`: chrome that matches the terminal's own colors.
            background: base.slot(0),
            foreground: base.slot(5),
            pane_header_focused_fg: base.slot(5),
            dim: base.slot(3),
            gutter: base.slot(4),
            filler_fg: base.slot(1),
            // Semantic chrome also matches the terminal — probed base08/base0A/base0B, not the
            // curated fallback's (mirrors the syntax slots' reasoning just above).
            error_fg: base.slot(8),
            warn_fg: base.slot(10),
            current_fg: base.slot(11),
            heading_fg: base.slot(12),
            modified_fg: base.slot(9),
            // Unlike the curated schemes, `auto` must NOT paint over the terminal's own
            // background — base00 here IS the probed terminal bg, so painting a solid canvas
            // would defeat terminal transparency/background images for no benefit (the probed
            // fg/dim/gutter already match the inherited bg, since they came from the same probe).
            paint_canvas: false,
            colorless: false,
        }
    }

    /// The achromatic scheme used when `NO_COLOR` is set (CS2, NO_COLOR support, `no-color.org`).
    /// Every fg field (`foreground`/`dim`/`gutter`/`error_fg`/`warn_fg`/`current_fg`/
    /// `heading_fg`/`modified_fg`), every [`Palette::syntax`] entry, and [`Palette::background`]
    /// collapse to `Color::Reset` — the terminal's own default fg/bg, nothing painted
    /// (`paint_canvas: false`, since Reset already means "don't touch"). `render.rs` has no
    /// non-color channel (reverse/dim modifiers) to fall back on for the 11 diff/cursor washes —
    /// adding one is a render.rs change, out of CS2's scope (`render.rs` must not change) — so
    /// those instead become achromatic (`r == g == b`) `Rgb` grayscale ladders: near-black for a
    /// dark terminal, near-white for a light one (picked by `light`, matching `main.rs`'s
    /// `is_light_background(theme.background)` call on the pre-mono base so `auto`'s probe still
    /// picks the right ladder). Hand-tuned to preserve the same three invariants the curated
    /// schemes maintain: subtle vs strong read as distinct steps, staged reads dimmer (closer to
    /// the implied background) than unstaged, and cursor vs selection are distinct. Add and Del
    /// share one ladder — colorless mode can't carry add-vs-del by hue, so that distinction
    /// falls to gutter glyph/structure instead, an accepted, documented degradation (see
    /// ADR-029's NO_COLOR note).
    pub fn mono(light: bool) -> Self {
        // (subtle, strong, staged_subtle, staged_strong, cursor, selection, cursor_unfocused)
        let (subtle, strong, staged_subtle, staged_strong, cursor, selection, cursor_unfocused) =
            if light {
                (
                    Color::Rgb(215, 215, 215),
                    Color::Rgb(165, 165, 165),
                    Color::Rgb(230, 230, 230),
                    Color::Rgb(205, 205, 205),
                    Color::Rgb(190, 190, 190),
                    Color::Rgb(200, 200, 200),
                    Color::Rgb(210, 210, 210),
                )
            } else {
                (
                    Color::Rgb(40, 40, 40),
                    Color::Rgb(90, 90, 90),
                    Color::Rgb(25, 25, 25),
                    Color::Rgb(50, 50, 50),
                    Color::Rgb(65, 65, 65),
                    Color::Rgb(55, 55, 55),
                    Color::Rgb(45, 45, 45),
                )
            };

        Palette {
            syntax: vec![Color::Reset; SYNTAX_SLOTS.len()],
            del_subtle: subtle,
            del_strong: strong,
            add_subtle: subtle,
            add_strong: strong,
            del_staged_subtle: staged_subtle,
            del_staged_strong: staged_strong,
            add_staged_subtle: staged_subtle,
            add_staged_strong: staged_strong,
            cursor_bg: cursor,
            selection_bg: selection,
            cursor_unfocused_bg: cursor_unfocused,
            pane_header_focused_fg: Color::Reset,
            background: Color::Reset,
            foreground: Color::Reset,
            dim: Color::Reset,
            gutter: Color::Reset,
            filler_fg: Color::Reset,
            error_fg: Color::Reset,
            warn_fg: Color::Reset,
            current_fg: Color::Reset,
            heading_fg: Color::Reset,
            modified_fg: Color::Reset,
            paint_canvas: false,
            colorless: true,
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

    /// Resolve the on-tint palette for a `workon.review.theme` selection (ADR-029/CS5) — the
    /// **I/O-free** cases. `Light`/`Dark` return their curated schemes. `Auto` is the terminal
    /// probe's job ([`crate::terminal_query::detect_auto_palette`], CS6), which needs tty access
    /// this pure function can't have; `main.rs` routes `Auto` there and only falls through to this
    /// function's dark result if it declines to probe. A config-read error is likewise the
    /// caller's concern (see `main.rs`): this handles only a successfully-parsed selection.
    pub fn for_theme(theme: crate::config::Theme) -> Self {
        match theme {
            crate::config::Theme::Light => Self::light(),
            crate::config::Theme::Dark => Self::dark(),
            crate::config::Theme::Auto => Self::dark(), // probe lives in main.rs/terminal_query
        }
    }

    /// Apply `workon.review.theme.*` overrides on top of an already-resolved palette (CS1,
    /// user-configurable colors tier) — works the same on ANY base (`dark`/`light`/`auto`'s
    /// probe result), applied last in `main.rs`'s resolution chain.
    ///
    /// **Uniform slot rule:** a slot override rewrites every palette field role-mapped to that
    /// slot, regardless of which base authored the field's current value — base00 →
    /// [`Palette::background`] (and sets [`Palette::paint_canvas`], so an explicitly chosen
    /// background always paints, even under `auto`, which otherwise leaves the canvas
    /// unpainted), base03 → [`Palette::dim`], base04 → [`Palette::gutter`], base05 →
    /// [`Palette::foreground`], base08 → [`Palette::error_fg`], base09 → [`Palette::modified_fg`],
    /// base0A → [`Palette::warn_fg`], base0B → [`Palette::current_fg`], base0C →
    /// [`Palette::heading_fg`], plus every [`Palette::syntax`] entry whose [`SYNTAX_SLOTS`]
    /// template maps to that slot. This is deliberately uniform rather than "only override
    /// fields the base didn't hand-author": the alternative (silently ignoring a slot override
    /// for `dark()`'s hand-tuned `error_fg`) is the UX trap — a user who sets `base08` expects
    /// red to change, full stop.
    ///
    /// Slot overrides do NOT re-derive the diff/cursor tints — that stays the 11 tint override
    /// keys' job, applied last and verbatim below, so a slot override can't silently reshape a
    /// hand-tuned wash it wasn't asked to touch.
    pub fn apply_overrides(&mut self, overrides: &ThemeOverrides) {
        for (capture, &slot) in SYNTAX_SLOTS.iter().enumerate() {
            if let Some(color) = overrides.slots[slot] {
                self.syntax[capture] = color;
            }
        }

        if let Some(color) = overrides.slots[0] {
            self.background = color;
            self.paint_canvas = true;
        }
        if let Some(color) = overrides.slots[3] {
            self.dim = color;
        }
        if let Some(color) = overrides.slots[4] {
            self.gutter = color;
        }
        if let Some(color) = overrides.slots[5] {
            self.foreground = color;
        }
        if let Some(color) = overrides.slots[8] {
            self.error_fg = color;
        }
        if let Some(color) = overrides.slots[9] {
            self.modified_fg = color;
        }
        if let Some(color) = overrides.slots[10] {
            self.warn_fg = color;
        }
        if let Some(color) = overrides.slots[11] {
            self.current_fg = color;
        }
        if let Some(color) = overrides.slots[12] {
            self.heading_fg = color;
        }

        // Tint overrides assign last and verbatim — unaffected by any slot override above.
        if let Some(color) = overrides.del_subtle {
            self.del_subtle = color;
        }
        if let Some(color) = overrides.del_strong {
            self.del_strong = color;
        }
        if let Some(color) = overrides.add_subtle {
            self.add_subtle = color;
        }
        if let Some(color) = overrides.add_strong {
            self.add_strong = color;
        }
        if let Some(color) = overrides.del_staged_subtle {
            self.del_staged_subtle = color;
        }
        if let Some(color) = overrides.del_staged_strong {
            self.del_staged_strong = color;
        }
        if let Some(color) = overrides.add_staged_subtle {
            self.add_staged_subtle = color;
        }
        if let Some(color) = overrides.add_staged_strong {
            self.add_staged_strong = color;
        }
        if let Some(color) = overrides.cursor_bg {
            self.cursor_bg = color;
        }
        if let Some(color) = overrides.selection_bg {
            self.selection_bg = color;
        }
        if let Some(color) = overrides.cursor_unfocused_bg {
            self.cursor_unfocused_bg = color;
        }
        if let Some(color) = overrides.pane_header_focused_fg {
            self.pane_header_focused_fg = color;
        }
        if let Some(color) = overrides.filler_fg {
            self.filler_fg = color;
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
        assert_eq!(t.cursor_unfocused_bg, Color::Rgb(35, 38, 55));
    }

    #[test]
    fn dark_semantic_fg_matches_the_historical_render_rs_constants() {
        // CS2's pixel-identity gate for the promoted `FG_ERROR`/`FG_WARN`/`FG_CURRENT` consts.
        let t = Palette::dark();
        assert_eq!(t.error_fg, Color::Rgb(220, 60, 60));
        assert_eq!(t.warn_fg, Color::Rgb(214, 158, 46));
        assert_eq!(t.current_fg, Color::Rgb(96, 200, 128));
    }

    #[test]
    fn dark_heading_fg_takes_the_eighties_dark_cyan_accent() {
        // CS1: no historical constant to reproduce (this field is new) — unlike
        // `dark_semantic_fg_matches_the_historical_render_rs_constants` above, it takes base0C
        // straight from the scheme.
        let t = Palette::dark();
        assert_eq!(t.heading_fg, Color::Rgb(0x66, 0xcc, 0xcc)); // base0C
    }

    #[test]
    fn dark_modified_fg_takes_the_eighties_dark_orange_accent() {
        // CS3: no historical constant to reproduce (this field is new, same reasoning as
        // `dark_heading_fg_takes_the_eighties_dark_cyan_accent` above) — takes base09 straight
        // from the scheme.
        let t = Palette::dark();
        assert_eq!(t.modified_fg, Color::Rgb(0xf9, 0x91, 0x57)); // base09
        assert_ne!(
            t.modified_fg, t.warn_fg,
            "modified_fg must stay independently themeable from warn_fg"
        );
    }

    #[test]
    fn dark_chrome_fields_match_the_eighties_dark_ramp_and_paint_the_canvas() {
        // `dark()`'s canvas/chrome must come from the SAME ramp `Palette::dark`'s syntax/tints
        // already use (base00/base03/base04/base05), and must paint (a curated theme fully
        // controls the look — see the theme module's revised hybrid-boundary doc comment).
        let t = Palette::dark();
        assert_eq!(t.background, Color::Rgb(0x2d, 0x2d, 0x2d)); // base00
        assert_eq!(t.foreground, Color::Rgb(0xd3, 0xd0, 0xc8)); // base05
        assert_eq!(t.dim, Color::Rgb(0x74, 0x73, 0x69)); // base03
        assert_eq!(t.gutter, Color::Rgb(0xa0, 0x9f, 0x93)); // base04
        assert!(t.paint_canvas);
    }

    #[test]
    fn pane_header_focused_fg_defaults_to_foreground_and_is_distinct_from_dim() {
        // CS1 (`focused-pane-header`, locked decision #2): the new tint field defaults to the
        // normal foreground, not an independently authored color — and it must read distinct from
        // `dim` (the unfocused label's color) in every curated/probed scheme, mirroring the
        // contrast checks around `mono_washes_are_achromatic_and_preserve_the_curated_invariants`.
        for t in [Palette::dark(), Palette::light()] {
            assert_eq!(t.pane_header_focused_fg, t.foreground);
            assert_ne!(t.pane_header_focused_fg, t.dim);
        }
    }

    #[test]
    fn light_background_is_high_luminance_and_foreground_is_low_luminance() {
        // A real light theme: a near-white canvas with dark text on it, and it must paint (an
        // unpainted canvas would let the terminal's own dark bg bleed through, the exact bug this
        // fix addresses).
        let t = Palette::light();
        assert_eq!(t.background, Color::Rgb(0xfa, 0xfa, 0xfa)); // base00
        assert_eq!(t.foreground, Color::Rgb(0x38, 0x3a, 0x42)); // base05
        assert_eq!(t.dim, Color::Rgb(0xa0, 0xa1, 0xa7)); // base03
        assert_eq!(t.gutter, Color::Rgb(0x69, 0x6c, 0x77)); // base04
        assert!(luminance(t.background) > luminance(t.foreground));
        assert!(t.paint_canvas);
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
    fn light_cursor_and_selection_washes_are_distinct_and_unfocused_cursor_is_dimmer() {
        let t = Palette::light();
        assert_ne!(t.cursor_bg, t.selection_bg);
        // The unfocused cursor wash should read dimmer than the focused cursor wash.
        assert!(distance_from_base00(t.cursor_unfocused_bg) < distance_from_base00(t.cursor_bg));
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
    fn light_semantic_fg_takes_one_lights_base08_base0a_base0b() {
        let t = Palette::light();
        assert_eq!(t.error_fg, Color::Rgb(0xca, 0x12, 0x43)); // base08
        assert_eq!(t.warn_fg, Color::Rgb(0xc1, 0x84, 0x01)); // base0A
        assert_eq!(t.current_fg, Color::Rgb(0x50, 0xa1, 0x4f)); // base0B
    }

    #[test]
    fn light_heading_fg_takes_one_lights_cyan_accent() {
        let t = Palette::light();
        assert_eq!(t.heading_fg, Color::Rgb(0x01, 0x84, 0xbc)); // base0C
    }

    #[test]
    fn light_modified_fg_takes_one_lights_orange_accent() {
        let t = Palette::light();
        assert_eq!(t.modified_fg, Color::Rgb(0xd7, 0x5f, 0x00)); // base09
        assert_ne!(
            t.modified_fg, t.warn_fg,
            "modified_fg must stay independently themeable from warn_fg"
        );
    }

    /// A synthetic probed scheme with a distinct value in every slot and the given `base00`, so a
    /// test can assert `from_terminal`'s syntax slots came from the probed scheme (not a curated
    /// one) and read the base00 luminance branch.
    fn probed_base16(base00: Color) -> Base16 {
        let mut slots = [Color::Rgb(0, 0, 0); 16];
        for (i, slot) in slots.iter_mut().enumerate() {
            // A unique, recognizable color per slot: R channel = slot index * 16.
            *slot = Color::Rgb((i as u8) * 16, 0x20, 0x40);
        }
        slots[0] = base00;
        Base16 { slots }
    }

    #[test]
    fn filler_fg_is_base01_the_ramp_slot_nearest_the_background() {
        // The deleted-gap hatch recedes behind dim text: base01 (the bg-nearest ramp slot),
        // decoupled from base03 so a retuned comment/dim tone no longer drags the hatch with it.
        // Holds in every scheme, including `auto`'s probed ramp.
        let lum = |c: Color| match c {
            Color::Rgb(r, g, b) => r as u32 + g as u32 + b as u32,
            other => panic!("expected RGB, got {other:?}"),
        };
        for palette in [
            Palette::dark(),
            Palette::from_terminal(probed_base16(Color::Rgb(0x1a, 0x1a, 0x1a))),
        ] {
            assert!(
                lum(palette.filler_fg) < lum(palette.dim),
                "dark-scheme filler hatch must sit closer to the background than dim"
            );
        }
        assert_eq!(Palette::dark().filler_fg, Base16::EIGHTIES_DARK.slot(1));
        assert_eq!(Palette::light().filler_fg, Base16::ONE_LIGHT.slot(1));
    }

    #[test]
    fn only_the_comment_capture_renders_italic() {
        // Guards SYNTAX_ITALICS's hardcoded index against HIGHLIGHT_NAMES reordering: the italic
        // entry must be the one "comment" resolves to, and representative neighbors stay upright.
        assert!(syntax_italic(capture_index("comment").unwrap()));
        assert!(!syntax_italic(capture_index("keyword").unwrap()));
        assert!(!syntax_italic(capture_index("string").unwrap()));
        assert!(!syntax_italic(capture_index("attribute").unwrap()));
    }

    #[test]
    fn from_terminal_takes_syntax_from_the_probed_scheme() {
        let probed = probed_base16(Color::Rgb(0x1a, 0x1a, 0x1a)); // dark bg
        let palette = Palette::from_terminal(probed);
        // keyword → base0E (slot 14): the probed scheme's slot, NOT a curated palette's.
        assert_eq!(
            palette.syntax(capture_index("keyword").unwrap()),
            probed.slot(14)
        );
        assert_eq!(
            palette.syntax(capture_index("string").unwrap()),
            probed.slot(11) // base0B
        );
        assert_ne!(
            palette.syntax(capture_index("keyword").unwrap()),
            Palette::dark().syntax(capture_index("keyword").unwrap())
        );
    }

    #[test]
    fn from_terminal_derives_diff_washes_from_the_probed_accents() {
        // The ADR-029 derived-washes addendum: del/add washes blend the PROBED base08/base0B
        // toward the PROBED background — theme-author arithmetic (`accent:mix(bg, 90)`), not the
        // curated fallback's generic red/green.
        let bg = Color::Rgb(0x1a, 0x1a, 0x1a); // dark
        let probed = probed_base16(bg);
        let palette = Palette::from_terminal(probed);
        let red = probed.slot(8);
        let green = probed.slot(11);
        assert_eq!(palette.del_subtle, tint_toward(red, bg, 0.90));
        assert_eq!(palette.del_strong, tint_toward(red, bg, 0.75));
        assert_eq!(palette.add_subtle, tint_toward(green, bg, 0.90));
        assert_eq!(palette.add_strong, tint_toward(green, bg, 0.75));
        assert_ne!(palette.del_subtle, Palette::dark().del_subtle);
    }

    #[test]
    fn from_terminal_staged_washes_read_dimmer_than_unstaged() {
        // Locked decision #7 survives derivation: a staged wash sits closer to the background
        // than its unstaged counterpart (a strictly larger blend toward bg).
        let bg = Color::Rgb(0x1a, 0x1a, 0x1a);
        let probed = probed_base16(bg);
        let palette = Palette::from_terminal(probed);
        let red = probed.slot(8);
        let green = probed.slot(11);
        assert_eq!(palette.del_staged_subtle, tint_toward(red, bg, 0.94));
        assert_eq!(palette.del_staged_strong, tint_toward(red, bg, 0.85));
        assert_eq!(palette.add_staged_subtle, tint_toward(green, bg, 0.94));
        assert_eq!(palette.add_staged_strong, tint_toward(green, bg, 0.85));
        assert_ne!(palette.del_staged_subtle, palette.del_subtle);
        assert_ne!(palette.add_staged_strong, palette.add_strong);
    }

    #[test]
    fn from_terminal_with_a_light_background_derives_with_lights_ratios() {
        // A probed LIGHT background reuses `Palette::light`'s hand-tuned blend ratios, applied
        // to the probed accents.
        let bg = Color::Rgb(0xf5, 0xf5, 0xf5);
        let probed = probed_base16(bg);
        let palette = Palette::from_terminal(probed);
        assert_eq!(palette.del_subtle, tint_toward(probed.slot(8), bg, 0.88));
        assert_eq!(palette.add_strong, tint_toward(probed.slot(11), bg, 0.65));
    }

    #[test]
    fn from_terminal_with_a_dark_background_borrows_darks_curated_cursor_washes() {
        // Cursor/selection stay curated-by-luminance (no ANSI counterpart to derive from) even
        // though the diff washes now derive.
        let palette = Palette::from_terminal(probed_base16(Color::Rgb(0x1a, 0x1a, 0x1a)));
        let dark = Palette::dark();
        assert_eq!(palette.cursor_bg, dark.cursor_bg);
        assert_eq!(palette.selection_bg, dark.selection_bg);
        assert_eq!(palette.cursor_unfocused_bg, dark.cursor_unfocused_bg);
    }

    #[test]
    fn from_terminal_with_a_light_background_borrows_lights_curated_cursor_washes() {
        let palette = Palette::from_terminal(probed_base16(Color::Rgb(0xf5, 0xf5, 0xf5)));
        let light = Palette::light();
        assert_eq!(palette.cursor_bg, light.cursor_bg);
        assert_eq!(palette.selection_bg, light.selection_bg);
        // ...and NOT dark's, confirming the luminance branch flipped.
        assert_ne!(palette.cursor_bg, Palette::dark().cursor_bg);
    }

    #[test]
    fn from_terminal_takes_chrome_from_the_probed_scheme_and_does_not_paint() {
        // `auto`'s canvas/chrome must come from the PROBED scheme (so it matches the terminal),
        // and must NOT paint — the terminal's own background stays, preserving transparency (see
        // the theme module's revised hybrid-boundary doc comment).
        let probed = probed_base16(Color::Rgb(0x1a, 0x1a, 0x1a));
        let palette = Palette::from_terminal(probed);
        assert_eq!(palette.background, probed.slot(0));
        assert_eq!(palette.foreground, probed.slot(5));
        assert_eq!(palette.dim, probed.slot(3));
        assert_eq!(palette.gutter, probed.slot(4));
        assert!(!palette.paint_canvas);
    }

    #[test]
    fn from_terminal_takes_semantic_fg_from_the_probed_scheme_not_the_curated_fallback() {
        // Same reasoning as syntax/chrome: `auto`'s error/warn/current colors should match the
        // terminal, not borrow the curated dark/light fallback's (unlike the cursor/selection
        // washes, which DO borrow — see
        // `from_terminal_with_a_dark_background_borrows_darks_curated_cursor_washes`).
        let probed = probed_base16(Color::Rgb(0x1a, 0x1a, 0x1a));
        let palette = Palette::from_terminal(probed);
        assert_eq!(palette.error_fg, probed.slot(8));
        assert_eq!(palette.warn_fg, probed.slot(10));
        assert_eq!(palette.current_fg, probed.slot(11));
        assert_eq!(palette.heading_fg, probed.slot(12));
        assert_eq!(palette.modified_fg, probed.slot(9));
        assert_ne!(palette.error_fg, Palette::dark().error_fg);
    }

    #[test]
    fn is_light_background_splits_on_the_luminance_midpoint() {
        assert!(is_light_background(Base16::ONE_LIGHT.slot(0)));
        assert!(!is_light_background(Base16::EIGHTIES_DARK.slot(0)));
        // A non-RGB color (never produced by the probe) reads as dark.
        assert!(!is_light_background(Color::Gray));
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

    #[test]
    fn parse_hex_color_accepts_hash_and_bare_six_digit_hex() {
        assert_eq!(
            parse_hex_color("#2d2d2d"),
            Some(Color::Rgb(0x2d, 0x2d, 0x2d))
        );
        assert_eq!(
            parse_hex_color("2d2d2d"),
            Some(Color::Rgb(0x2d, 0x2d, 0x2d))
        );
    }

    #[test]
    fn parse_hex_color_rejects_shorthand_invalid_and_empty() {
        assert_eq!(parse_hex_color("#fff"), None, "3-digit shorthand rejected");
        assert_eq!(parse_hex_color("2d2d2g"), None, "non-hex digit rejected");
        assert_eq!(parse_hex_color(""), None, "empty rejected");
    }

    #[test]
    fn empty_overrides_is_an_identity_on_dark() {
        // Pixel-identity precedent (same as `dark_diff_tints_match_the_historical_constants`):
        // an empty `ThemeOverrides` must leave every field untouched.
        let overrides = ThemeOverrides::default();
        assert!(overrides.is_empty());
        let mut t = Palette::dark();
        let before = Palette::dark();
        t.apply_overrides(&overrides);
        assert_eq!(t.background, before.background);
        assert_eq!(t.foreground, before.foreground);
        assert_eq!(t.error_fg, before.error_fg);
        assert_eq!(t.heading_fg, before.heading_fg);
        assert_eq!(t.del_subtle, before.del_subtle);
        assert_eq!(t.cursor_bg, before.cursor_bg);
        assert_eq!(t.paint_canvas, before.paint_canvas);
        assert_eq!(
            t.syntax(capture_index("keyword").unwrap()),
            before.syntax(capture_index("keyword").unwrap())
        );
    }

    #[test]
    fn apply_overrides_base0e_recolors_the_keyword_syntax_capture() {
        let mut overrides = ThemeOverrides::default();
        overrides.set_slot(14, Color::Rgb(0x11, 0x22, 0x33)); // base0E → keyword
        let mut t = Palette::dark();
        t.apply_overrides(&overrides);
        assert_eq!(
            t.syntax(capture_index("keyword").unwrap()),
            Color::Rgb(0x11, 0x22, 0x33)
        );
        // Unrelated captures are untouched.
        assert_eq!(
            t.syntax(capture_index("string").unwrap()),
            Palette::dark().syntax(capture_index("string").unwrap())
        );
    }

    #[test]
    fn apply_overrides_base08_rewrites_error_fg_even_on_darks_hand_authored_value() {
        // The uniform rule (see `Palette::apply_overrides`'s doc comment): a slot override
        // rewrites its role-mapped field even when the base authored that field explicitly
        // (`dark()`'s `error_fg` is a hand-tuned literal, not derived from base08).
        let mut overrides = ThemeOverrides::default();
        overrides.set_slot(8, Color::Rgb(0xaa, 0xbb, 0xcc)); // base08 → error_fg
        let mut t = Palette::dark();
        t.apply_overrides(&overrides);
        assert_eq!(t.error_fg, Color::Rgb(0xaa, 0xbb, 0xcc));
        assert_ne!(t.error_fg, Palette::dark().error_fg);
    }

    #[test]
    fn apply_overrides_base00_on_a_from_terminal_palette_sets_paint_canvas() {
        // `auto`'s probe result leaves `paint_canvas: false`; an explicit base00 override means
        // the user chose a background, so it must paint even under `auto`.
        let probed = probed_base16(Color::Rgb(0x1a, 0x1a, 0x1a));
        let mut t = Palette::from_terminal(probed);
        assert!(!t.paint_canvas);
        let mut overrides = ThemeOverrides::default();
        overrides.set_slot(0, Color::Rgb(0x10, 0x10, 0x10));
        t.apply_overrides(&overrides);
        assert_eq!(t.background, Color::Rgb(0x10, 0x10, 0x10));
        assert!(t.paint_canvas);
    }

    #[test]
    fn apply_overrides_tint_lands_verbatim_unaffected_by_slot_overrides() {
        let mut overrides = ThemeOverrides::default();
        overrides.set_slot(8, Color::Rgb(0xaa, 0xbb, 0xcc)); // base08 → error_fg, NOT del_subtle
        overrides.cursor_bg = Some(Color::Rgb(0x01, 0x02, 0x03));
        let mut t = Palette::dark();
        t.apply_overrides(&overrides);
        assert_eq!(t.cursor_bg, Color::Rgb(0x01, 0x02, 0x03));
        // The del/add tints are untouched by the base08 slot override — tint overrides are the
        // only thing that moves them.
        assert_eq!(t.del_subtle, Palette::dark().del_subtle);
    }

    #[test]
    fn mono_fg_and_syntax_fields_are_all_reset() {
        let t = Palette::mono(false);
        assert_eq!(t.background, Color::Reset);
        assert_eq!(t.foreground, Color::Reset);
        assert_eq!(t.dim, Color::Reset);
        assert_eq!(t.gutter, Color::Reset);
        assert_eq!(t.error_fg, Color::Reset);
        assert_eq!(t.warn_fg, Color::Reset);
        assert_eq!(t.current_fg, Color::Reset);
        assert_eq!(t.heading_fg, Color::Reset);
        assert_eq!(t.modified_fg, Color::Reset);
        assert!(!t.paint_canvas);
        for capture in 0..syntax_slot_count() {
            assert_eq!(
                t.syntax(capture),
                Color::Reset,
                "capture {capture} not Reset"
            );
        }
    }

    /// A wash must be an achromatic (`r == g == b`) `Rgb` — never `Reset`, since the 11 washes
    /// still need to carry cursor/selection/staged attribution (unlike the fg fields above).
    fn assert_achromatic(color: Color) {
        let (r, g, b) = rgb(color);
        assert_eq!(r, g, "not achromatic: {color:?}");
        assert_eq!(g, b, "not achromatic: {color:?}");
    }

    #[test]
    fn mono_washes_are_achromatic_and_preserve_the_curated_invariants() {
        for light in [false, true] {
            let t = Palette::mono(light);
            for wash in [
                t.del_subtle,
                t.del_strong,
                t.add_subtle,
                t.add_strong,
                t.del_staged_subtle,
                t.del_staged_strong,
                t.add_staged_subtle,
                t.add_staged_strong,
                t.cursor_bg,
                t.selection_bg,
                t.cursor_unfocused_bg,
            ] {
                assert_achromatic(wash);
            }

            // Add and Del share one gray ladder (accepted degradation — hue can't carry
            // add-vs-del in colorless mode, so gutter structure does instead).
            assert_eq!(t.del_subtle, t.add_subtle);
            assert_eq!(t.del_strong, t.add_strong);
            assert_eq!(t.del_staged_subtle, t.add_staged_subtle);
            assert_eq!(t.del_staged_strong, t.add_staged_strong);

            // Subtle vs strong remain visibly distinct steps.
            assert_ne!(t.del_subtle, t.del_strong);
            assert_ne!(t.del_staged_subtle, t.del_staged_strong);

            // Staged reads dimmer (closer to the implied background — brighter grays near a
            // light bg, darker grays near a dark bg) than unstaged.
            let (staged_subtle, _, _) = rgb(t.del_staged_subtle);
            let (subtle, _, _) = rgb(t.del_subtle);
            let (staged_strong, _, _) = rgb(t.del_staged_strong);
            let (strong, _, _) = rgb(t.del_strong);
            if light {
                assert!(staged_subtle > subtle, "staged should sit closer to white");
                assert!(staged_strong > strong, "staged should sit closer to white");
            } else {
                assert!(staged_subtle < subtle, "staged should sit closer to black");
                assert!(staged_strong < strong, "staged should sit closer to black");
            }

            // Cursor vs selection are distinct, and the unfocused cursor wash reads dimmer
            // (closer to the implied background) than the focused cursor wash.
            assert_ne!(t.cursor_bg, t.selection_bg);
            let (cursor, _, _) = rgb(t.cursor_bg);
            let (cursor_unfocused, _, _) = rgb(t.cursor_unfocused_bg);
            if light {
                assert!(cursor_unfocused > cursor);
            } else {
                assert!(cursor_unfocused < cursor);
            }
        }
    }

    #[test]
    fn mono_pane_header_focused_fg_collapses_with_dim_leaving_bold_the_only_differentiator() {
        // CS1 (`focused-pane-header`, locked decision #3): under `NO_COLOR`, `pane_header_focused_fg`
        // and `dim` both collapse to `Color::Reset` — color alone can no longer tell a focused
        // header label from an unfocused one, so `crate::render`'s structural BOLD is load-bearing
        // here (asserted against real render output in `render.rs`'s own NO_COLOR test).
        for light in [false, true] {
            let t = Palette::mono(light);
            assert_eq!(t.pane_header_focused_fg, Color::Reset);
            assert_eq!(t.pane_header_focused_fg, t.dim);
        }
    }

    #[test]
    fn only_mono_sets_colorless() {
        // `colorless` is the flag `render.rs`'s icon paint sites consult to collapse
        // palette-external colors (nerd-font icons) to `foreground` under NO_COLOR — it must be
        // true ONLY for `mono`, false for every curated/probed constructor.
        assert!(!Palette::dark().colorless);
        assert!(!Palette::light().colorless);
        assert!(!Palette::from_terminal(probed_base16(Color::Rgb(0x1a, 0x1a, 0x1a))).colorless);
        assert!(Palette::mono(false).colorless);
        assert!(Palette::mono(true).colorless);
    }

    #[test]
    fn mono_dark_ladder_sits_near_black_and_light_ladder_near_white() {
        let (r, _, _) = rgb(Palette::mono(false).del_strong);
        assert!(r < 128, "dark ladder should be a dark gray");
        let (r, _, _) = rgb(Palette::mono(true).del_strong);
        assert!(r > 128, "light ladder should be a pale gray");
    }
}
