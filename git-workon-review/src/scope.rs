//! Enclosing tree-sitter "scope" lookup used by the reveal-to-scope gap expansion.
//!
//! Pure module: given a language key (the same key
//! [`crate::highlight::lang_key_for_ext`] resolves a file extension to) and a file's full text,
//! [`enclosing_scope_lines`] finds the smallest allowlisted structural node (function/impl/
//! class/...) containing a given line, so [`crate::app::App::expand_gap_at_cursor`] can reveal
//! exactly that much of a collapsed gap instead of a flat +10 rows.
//!
//! ## Allowlist philosophy
//!
//! Only "scope" node KINDS are allowlisted per language — deliberately narrow (a function body,
//! an impl/class block, ...) so a press reads as "show me the surrounding definition," not "show
//! me every nested block/expression the cursor happens to sit inside." Languages without a
//! reasonable definition of "scope" for this purpose (json/toml — data, not code with nested
//! definitions) get an empty allowlist, which makes [`enclosing_scope_lines`] always return
//! `None`: the caller falls back to the flat reveal uniformly, no special-casing needed at the
//! call site.
//!
//! ## Line/coordinate conventions
//!
//! The public API is 1-based, matching [`crate::align::Row::Line`] and the rest of the
//! diff-alignment code. tree-sitter's own [`Point::row`] is 0-based; this module converts at its
//! boundary (in, and back out) and nowhere else. The returned range is inclusive on both ends.
//!
//! ## No caching
//!
//! Parsing happens on demand, once per `Enter` press on a gap — bounded by [`MAX_SCOPE_LINES`]
//! (mirrors [`crate::highlight::MAX_HIGHLIGHT_LINES`]'s cap philosophy). That's cheap enough not
//! to be worth a parse-tree cache keyed on file identity + edit generation.

use tree_sitter::{Parser, Point};

use crate::highlight::language_for_key;

/// Files with more lines than this skip scope lookup entirely (the caller falls back to the flat
/// +N reveal) — same cap philosophy as [`crate::highlight::MAX_HIGHLIGHT_LINES`].
pub const MAX_SCOPE_LINES: usize = 20_000;

/// Per-language allowlist of "scope" node kinds, matched by exact string against
/// [`tree_sitter::Node::kind`]. Node kind names are grammar-specific facts verified against the
/// bundled grammars by this module's tests — do not extend without a test parsing a real snippet.
fn scope_kinds(lang_key: &str) -> &'static [&'static str] {
    match lang_key {
        "rust" => &[
            "function_item",
            "impl_item",
            "trait_item",
            "mod_item",
            "struct_item",
            "enum_item",
        ],
        "javascript" => &[
            "function_declaration",
            "function_expression",
            "method_definition",
            "class_declaration",
            "arrow_function",
        ],
        "typescript" | "tsx" => &[
            "function_declaration",
            "function_expression",
            "method_definition",
            "class_declaration",
            "arrow_function",
            "interface_declaration",
            "enum_declaration",
            "module_declaration",
        ],
        "lua" => &["function_declaration", "function_definition"],
        // json/toml/markdown: no structural "scope" concept worth revealing to — always fall
        // back to the flat reveal.
        _ => &[],
    }
}

/// The smallest allowlisted ancestor node (see [`scope_kinds`]) enclosing 1-based `line`, as an
/// inclusive 1-based `(start_line, end_line)` range — or `None` when: `lang_key` has no (or an
/// empty) allowlist, `text` exceeds [`MAX_SCOPE_LINES`], the grammar fails to build, or no
/// allowlisted ancestor contains `line` (e.g. a top-level `use` statement outside any item).
pub fn enclosing_scope_lines(lang_key: &str, text: &str, line: usize) -> Option<(usize, usize)> {
    let kinds = scope_kinds(lang_key);
    if kinds.is_empty() {
        return None;
    }
    if text.lines().count() > MAX_SCOPE_LINES {
        return None;
    }

    let language = language_for_key(lang_key)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(text, None)?;

    let point = Point {
        row: line.saturating_sub(1),
        column: 0,
    };
    let mut node = tree
        .root_node()
        .named_descendant_for_point_range(point, point)?;
    loop {
        if kinds.contains(&node.kind()) {
            return Some((node.start_position().row + 1, node.end_position().row + 1));
        }
        node = node.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_line_inside_a_function_body_returns_the_function_range() {
        let src = "fn outer() {\n    let x = 1;\n    let y = 2;\n}\n";
        // Line 2 (`let x = 1;`) is inside `fn outer`, which spans lines 1-4.
        let range = enclosing_scope_lines("rust", src, 2);
        assert_eq!(range, Some((1, 4)));
    }

    #[test]
    fn rust_nested_function_returns_the_inner_function_not_the_outer() {
        let src = "fn outer() {\n    fn inner() {\n        let z = 1;\n    }\n}\n";
        // Line 3 is inside `inner`, which spans lines 2-4 — the smallest (deepest) allowlisted
        // ancestor, not `outer` (lines 1-5).
        let range = enclosing_scope_lines("rust", src, 3);
        assert_eq!(range, Some((2, 4)));
    }

    #[test]
    fn rust_top_level_use_line_returns_none() {
        let src = "use std::fmt;\n\nfn main() {}\n";
        // Line 1 is a top-level `use` — no allowlisted ancestor contains it.
        assert_eq!(enclosing_scope_lines("rust", src, 1), None);
    }

    #[test]
    fn rust_impl_block_with_two_functions_asking_between_them_returns_the_impl() {
        let src = "struct S;\n\nimpl S {\n    fn a(&self) {\n        let _ = 1;\n    }\n\n    fn b(&self) {\n        let _ = 2;\n    }\n}\n";
        // Line 5 is inside `fn a`'s body — smallest allowlisted ancestor is the fn.
        assert_eq!(enclosing_scope_lines("rust", src, 5), Some((4, 6)));
        // Line 7 is the blank line between the two fns, still inside the impl block but outside
        // both fn bodies — smallest allowlisted ancestor is the impl.
        assert_eq!(enclosing_scope_lines("rust", src, 7), Some((3, 11)));
    }

    #[test]
    fn typescript_line_in_a_method_returns_the_method_range() {
        let src = "class C {\n    method() {\n        const x = 1;\n    }\n}\n";
        let range = enclosing_scope_lines("typescript", src, 3);
        assert_eq!(range, Some((2, 4)));
    }

    #[test]
    fn typescript_line_in_an_interface_returns_the_interface_range() {
        let src = "interface Foo {\n    bar: string;\n    baz: number;\n}\n";
        let range = enclosing_scope_lines("typescript", src, 2);
        assert_eq!(range, Some((1, 4)));
    }

    #[test]
    fn lua_line_in_a_function_returns_its_range() {
        let src = "function greet()\n    local msg = \"hi\"\n    print(msg)\nend\n";
        let range = enclosing_scope_lines("lua", src, 2);
        assert_eq!(range, Some((1, 4)));
    }

    #[test]
    fn json_always_returns_none() {
        let src = "{\n    \"a\": 1,\n    \"b\": {\n        \"c\": 2\n    }\n}\n";
        assert_eq!(enclosing_scope_lines("json", src, 4), None);
    }

    #[test]
    fn oversized_text_returns_none() {
        // Synthesize a cheap file with more lines than MAX_SCOPE_LINES; content doesn't matter,
        // only line count.
        let src = "fn f() {}\n".repeat(MAX_SCOPE_LINES + 1);
        assert_eq!(enclosing_scope_lines("rust", &src, 1), None);
    }
}
