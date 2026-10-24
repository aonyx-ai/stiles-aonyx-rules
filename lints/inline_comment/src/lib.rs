use tree_sitter::Node;
use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity, Span};

/// The characters that authors use to draw a horizontal rule
const DIVIDER_CHARS: &[char] = &['-', '=', '*', '~', '#', '_', '+', '/'];

/// The marker that clippy's `undocumented_unsafe_blocks` looks for
const SAFETY_MARKER: &str = "SAFETY:";

/// Flags comments that are not documentation
///
/// Doc comments (`///`, `//!`, `/** */`, and `/*! */`) reach the published
/// API docs, so the rule leaves them alone. A plain comment reaches only
/// whoever reads the source, and nothing keeps it true as the code beside
/// it changes. The rule makes two exceptions. A `SAFETY:` comment stays
/// because clippy demands it. So does the comment that opens a file, which
/// is usually a license header. A run of comment lines produces one
/// diagnostic that spans the whole run.
pub struct InlineComment;

/// The shape of a flagged comment, which decides the advice the rule gives
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum CommentKind {
    /// A horizontal rule, with or without a heading inside it
    Divider,
    /// Anything else the author wrote
    Prose,
}

/// Returns whether the comment carries a documentation marker
///
/// The grammar records the marker in the `outer` or `inner` field, which
/// covers `///`, `//!`, `/** */`, and `/*! */`. A bare `///` counts as
/// documentation even though it says nothing.
fn is_doc_comment(node: Node<'_>) -> bool {
    node.child_by_field_name("outer").is_some() || node.child_by_field_name("inner").is_some()
}

/// Returns whether the node is a comment without a documentation marker
fn is_plain_comment(node: Node<'_>) -> bool {
    let is_comment = node.kind() == "line_comment" || node.kind() == "block_comment";
    is_comment && !is_doc_comment(node)
}

/// Returns whether the comment opens the file
///
/// A copyright or license header sits above everything else, and the tree
/// puts it first under `source_file`. The check is structural, so it exempts
/// whatever opens the file, header or not. The rule still reports every
/// later run.
fn is_file_header(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };

    parent.kind() == "source_file" && node.prev_sibling().is_none()
}

/// Returns what a comment holds, without its delimiters
fn comment_content(text: &str) -> &str {
    let text = text.trim();
    let Some(text) = text.strip_prefix("/*") else {
        return text.strip_prefix("//").unwrap_or(text);
    };
    text.strip_suffix("*/").unwrap_or(text)
}

/// Returns whether the comment justifies an `unsafe` block
///
/// Clippy's `undocumented_unsafe_blocks` demands this comment, so the rule
/// must leave it alone. The check copies clippy's: the marker follows the
/// comment punctuation and any spaces, and its case does not matter.
fn is_safety_comment(text: &str) -> bool {
    comment_content(text)
        .trim_start()
        .get(..SAFETY_MARKER.len())
        .is_some_and(|marker| marker.eq_ignore_ascii_case(SAFETY_MARKER))
}

/// Returns the shape of a comment, read from its own text
fn classify(text: &str) -> CommentKind {
    let content = comment_content(text).trim();
    if content.is_empty() {
        return CommentKind::Prose;
    }
    if content.chars().all(|c| DIVIDER_CHARS.contains(&c)) {
        return CommentKind::Divider;
    }

    let head = content
        .chars()
        .take_while(|c| DIVIDER_CHARS.contains(c))
        .count();
    let tail = content
        .chars()
        .rev()
        .take_while(|c| DIVIDER_CHARS.contains(c))
        .count();

    match head >= 3 && tail >= 3 {
        true => CommentKind::Divider,
        false => CommentKind::Prose,
    }
}

/// Returns the source text of a node, read from its parent's text
///
/// Sibling navigation yields tree-sitter nodes, which carry byte ranges but
/// no text of their own. A node outside the parent's range yields an empty
/// string; a child is never outside it.
fn text_of<'a>(node: Node<'_>, parent_text: &'a str, parent_start: usize) -> &'a str {
    let Some(start) = node.start_byte().checked_sub(parent_start) else {
        return "";
    };
    let Some(end) = node.end_byte().checked_sub(parent_start) else {
        return "";
    };

    parent_text.get(start..end).unwrap_or_default()
}

/// Returns whether this comment belongs to a run that starts above it
///
/// Comments in a run are siblings with one line break between them. The rule
/// reports a run at most once, at its first comment, so the ones below it
/// produce no diagnostic.
fn continues_run(node: Node<'_>) -> bool {
    let Some(previous) = node.prev_sibling() else {
        return false;
    };

    is_plain_comment(previous) && previous.end_position().row + 1 == node.start_position().row
}

/// Returns the end byte of the comment run that starts at this node
///
/// The run stops short of a safety comment, so no diagnostic spans one.
fn run_end(node: Node<'_>, parent_text: &str, parent_start: usize) -> usize {
    let mut last = node;
    while let Some(next) = last.next_sibling() {
        let joins = is_plain_comment(next)
            && last.end_position().row + 1 == next.start_position().row
            && !is_safety_comment(text_of(next, parent_text, parent_start));
        if !joins {
            break;
        }
        last = next;
    }

    last.end_byte()
}

/// Returns the diagnostic for a comment node, if the rule reports one
fn check_comment(node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let raw = node.raw();
    let text = node.text();
    if is_doc_comment(raw) || is_safety_comment(text) || is_file_header(raw) || continues_run(raw) {
        return Vec::new();
    }

    let Some(parent) = node.parent() else {
        return Vec::new();
    };

    let span = node.span();
    let file = span.file_path().clone();
    let end = run_end(raw, parent.text(), parent.raw().start_byte());
    let span = Span::new(file, span.start(), end);

    let message = match classify(text) {
        CommentKind::Divider => "remove this section divider",
        CommentKind::Prose => "remove this comment or move what it explains into a doc comment",
    };

    vec![Diagnostic::new(
        RuleId::new("lint.inline-comment"),
        Severity::Warn,
        message.into(),
        span,
    )]
}

impl RustLintPass for InlineComment {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_comment(node)
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_comment(node)
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for InlineComment {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.inline-comment")]
    }
}

whisker_plugin::export_lints![rust: [InlineComment]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass};

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(InlineComment))];
        execute(&tree, &mut passes)
    }

    #[test]
    fn blank_line_between_comments_splits_the_run() {
        let diagnostics = run("use std::fmt;\n// first\n\n// second\nfn f() {}");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn block_comment_is_flagged() {
        let diagnostics = run("fn f() { /* why */ }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn classify_with_empty_content_returns_prose() {
        let kind = classify("//");

        assert_eq!(kind, CommentKind::Prose);
    }

    #[test]
    fn classify_with_only_divider_characters_returns_divider() {
        let kind = classify("// ========================");

        assert_eq!(kind, CommentKind::Divider);
    }

    #[test]
    fn classify_with_prose_returns_prose() {
        let kind = classify("// check if the user is valid");

        assert_eq!(kind, CommentKind::Prose);
    }

    #[test]
    fn classify_with_short_wrapping_returns_prose() {
        let kind = classify("// -- x --");

        assert_eq!(kind, CommentKind::Prose);
    }

    #[test]
    fn classify_with_wrapped_heading_returns_divider() {
        let kind = classify("// --- Helper functions ---");

        assert_eq!(kind, CommentKind::Divider);
    }

    #[test]
    fn comment_above_a_safety_comment_is_flagged_alone() {
        let source = "fn f() {\n    // an ordinary note\n    // SAFETY: the caller checks the pointer\n    unsafe { g() }\n}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
        let start = source.find("// an").expect("source contains the note");
        assert_diagnostic(&diagnostics[0]).has_span(
            "<test>",
            start,
            start + "// an ordinary note".len(),
        );
    }

    #[test]
    fn comment_after_a_doc_comment_is_flagged() {
        let diagnostics = run("/// Documents the function\n// but this does not\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn comment_after_the_file_header_is_flagged() {
        let source = "// Copyright 2026 The Whisker Authors\n\n// an ordinary comment\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0]).message_contains("doc comment");
    }

    #[test]
    fn comment_before_a_doc_comment_is_reported_once() {
        let diagnostics = run("use std::fmt;\n// plain\n/// Documents the function\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn comment_below_module_docs_is_flagged() {
        let diagnostics = run("//! Documents the module\n// an ordinary comment\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn comment_content_with_block_comment_strips_delimiters() {
        let content = comment_content("/* why */");

        assert_eq!(content, " why ");
    }

    #[test]
    fn comment_content_with_empty_block_comment_returns_empty() {
        let content = comment_content("/**/");

        assert_eq!(content, "");
    }

    #[test]
    fn comment_content_with_line_comment_strips_slashes() {
        let content = comment_content("// why");

        assert_eq!(content, " why");
    }

    #[test]
    fn comment_run_reports_its_whole_span() {
        let source = "fn f() {\n    // first\n    // second\n}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
        let start = source
            .find("// first")
            .expect("source contains the first line");
        let end = source
            .find("// second")
            .expect("source contains the second line")
            + "// second".len();
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, end);
    }

    #[test]
    fn consecutive_comments_at_module_level_are_reported_once() {
        let diagnostics = run("use std::fmt;\n// first\n// second\n// third\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn consecutive_comments_in_a_function_body_are_reported_once() {
        let diagnostics = run("fn f() {\n    // first\n    // second\n    // third\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn consecutive_comments_in_a_match_body_are_reported_once() {
        let source = "fn f() {\n    match x {\n        // first\n        // second\n        A => {}\n    }\n}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn consecutive_comments_in_a_struct_body_are_reported_once() {
        let source = "struct S {\n    a: u32,\n    // first\n    // second\n    b: u32,\n}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn divider_is_reported_as_a_section_divider() {
        let diagnostics = run("use std::fmt;\n// ========================\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.inline-comment")
            .has_severity(Severity::Warn)
            .message_contains("section divider");
    }

    #[test]
    fn doc_comment_on_a_function_is_not_flagged() {
        let diagnostics = run("/// Returns nothing\n///\n/// More detail here.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_outer_doc_comment_is_not_flagged() {
        let diagnostics = run("///\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn file_header_block_comment_is_not_flagged() {
        let diagnostics = run("/* Copyright 2026 The Whisker Authors */\n\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn file_header_comment_is_not_flagged() {
        let source = "// Copyright 2026 The Whisker Authors\n// SPDX-License-Identifier: Apache-2.0\n\nuse std::fmt;\n\nfn f() {}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn four_slash_comment_is_flagged() {
        let diagnostics = run("use std::fmt;\n////not a doc comment\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inner_block_doc_comment_is_not_flagged() {
        let diagnostics = run("/*! Documents the module */\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_comment_is_not_flagged() {
        let diagnostics = run("//! Documents the module\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn is_safety_comment_with_lowercase_marker_returns_true() {
        let is_safety = is_safety_comment("// safety: clippy accepts any case");

        assert!(is_safety);
    }

    #[test]
    fn is_safety_comment_with_marker_after_prose_returns_false() {
        let is_safety = is_safety_comment("// the caller checks it. SAFETY: it is aligned");

        assert!(!is_safety);
    }

    #[test]
    fn is_safety_comment_with_no_colon_returns_false() {
        let is_safety = is_safety_comment("// SAFETY is the reason for this check");

        assert!(!is_safety);
    }

    #[test]
    fn is_safety_comment_with_uppercase_marker_returns_true() {
        let is_safety = is_safety_comment("// SAFETY: the caller upholds the invariant");

        assert!(is_safety);
    }

    #[test]
    fn module_level_comment_is_flagged() {
        let diagnostics = run("use std::fmt;\n// helpers live below\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.inline-comment")
            .has_severity(Severity::Warn)
            .message_contains("doc comment");
    }

    /// Pins the cost of a structural header check: the rule cannot tell a
    /// license header from an ordinary comment that opens a file
    #[test]
    fn ordinary_comment_that_opens_the_file_is_not_flagged() {
        let diagnostics = run("// check the user\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn outer_block_doc_comment_is_not_flagged() {
        let diagnostics = run("/** Documents the function */\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn plain_comment_in_a_function_body_is_flagged() {
        let diagnostics = run("fn f() {\n    // check the user\n    g();\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn safety_block_comment_is_not_flagged() {
        let source =
            "fn f() {\n    /* SAFETY: the caller upholds the invariant */\n    unsafe { g() }\n}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn safety_comment_is_not_flagged() {
        let source =
            "fn f() {\n    // SAFETY: the pointer comes from a reference\n    unsafe { g() }\n}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn safety_comment_with_a_continuation_line_is_not_flagged() {
        let source = "fn f() {\n    // SAFETY: the pointer comes from a reference,\n    // so it is aligned and non-null\n    unsafe { g() }\n}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn section_header_is_reported_as_a_section_divider() {
        let diagnostics = run("use std::fmt;\n// --- Helper functions ---\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0]).message_contains("section divider");
    }

    #[test]
    fn slashes_inside_a_doc_comment_are_not_flagged() {
        let diagnostics = run("/// Shows a body like `// ...` in an example\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn slashes_inside_a_string_literal_are_not_flagged() {
        let diagnostics = run("fn f() {\n    let s = \"// not a comment\";\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn source_without_comments_is_not_flagged() {
        let diagnostics = run("fn f() {\n    let x = 1;\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn statement_between_comments_splits_the_run() {
        let diagnostics = run("fn f() {\n    // first\n    g();\n    // second\n}");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn text_of_with_an_offset_parent_returns_the_node_source() {
        let tree = parse("fn f() {\n    let x = 1;\n}", Language::Rust);
        let function = tree
            .root_node()
            .named_child(0)
            .expect("the source has one function");
        let block = function.named_child(2).expect("the function has a body");
        let statement = block.named_child(0).expect("the body has a statement");

        let text = text_of(statement.raw(), block.text(), block.raw().start_byte());

        assert_eq!(text, "let x = 1;");
    }

    #[test]
    fn trailing_comment_is_flagged() {
        let diagnostics = run("fn f() {\n    let x = 1; // why\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<InlineComment>();
        assert_send::<CommentKind>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<InlineComment>();
        assert_sync::<CommentKind>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<InlineComment>();
        assert_unpin::<CommentKind>();
    }
}
