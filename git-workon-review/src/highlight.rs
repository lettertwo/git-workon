//! Syntax highlighting via tree-sitter.
//!
//! One `HighlightConfiguration` is built lazily per language and cached.
//! Highlight events give byte offsets over the whole source; we split them
//! into per-line spans here so the renderer can compose them against
//! word-diff spans without re-deriving line boundaries.

use std::collections::HashMap;

use ratatui::style::Color;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Files with more lines than this are skipped (plain fg) to keep
/// highlighting fast.
pub const MAX_HIGHLIGHT_LINES: usize = 20_000;

/// Foreground color spans for a single line: byte range + color.
#[derive(Debug, Clone)]
pub struct FgSpan {
    pub start: usize,
    pub end: usize,
    pub color: Color,
}

/// The standard highlight-capture names we recognize. `configure()` matches
/// dotted capture names by longest prefix, so e.g. `keyword.control` maps to
/// `keyword`. Parallel with `HIGHLIGHT_COLORS`.
const HIGHLIGHT_NAMES: &[&str] = &[
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

// A small dark theme in the same family as syntect's base16-eighties.dark so
// the two engines look comparable side by side.
const C_RED: Color = Color::Rgb(0xf2, 0x77, 0x7a);
const C_ORANGE: Color = Color::Rgb(0xf9, 0x91, 0x57);
const C_YELLOW: Color = Color::Rgb(0xff, 0xcc, 0x66);
const C_GREEN: Color = Color::Rgb(0x99, 0xcc, 0x99);
const C_CYAN: Color = Color::Rgb(0x66, 0xcc, 0xcc);
const C_BLUE: Color = Color::Rgb(0x66, 0x99, 0xcc);
const C_PURPLE: Color = Color::Rgb(0xcc, 0x99, 0xcc);
const C_FG: Color = Color::Rgb(0xd3, 0xd0, 0xc8);
const C_COMMENT: Color = Color::Rgb(0x74, 0x73, 0x69);

const HIGHLIGHT_COLORS: &[Color] = &[
    C_ORANGE,  // attribute
    C_COMMENT, // comment
    C_ORANGE,  // constant
    C_ORANGE,  // constant.builtin
    C_YELLOW,  // constructor
    C_FG,      // embedded
    C_CYAN,    // escape
    C_BLUE,    // function
    C_BLUE,    // function.builtin
    C_BLUE,    // function.macro
    C_BLUE,    // function.method
    C_PURPLE,  // keyword
    C_RED,     // label
    C_ORANGE,  // number
    C_FG,      // operator
    C_CYAN,    // property
    C_FG,      // punctuation
    C_FG,      // punctuation.bracket
    C_FG,      // punctuation.delimiter
    C_CYAN,    // punctuation.special
    C_GREEN,   // string
    C_CYAN,    // string.special
    C_RED,     // tag
    C_YELLOW,  // type
    C_YELLOW,  // type.builtin
    C_FG,      // variable
    C_RED,     // variable.builtin
    C_FG,      // variable.parameter
];

/// Color for a highlight-capture name, for tests and debugging.
#[cfg(test)]
pub fn color_of(name: &str) -> Option<Color> {
    HIGHLIGHT_NAMES
        .iter()
        .position(|n| *n == name)
        .map(|i| HIGHLIGHT_COLORS[i])
}

fn lang_key_for_ext(ext: &str) -> Option<&'static str> {
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
                    let Some(&idx) = stack.last() else { continue };
                    let color = HIGHLIGHT_COLORS[idx];
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
                                color,
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
    fn names_and_colors_are_parallel() {
        assert_eq!(HIGHLIGHT_NAMES.len(), HIGHLIGHT_COLORS.len());
    }

    #[test]
    fn rust_snippet_yields_expected_span_kinds_on_right_lines() {
        let src = "fn main() {\n    let s = \"hi\";\n}\n";
        let mut ts = TsHighlighter::new();
        let hl = ts
            .highlight_file("test.rs", src)
            .expect("rust grammar available");
        assert_eq!(hl.len(), 3);

        // Line 0: `fn` at bytes 0..2 should be keyword-colored.
        let kw = color_of("keyword").unwrap();
        assert!(
            hl[0]
                .iter()
                .any(|s| s.start == 0 && s.end >= 2 && s.color == kw),
            "expected keyword span over `fn` on line 0, got {:?}",
            hl[0]
        );

        // Line 0: `main` should be function-colored.
        let func = color_of("function").unwrap();
        assert!(
            hl[0]
                .iter()
                .any(|s| { s.color == func && &src[..11][s.start..s.end.min(11)] == "main" }),
            "expected function span over `main` on line 0, got {:?}",
            hl[0]
        );

        // Line 1: string literal should be string-colored.
        let string = color_of("string").unwrap();
        assert!(
            hl[1].iter().any(|s| s.color == string),
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
        let comment = color_of("comment").unwrap();
        for (i, spans) in hl.iter().enumerate() {
            assert!(
                spans.iter().any(|s| s.color == comment),
                "expected comment span on line {i}"
            );
        }
    }
}
