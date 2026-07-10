//! CS5's opt-in nerd-font file-type icon table — a pure module, no [`crate::app::App`]/
//! [`crate::outline`] dependency, mirroring [`crate::summary`]'s pure-module posture.
//!
//! A terminal cannot report which font (patched with the nerd-font private-use glyphs or not)
//! the user has configured, so there is NO auto-detection here or anywhere else in the crate —
//! icons are strictly opt-in via `workon.review.outline.icons = nerd` (see `config.rs`'s schema
//! doc block and `App::apply_view_config`). With the config left at its default (`none`),
//! nothing in this module is ever called from `render.rs`.

/// Which of the outline's icon strategies is active — `workon.review.outline.icons`
/// (`nerd`/`none`), read once at startup by `App::apply_view_config` (CS5 mirrors CS3's
/// `OutlineOrder` plumbing exactly: `RawViewConfig` field -> `ReviewConfig` getter ->
/// `parse_outline_icons` -> warn-and-fallback in `apply_view_config` -> `OutlineState` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutlineIcons {
    /// No icon glyph — today's plain `[glyph][letter] path` row (CS5's unconditional part only).
    #[default]
    None,
    /// A nerd-font private-use glyph per file extension (falling back to
    /// [`DEFAULT_ICON`]/[`DIR_ICON`]), inserted before the path/name.
    Nerd,
}

/// The directory-row icon (nerd-font `nf-fa-folder`, U+F07B) — used for every
/// [`crate::outline::OutlineItem::Dir`] row when [`OutlineIcons::Nerd`] is active.
pub const DIR_ICON: char = '\u{f07b}'; // nf-fa-folder

/// The fallback file icon (nerd-font `nf-fa-file`, U+F15B) for any extension not in
/// [`icon_for_path`]'s table (including extensionless files).
pub const DEFAULT_ICON: char = '\u{f15b}'; // nf-fa-file

/// Look up the nerd-font glyph for `path`'s extension — small, deliberately-curated table
/// covering the languages this crate's own `highlight.rs` already bundles grammars for
/// (`lang_key_for_ext`), plus a couple of common project files. Every codepoint below is in the
/// nerd-font private-use area (`seti`/`devicons`/`fa` icon sets); unrecognized extensions and
/// extensionless files fall back to [`DEFAULT_ICON`].
pub fn icon_for_path(path: &str) -> char {
    // `Cargo.lock`/other `*.lock` files: match on the file NAME first, since "lock" isn't a
    // meaningful extension-based language distinction the way the rest of the table is.
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".lock") {
        return '\u{f023}'; // nf-fa-lock
    }
    let ext = match name.rsplit_once('.') {
        Some((_, ext)) => ext,
        None => return DEFAULT_ICON,
    };
    match ext {
        "rs" => '\u{e7a8}',                 // seti-rust
        "lua" => '\u{e620}',                // seti-lua
        "js" | "mjs" | "cjs" => '\u{e74e}', // seti-javascript
        "jsx" | "tsx" => '\u{e7ba}',        // seti-react
        "ts" | "mts" | "cts" => '\u{e628}', // seti-typescript
        "json" => '\u{e60b}',               // seti-json
        "toml" => '\u{e6b2}',               // seti-config (toml has no dedicated seti glyph)
        "md" | "markdown" => '\u{e73e}',    // seti-markdown
        _ => DEFAULT_ICON,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_map_to_their_glyphs() {
        assert_eq!(icon_for_path("src/main.rs"), '\u{e7a8}');
        assert_eq!(icon_for_path("scripts/init.lua"), '\u{e620}');
        assert_eq!(icon_for_path("index.js"), '\u{e74e}');
        assert_eq!(icon_for_path("app.mjs"), '\u{e74e}');
        assert_eq!(icon_for_path("component.tsx"), '\u{e7ba}');
        assert_eq!(icon_for_path("component.jsx"), '\u{e7ba}');
        assert_eq!(icon_for_path("types.ts"), '\u{e628}');
        assert_eq!(icon_for_path("package.json"), '\u{e60b}');
        assert_eq!(icon_for_path("Cargo.toml"), '\u{e6b2}');
        assert_eq!(icon_for_path("README.md"), '\u{e73e}');
    }

    #[test]
    fn lock_files_match_on_name_not_extension() {
        assert_eq!(icon_for_path("Cargo.lock"), '\u{f023}');
        assert_eq!(icon_for_path("nested/dir/yarn.lock"), '\u{f023}');
    }

    #[test]
    fn unknown_and_extensionless_paths_fall_back_to_the_default_icon() {
        assert_eq!(icon_for_path("Makefile"), DEFAULT_ICON);
        assert_eq!(icon_for_path("script.sh"), DEFAULT_ICON);
        assert_eq!(icon_for_path("noextension"), DEFAULT_ICON);
    }
}
