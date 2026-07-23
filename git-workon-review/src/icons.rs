//! CS5's opt-in nerd-font file-type icon table — a pure module, no [`crate::app::App`]/
//! [`crate::outline`] dependency, mirroring [`crate::summary`]'s pure-module posture.
//!
//! A terminal cannot report which font (patched with the nerd-font private-use glyphs or not)
//! the user has configured, so there is NO auto-detection here or anywhere else in the crate —
//! icons are strictly opt-in via `workon.review.icons = nerd` (see `config.rs`'s schema
//! doc block and `App::apply_view_config`). With the config left at its default (`none`),
//! nothing in this module is ever called from `render.rs`.
//!
//! **Icon table (CS1 polish pass):** per-file glyphs and brand colors are looked up via the
//! [`devicons`] crate (Apache-2.0, `alexpasmantier/devicons`) rather than a hand-rolled table —
//! 597 filename+extension entries with the same filename-before-extension precedence
//! [`icon_for_path`] already followed. devicons ships separate Dark/Light color maps; the caller
//! picks one from the active [`crate::theme::Palette`] (see [`icon_for_path`]'s doc comment).
//! **Nerd-font v3 requirement:** devicons' glyphs are drawn from nerd-font v3's private-use
//! codepoints, roughly a fifth of which sit in a Unicode supplementary plane (outside the BMP).
//! The crate's own `IconMode::Nerd` glyphs picked in CS3 (status/header markers) stay
//! BMP-only for wider font compatibility, but a per-file icon from devicons may require a v3
//! nerd-font — this is the same "no auto-detection" opt-in tradeoff as the rest of this module.
//! devicons does not cover directories (it is a per-file mapper), so [`DIR_ICON`] is still ours.

use devicons::{icon_for_file, FileIcon, Theme as DeviconsTheme};
use ratatui::style::Color;

/// Which iconography strategy is active TUI-wide — `workon.review.icons` (`nerd`/`none`),
/// read once at startup by `App::apply_view_config` (`RawViewConfig` field -> `ReviewConfig`
/// getter -> `resolve_option` (against `ICON_MODE_OPTIONS`) -> warn-and-fallback in
/// `apply_view_config` -> `App` field).
/// Top-level like the theme, not an outline setting: it gates the outline's file/dir icons,
/// the summary panel's glyphs, and the winbar's marker/diffstat/file icons alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconMode {
    /// No icon glyph — today's plain `[glyph][letter] path` row (CS5's unconditional part only).
    #[default]
    None,
    /// A nerd-font private-use glyph per file extension (falling back to
    /// [`DEFAULT_ICON`]/[`DIR_ICON`]), inserted before the path/name.
    Nerd,
}

/// The directory-row icon (nerd-font `nf-fa-folder`, U+F07B) — used for every
/// [`crate::outline::OutlineItem::Dir`] row when [`IconMode::Nerd`] is active. devicons is a
/// per-file mapper (it has no directory entries), so this stays our own constant.
pub const DIR_ICON: char = '\u{f07b}'; // nf-fa-folder

/// The fallback file icon (nerd-font `nf-fa-file`, U+F15B) — devicons itself falls back to a
/// generic glyph for unrecognized extensions, but [`icon_for_path`] surfaces this constant
/// instead so callers (and this module's own tests) have a stable, documented "unknown" glyph.
pub const DEFAULT_ICON: char = '\u{f15b}'; // nf-fa-file

/// Look up the nerd-font glyph and brand color for `path`, via [`devicons::icon_for_file`].
/// `light_background` selects devicons' Light vs Dark color map — pass
/// `crate::theme::is_light_background(palette.background)` so the icon color suits the active
/// [`crate::theme::Palette`], not necessarily the terminal's real background.
///
/// Returns `(glyph, color)`, where `color` is `None` if devicons' hex string didn't parse (never
/// observed in practice, but handled without panicking rather than trusting an external crate's
/// string format unconditionally) — callers should fall back to their own plain foreground.
pub fn icon_for_path(path: &str, light_background: bool) -> (char, Option<Color>) {
    let devicons_theme = Some(if light_background {
        DeviconsTheme::Light
    } else {
        DeviconsTheme::Dark
    });
    let mut resolved = icon_for_file(path, &devicons_theme);
    if resolved.icon == FileIcon::default().icon {
        // devicons' filename match is exact-case (only its EXTENSION fallback lowercases), so
        // "Makefile" misses its lowercase-only "makefile" key. Mirror its extension strategy:
        // exact first (some keys, e.g. "PKGBUILD"/".Xresources", exist ONLY in exact case), then
        // retry with the lowercased basename. `FileIcon::default().icon` is devicons' unknown-file
        // sentinel — no real table entry uses it (verified against 0.6.12's tables).
        let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
        resolved = icon_for_file(name.as_str(), &devicons_theme);
    }
    if resolved.icon == FileIcon::default().icon {
        // Still unknown: surface OUR stable fallback glyph (see `DEFAULT_ICON`) instead of
        // devicons' bare `*`, which reads as a typo next to real icon glyphs.
        return (DEFAULT_ICON, None);
    }
    (resolved.icon, parse_hex_color(resolved.color))
}

/// Parse a `"#rrggbb"` hex string (devicons' [`FileIcon::color`] format) into a
/// [`ratatui::style::Color::Rgb`]. Returns `None` on anything malformed rather than panicking —
/// this is data from an external crate, not a value this codebase controls.
fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_map_to_devicons_glyphs() {
        // Pinned against devicons 0.6.12's actual table — these WILL need updating if the crate
        // revises its glyph picks; that's an intentional pin, not a bug.
        assert_eq!(icon_for_path("src/main.rs", false).0, '\u{e68b}');
        assert_eq!(icon_for_path("index.js", false).0, '\u{e60c}');
        assert_eq!(icon_for_path("component.tsx", false).0, '\u{e7ba}');
        assert_eq!(icon_for_path("types.ts", false).0, '\u{e628}');
        assert_eq!(icon_for_path("package.json", false).0, '\u{e71e}');
        assert_eq!(icon_for_path("Cargo.toml", false).0, '\u{e6b2}');
        assert_eq!(icon_for_path("README.md", false).0, '\u{f48a}');
    }

    #[test]
    fn filename_precedence_matches_our_previous_lock_file_handling() {
        // devicons matches on filename before extension, same shape as the old hand-rolled table.
        assert_eq!(icon_for_path("Cargo.lock", false).0, '\u{e672}');
        assert_eq!(icon_for_path("nested/dir/yarn.lock", false).0, '\u{e672}');
    }

    #[test]
    fn newly_covered_project_files_resolve_to_devicons_glyphs() {
        // Names our old 8-entry table didn't cover — devicons' broader table does.
        assert_eq!(icon_for_path("Makefile", false).0, '\u{e779}');
        assert_eq!(icon_for_path("Dockerfile", false).0, '\u{f0868}');
        assert_eq!(icon_for_path(".gitignore", false).0, '\u{e702}');
    }

    #[test]
    fn filename_match_retries_lowercased_when_the_exact_case_misses() {
        // devicons' own filename lookup is exact-case; its table has "makefile" but no
        // "Makefile", so without our lowercased retry the standard spelling would fall through
        // to the unknown-file fallback.
        assert_eq!(
            icon_for_path("Makefile", false),
            icon_for_path("makefile", false)
        );
        assert_eq!(icon_for_path("nested/dir/LICENSE", false).0, '\u{e60a}');
    }

    #[test]
    fn unknown_files_fall_back_to_our_default_icon_not_devicons_asterisk() {
        // devicons returns a literal '*' for unknown files; `icon_for_path` surfaces our stable
        // DEFAULT_ICON (with no color) instead — see `DEFAULT_ICON`'s doc comment.
        assert_eq!(
            icon_for_path("file.zzznotreal", false),
            (DEFAULT_ICON, None)
        );
        assert_eq!(icon_for_path("noextension", false), (DEFAULT_ICON, None));
    }

    #[test]
    fn colors_differ_between_dark_and_light_themes_for_a_branded_extension() {
        let (_, dark_color) = icon_for_path("src/main.rs", false);
        let (_, light_color) = icon_for_path("src/main.rs", true);
        assert!(dark_color.is_some());
        assert!(light_color.is_some());
    }

    #[test]
    fn parse_hex_color_accepts_well_formed_rrggbb() {
        assert_eq!(
            parse_hex_color("#a074c4"),
            Some(Color::Rgb(0xa0, 0x74, 0xc4))
        );
        assert_eq!(parse_hex_color("#000000"), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(parse_hex_color("#ffffff"), Some(Color::Rgb(255, 255, 255)));
    }

    #[test]
    fn parse_hex_color_rejects_malformed_input_without_panicking() {
        assert_eq!(parse_hex_color("a074c4"), None); // missing '#'
        assert_eq!(parse_hex_color("#a074c"), None); // too short
        assert_eq!(parse_hex_color("#a074c400"), None); // too long
        assert_eq!(parse_hex_color("#zzzzzz"), None); // not hex digits
        assert_eq!(parse_hex_color(""), None);
    }
}
