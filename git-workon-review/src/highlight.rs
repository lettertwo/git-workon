//! Syntax highlighting via tree-sitter.
//!
//! One `HighlightConfiguration` is built lazily per language and cached.
//! Highlight events give byte offsets over the whole source; we split them
//! into per-line spans here so the renderer can compose them against
//! word-diff spans without re-deriving line boundaries.

use std::collections::HashMap;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Files with more lines than this are skipped (plain fg) to keep
/// highlighting fast.
pub const MAX_HIGHLIGHT_LINES: usize = 20_000;

/// Foreground syntax span for a single line: a byte range and the semantic *capture index* —
/// the position in [`HIGHLIGHT_NAMES`] of the capture that covers it. The color is resolved at
/// render time against the active [`crate::theme::Palette`] (ADR-035), NOT baked in here: the
/// tree-sitter pass is theme-free and cacheable, and a theme switch recolors by re-rendering.
#[derive(Debug, Clone)]
pub struct FgSpan {
    pub start: usize,
    pub end: usize,
    /// Index into [`HIGHLIGHT_NAMES`]; resolve via [`crate::theme::Palette::syntax`].
    pub capture: usize,
}

/// The standard highlight-capture names we recognize. `configure()` matches
/// dotted capture names by longest prefix, so e.g. `keyword.control` maps to
/// `keyword`. This is the capture *index space* — theme-invariant (see ADR-035); the
/// per-capture colors live in [`crate::theme`]'s `SYNTAX_SLOTS` template.
pub(crate) const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// The capture index for a highlight-capture name — its position in [`HIGHLIGHT_NAMES`], which is
/// exactly what an [`FgSpan::capture`] holds. Used by [`crate::theme`] and tests to relate a named
/// capture to the index the highlighter records. `None` for an unrecognized name.
pub fn capture_index(name: &str) -> Option<usize> {
    HIGHLIGHT_NAMES.iter().position(|n| *n == name)
}

/// Maps a file extension to the [`build_config`]/[`language_for_key`] key for its grammar, or
/// `None` when no bundled grammar covers it. `pub(crate)` so [`crate::app`] can resolve a gap's
/// anchor file to a scope-lookup language (tree-sitter scope reveal) without duplicating this
/// table.
pub(crate) fn lang_key_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "lua" => Some("lua"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "js" | "mjs" | "cjs" | "jsx" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "md" | "markdown" => Some("markdown"),
        _ => None,
    }
}

/// The raw `tree_sitter::Language` for a [`lang_key_for_ext`] key, with no highlight query
/// configuration attached — [`build_config`] below wraps the same grammar constructors together
/// with a language's highlight/injection/locals queries for `TsHighlighter`; [`crate::scope`]
/// needs only the grammar (it parses to walk node kinds, not to highlight), so it shares this
/// smaller constructor instead of duplicating the `LANGUAGE.into()` calls.
pub(crate) fn language_for_key(key: &str) -> Option<tree_sitter::Language> {
    let language = match key {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        "toml" => tree_sitter_toml_ng::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "markdown" => tree_sitter_md::LANGUAGE.into(),
        _ => return None,
    };
    Some(language)
}

fn build_config(key: &'static str) -> Option<HighlightConfiguration> {
    let result = match key {
        "rust" => HighlightConfiguration::new(
            tree_sitter_rust::LANGUAGE.into(),
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        ),
        "lua" => HighlightConfiguration::new(
            tree_sitter_lua::LANGUAGE.into(),
            "lua",
            tree_sitter_lua::HIGHLIGHTS_QUERY,
            tree_sitter_lua::INJECTIONS_QUERY,
            tree_sitter_lua::LOCALS_QUERY,
        ),
        "json" => HighlightConfiguration::new(
            tree_sitter_json::LANGUAGE.into(),
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "toml" => HighlightConfiguration::new(
            tree_sitter_toml_ng::LANGUAGE.into(),
            "toml",
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "javascript" => {
            // The JS grammar includes JSX nodes, so the JSX query is safe to
            // append for plain .js too.
            let highlights = format!(
                "{}{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            );
            HighlightConfiguration::new(
                tree_sitter_javascript::LANGUAGE.into(),
                "javascript",
                &highlights,
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
            )
        }
        "typescript" => {
            // tree-sitter-highlight gives precedence to the LAST matching pattern, so the
            // inherited javascript query goes first and the language-specific query is
            // appended (wins on conflicts).
            let highlights = format!(
                "{}{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            );
            HighlightConfiguration::new(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                &highlights,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
            )
        }
        "tsx" => {
            // Same last-wins precedence as the typescript arm above.
            let highlights = format!(
                "{}{}{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            );
            HighlightConfiguration::new(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                "tsx",
                &highlights,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
            )
        }
        "markdown" => HighlightConfiguration::new(
            tree_sitter_md::LANGUAGE.into(),
            "markdown",
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            "",
            "",
        ),
        _ => return None,
    };

    match result {
        Ok(mut config) => {
            config.configure(HIGHLIGHT_NAMES);
            Some(config)
        }
        Err(_) => None,
    }
}

pub struct TsHighlighter {
    core: Highlighter,
    /// Lazily built configs; `None` records a failed build so we don't retry.
    configs: HashMap<&'static str, Option<HighlightConfiguration>>,
}

impl TsHighlighter {
    pub fn new() -> Self {
        Self {
            core: Highlighter::new(),
            configs: HashMap::new(),
        }
    }

    /// Highlight the full text of a file, returning one Vec<FgSpan> per line.
    /// `None` means: no grammar for this extension, file too large, or a
    /// highlight error — caller should fall back to unhighlighted text.
    pub fn highlight_file(&mut self, path: &str, text: &str) -> Option<Vec<Vec<FgSpan>>> {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let key = lang_key_for_ext(ext)?;

        let line_count = text.lines().count();
        if line_count > MAX_HIGHLIGHT_LINES {
            return None;
        }

        let config = self
            .configs
            .entry(key)
            .or_insert_with(|| build_config(key))
            .as_ref()?;

        // Byte offset of each line start; used to split whole-source spans
        // into per-line spans.
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }

        let mut out: Vec<Vec<FgSpan>> = vec![Vec::new(); line_count];
        let mut stack: Vec<usize> = Vec::new();

        let events = self
            .core
            .highlight(config, text.as_bytes(), None, |_| None)
            .ok()?;

        for event in events {
            match event.ok()? {
                HighlightEvent::HighlightStart(h) => stack.push(h.0),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    let Some(&capture) = stack.last() else {
                        continue;
                    };
                    let mut pos = start;
                    while pos < end {
                        let line_idx = line_starts.partition_point(|&s| s <= pos) - 1;
                        if line_idx >= line_count {
                            break;
                        }
                        let line_start = line_starts[line_idx];
                        // End of line content, excluding the trailing '\n'.
                        let line_end = line_starts
                            .get(line_idx + 1)
                            .map(|s| s - 1)
                            .unwrap_or(text.len());
                        let seg_end = end.min(line_end);
                        if pos < seg_end {
                            out[line_idx].push(FgSpan {
                                start: pos - line_start,
                                end: seg_end - line_start,
                                capture,
                            });
                        }
                        pos = match line_starts.get(line_idx + 1) {
                            Some(&next) => next,
                            None => end,
                        };
                    }
                }
            }
        }

        Some(out)
    }
}

impl Default for TsHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_syntax_template_are_parallel() {
        // The capture index space (`HIGHLIGHT_NAMES`) and the theme's per-capture syntax template
        // must stay the same length — a capture with no slot (or vice versa) would panic at render.
        assert_eq!(HIGHLIGHT_NAMES.len(), crate::theme::syntax_slot_count());
    }

    #[test]
    fn rust_snippet_yields_expected_span_kinds_on_right_lines() {
        let src = "fn main() {\n    let s = \"hi\";\n}\n";
        let mut ts = TsHighlighter::new();
        let hl = ts
            .highlight_file("test.rs", src)
            .expect("rust grammar available");
        assert_eq!(hl.len(), 3);

        // Spans now carry the semantic capture INDEX (color is resolved at render time against the
        // theme — see `FgSpan`), so these assert on the capture, not a baked color.

        // Line 0: `fn` at bytes 0..2 should be a keyword capture.
        let kw = capture_index("keyword").unwrap();
        assert!(
            hl[0]
                .iter()
                .any(|s| s.start == 0 && s.end >= 2 && s.capture == kw),
            "expected keyword span over `fn` on line 0, got {:?}",
            hl[0]
        );

        // Line 0: `main` should be a function capture.
        let func = capture_index("function").unwrap();
        assert!(
            hl[0]
                .iter()
                .any(|s| { s.capture == func && &src[..11][s.start..s.end.min(11)] == "main" }),
            "expected function span over `main` on line 0, got {:?}",
            hl[0]
        );

        // Line 1: string literal should be a string capture.
        let string = capture_index("string").unwrap();
        assert!(
            hl[1].iter().any(|s| s.capture == string),
            "expected string span on line 1, got {:?}",
            hl[1]
        );
    }

    #[test]
    fn unknown_extension_returns_none() {
        let mut ts = TsHighlighter::new();
        assert!(ts.highlight_file("mystery.zzz", "hello world\n").is_none());
        assert!(ts.highlight_file("no_extension", "hello world\n").is_none());
    }

    #[test]
    fn spans_never_cross_line_boundaries() {
        let src = "/* a\nmultiline\ncomment */\n";
        let mut ts = TsHighlighter::new();
        let hl = ts.highlight_file("c.rs", src).unwrap();
        let line_lens: Vec<usize> = src.lines().map(|l| l.len()).collect();
        for (i, spans) in hl.iter().enumerate() {
            for s in spans {
                assert!(s.end <= line_lens[i], "span {s:?} exceeds line {i} length");
            }
        }
        // The multiline comment should produce comment spans on all 3 lines.
        let comment = capture_index("comment").unwrap();
        for (i, spans) in hl.iter().enumerate() {
            assert!(
                spans.iter().any(|s| s.capture == comment),
                "expected comment span on line {i}"
            );
        }
    }
}
