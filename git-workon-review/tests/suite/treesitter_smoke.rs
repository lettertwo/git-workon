//! Proves the C-compilation path for tree-sitter grammars works end-to-end
//! in CI on all platforms: parse a snippet and run one highlight pass.

use tree_sitter::Parser;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

#[test]
fn parses_rust_snippet_without_errors() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("rust grammar loads");

    let tree = parser.parse("fn main() {}", None).expect("parses");
    assert!(!tree.root_node().has_error());
}

#[test]
fn highlight_pass_emits_at_least_one_highlight_start() {
    let mut config = HighlightConfiguration::new(
        tree_sitter_rust::LANGUAGE.into(),
        "rust",
        tree_sitter_rust::HIGHLIGHTS_QUERY,
        "",
        "",
    )
    .expect("highlight configuration builds");
    config.configure(&["keyword", "function"]);

    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(&config, b"fn main() {}", None, |_| None)
        .expect("highlighting succeeds");

    let saw_highlight_start = events
        .filter_map(|event| event.ok())
        .any(|event| matches!(event, HighlightEvent::HighlightStart(_)));

    assert!(
        saw_highlight_start,
        "expected at least one HighlightStart event"
    );
}
