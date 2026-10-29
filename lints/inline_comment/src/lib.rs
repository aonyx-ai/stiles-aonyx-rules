use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity, Span};
use stiles_typescript::TypeScriptLintPass;
use tree_sitter::Node;

/// The characters that authors use to draw a horizontal rule
const DIVIDER_CHARS: &[char] = &['-', '=', '*', '~', '#', '_', '+', '/'];

/// The marker that clippy's `undocumented_unsafe_blocks` looks for
const SAFETY_MARKER: &str = "SAFETY:";

/// The openings of a TypeScript comment that a tool reads
///
/// Each one is an instruction rather than an explanation. The TypeScript
/// compiler reads `@ts-expect-error`, and a linter or formatter reads its
/// suppressions. A coverage tool reads its ignores, a bundler reads its
/// hints and purity annotations, and a JSX compiler reads its pragmas.
/// Removing one changes what a tool does, so the rule leaves them alone.
const TOOL_DIRECTIVES: &[&str] = &[
    "@ts-",
    "eslint",
    "prettier-ignore",
    "biome-ignore",
    "oxlint-",
    "istanbul ignore",
    "c8 ignore",
    "v8 ignore",
    "webpack",
    "@vite-ignore",
    "#__PURE__",
    "@__PURE__",
    "#__NO_SIDE_EFFECTS__",
    "@__NO_SIDE_EFFECTS__",
    "@jsx",
];

/// Flags comments that are not documentation
///
/// Doc comments (`///`, `//!`, `/** */`, and `/*! */`) reach the published
/// API docs, so the rule leaves them alone. A plain comment reaches only
/// whoever reads the source, and nothing keeps it true as the code beside
/// it changes. The rule makes two exceptions. A `SAFETY:` comment stays
/// because clippy demands it. So does the comment that opens a file, which
/// is usually a license header. A run of comment lines produces one
/// diagnostic that spans the whole run.
///
/// In TypeScript a `/** */` comment is the documentation, and `//` and
/// `/* */` comments are plain. The exception there is a comment a tool
/// reads, such as `// @ts-expect-error`, a linter's suppression, or a
/// `/// <reference />` directive, which [`TOOL_DIRECTIVES`] lists. A
/// directive is one line, so the prose comment under it is still reported.
///
/// # Examples
///
/// ```
/// use inline_comment::InlineComment;
/// use stiles_rust::RustLintPassAdapter;
/// use stiles_testing::{execute, parse};
/// use stiles_types::{Language, LintPass};
///
/// let tree = parse("fn f() {\n    // an ordinary note\n    g();\n}", Language::Rust);
/// let pass: Box<dyn LintPass> = Box::new(RustLintPassAdapter::new(InlineComment));
/// let mut passes = vec![pass];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct InlineComment;

/// The grammar a comment was parsed with, which decides how it is read
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Syntax {
    Rust,
    TypeScript,
}

impl Syntax {
    /// Returns the kind of the node that holds a whole file
    fn root(self) -> &'static str {
        match self {
            Syntax::Rust => "source_file",
            Syntax::TypeScript => "program",
        }
    }

    /// Returns whether the node is a comment of any kind
    fn is_comment(self, node: Node<'_>) -> bool {
        match self {
            Syntax::Rust => node.kind() == "line_comment" || node.kind() == "block_comment",
            Syntax::TypeScript => node.kind() == "comment",
        }
    }

    /// Returns whether the comment is documentation
    fn is_doc_comment(self, node: Node<'_>, text: &str) -> bool {
        match self {
            Syntax::Rust => is_doc_comment(node),
            Syntax::TypeScript => text.starts_with("/**") && text != "/**/",
        }
    }

    /// Returns whether the node is a comment that is not documentation
    fn is_plain_comment(self, node: Node<'_>, text: &str) -> bool {
        self.is_comment(node) && !self.is_doc_comment(node, text)
    }

    /// Returns whether the comment is one the rule leaves alone
    fn is_exempt(self, text: &str) -> bool {
        match self {
            Syntax::Rust => is_safety_comment(text),
            Syntax::TypeScript => is_tool_directive(text),
        }
    }

    /// Returns whether the lines under an exempt comment belong to it
    ///
    /// A `SAFETY:` comment often runs over several lines, and every one of
    /// them justifies the block. A tool directive is one line, and a comment
    /// under it explains something else.
    fn exemption_spans_lines(self) -> bool {
        match self {
            Syntax::Rust => true,
            Syntax::TypeScript => false,
        }
    }
}

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

/// Returns whether the comment opens the file
///
/// A copyright or license header sits above everything else, and the tree
/// puts it first under the file's root. The check is structural, so it
/// exempts whatever opens the file, header or not. The rule still reports
/// every later run. A TypeScript file may open with a `#!` line, which a
/// header follows.
fn is_file_header(node: Node<'_>, syntax: Syntax) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != syntax.root() {
        return false;
    }

    let Some(previous) = node.prev_sibling() else {
        return true;
    };
    previous.kind() == "hash_bang_line" && previous.prev_sibling().is_none()
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

/// Returns whether a TypeScript comment is an instruction to a tool
///
/// A triple-slash directive such as `/// <reference types="node" />` is one
/// too: TypeScript reads it, and it is not documentation.
fn is_tool_directive(text: &str) -> bool {
    if let Some(rest) = text.trim_start().strip_prefix("///")
        && rest.trim_start().starts_with('<')
    {
        return true;
    }

    let content = comment_content(text).trim_start();
    TOOL_DIRECTIVES
        .iter()
        .any(|directive| content.starts_with(directive))
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
/// produce no diagnostic. An exempt comment above starts no run unless its
/// exemption spans the lines under it.
fn continues_run(node: Node<'_>, syntax: Syntax, parent_text: &str, parent_start: usize) -> bool {
    let Some(previous) = node.prev_sibling() else {
        return false;
    };
    let previous_text = text_of(previous, parent_text, parent_start);
    if syntax.is_exempt(previous_text) && !syntax.exemption_spans_lines() {
        return false;
    }

    syntax.is_plain_comment(previous, previous_text)
        && previous.end_position().row + 1 == node.start_position().row
}

/// Returns the end byte of the comment run that starts at this node
///
/// The run stops short of an exempt comment, so no diagnostic spans one.
fn run_end(node: Node<'_>, syntax: Syntax, parent_text: &str, parent_start: usize) -> usize {
    let mut last = node;
    while let Some(next) = last.next_sibling() {
        let next_text = text_of(next, parent_text, parent_start);
        let joins = syntax.is_plain_comment(next, next_text)
            && last.end_position().row + 1 == next.start_position().row
            && !syntax.is_exempt(next_text);
        if !joins {
            break;
        }
        last = next;
    }

    last.end_byte()
}

/// Returns the diagnostic for a comment node, if the rule reports one
fn check_comment(node: &DecoratedNode<'_>, syntax: Syntax) -> Vec<Diagnostic> {
    let raw = node.raw();
    let text = node.text();
    if syntax.is_doc_comment(raw, text) || syntax.is_exempt(text) || is_file_header(raw, syntax) {
        return Vec::new();
    }

    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    let parent_text = parent.text();
    let parent_start = parent.raw().start_byte();
    if continues_run(raw, syntax, parent_text, parent_start) {
        return Vec::new();
    }

    let span = node.span();
    let file = span.file_path().clone();
    let end = run_end(raw, syntax, parent_text, parent_start);
    let span = Span::builder()
        .file(file)
        .start(span.start())
        .end(end)
        .build();

    let message = match classify(text) {
        CommentKind::Divider => "remove this section divider",
        CommentKind::Prose => "remove this comment or move what it explains into a doc comment",
    };

    vec![
        Diagnostic::builder()
            .rule_id(RuleId::new("lint.inline-comment"))
            .severity(Severity::Error)
            .message(message)
            .span(span)
            .build(),
    ]
}

impl RustLintPass for InlineComment {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_comment(node, Syntax::Rust)
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_comment(node, Syntax::Rust)
    }
}

impl TypeScriptLintPass for InlineComment {
    fn check_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_comment(node, Syntax::TypeScript)
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for InlineComment {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.inline-comment")]
    }
}

stiles_plugin::export_lints![rust: [InlineComment], typescript: [InlineComment]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass};

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
    }

    #[test]
    fn comment_above_a_safety_comment_is_flagged_at_the_note_span() {
        let source = "fn f() {\n    // an ordinary note\n    // SAFETY: the caller checks the pointer\n    unsafe { g() }\n}";
        let start = source.find("// an").expect("source contains the note");

        let diagnostics = run(source);

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
    fn comment_after_the_file_header_is_flagged_once() {
        let source = "// Copyright 2026 The Stiles Authors\n\n// an ordinary comment\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn comment_after_the_file_header_is_flagged_with_a_doc_comment_hint() {
        let source = "// Copyright 2026 The Stiles Authors\n\n// an ordinary comment\nfn f() {}";

        let diagnostics = run(source);

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
    fn comment_run_is_reported_once() {
        let source = "fn f() {\n    // first\n    // second\n}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn comment_run_reports_its_whole_span() {
        let source = "fn f() {\n    // first\n    // second\n}";
        let start = source
            .find("// first")
            .expect("source contains the first line");
        let end = source
            .find("// second")
            .expect("source contains the second line")
            + "// second".len();

        let diagnostics = run(source);

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

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.inline-comment")
            .has_severity(Severity::Error)
            .message_contains("section divider");
    }

    #[test]
    fn divider_is_reported_once() {
        let diagnostics = run("use std::fmt;\n// ========================\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
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
        let diagnostics = run("/* Copyright 2026 The Stiles Authors */\n\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn file_header_comment_is_not_flagged() {
        let source = "// Copyright 2026 The Stiles Authors\n// SPDX-License-Identifier: Apache-2.0\n\nuse std::fmt;\n\nfn f() {}";

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
    fn module_level_comment_is_flagged_once() {
        let diagnostics = run("use std::fmt;\n// helpers live below\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn module_level_comment_is_flagged_with_an_error_that_suggests_a_doc_comment() {
        let diagnostics = run("use std::fmt;\n// helpers live below\nfn f() {}");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.inline-comment")
            .has_severity(Severity::Error)
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

        assert_diagnostic(&diagnostics[0]).message_contains("section divider");
    }

    #[test]
    fn section_header_is_reported_once() {
        let diagnostics = run("use std::fmt;\n// --- Helper functions ---\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
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
    }

    #[test]
    fn trait_send_comment_kind() {
        fn assert_send<T: Send>() {}
        assert_send::<CommentKind>();
    }

    #[test]
    fn trait_send_syntax() {
        fn assert_send<T: Send>() {}
        assert_send::<Syntax>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<InlineComment>();
    }

    #[test]
    fn trait_sync_comment_kind() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<CommentKind>();
    }

    #[test]
    fn trait_sync_syntax() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Syntax>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<InlineComment>();
    }

    #[test]
    fn trait_unpin_comment_kind() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<CommentKind>();
    }

    #[test]
    fn trait_unpin_syntax() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Syntax>();
    }

    mod typescript {
        use stiles_typescript::TypeScriptLintPassAdapter;

        use super::*;

        fn run(source: &str) -> Vec<Diagnostic> {
            let tree = parse(source, Language::TypeScript);
            let mut passes: Vec<Box<dyn LintPass>> =
                vec![Box::new(TypeScriptLintPassAdapter::new(InlineComment))];
            execute(&tree, &mut passes)
        }

        #[test]
        fn block_comment_is_flagged_once() {
            let diagnostics = run("const a = 1;\n/* explains a */\nconst b = a;");

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn block_comment_is_flagged_under_the_inline_comment_rule() {
            let diagnostics = run("const a = 1;\n/* explains a */\nconst b = a;");

            assert_diagnostic(&diagnostics[0]).has_rule_id("lint.inline-comment");
        }

        #[test]
        fn comment_run_is_reported_once() {
            let source = "const a = 1;\n// first\n// second\nconst b = a;";

            let diagnostics = run(source);

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn comment_run_reports_its_whole_span() {
            let source = "const a = 1;\n// first\n// second\nconst b = a;";
            let start = source.find("// first").unwrap();
            let end = source.find("second").unwrap() + "second".len();

            let diagnostics = run(source);

            assert_diagnostic(&diagnostics[0]).has_span("<test>", start, end);
        }

        #[test]
        fn comment_under_a_directive_is_flagged_at_its_own_span() {
            let source = "const a = 1;\n// eslint-disable-next-line no-console\n// log while debugging\nconsole.log(a);";
            let start = source.find("// log").unwrap();

            let diagnostics = run(source);

            assert_diagnostic(&diagnostics[0]).has_span(
                "<test>",
                start,
                start + "// log while debugging".len(),
            );
        }

        #[test]
        fn comment_under_a_directive_is_flagged_once() {
            let source = "const a = 1;\n// eslint-disable-next-line no-console\n// log while debugging\nconsole.log(a);";

            let diagnostics = run(source);

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn divider_is_reported_as_a_section_divider() {
            let diagnostics = run("const a = 1;\n// ---- helpers ----\nconst b = a;");

            assert_diagnostic(&diagnostics[0]).message_contains("section divider");
        }

        #[test]
        fn divider_is_reported_once() {
            let diagnostics = run("const a = 1;\n// ---- helpers ----\nconst b = a;");

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn doc_comment_is_not_flagged() {
            let diagnostics = run("const a = 1;\n/** Returns the sum */\nfunction add() {}");

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn file_header_after_a_hash_bang_is_not_flagged() {
            let diagnostics = run("#!/usr/bin/env node\n// Copyright Aonyx\nconst a = 1;");

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn file_header_is_not_flagged() {
            let diagnostics =
                run("// Copyright Aonyx\n// SPDX-License-Identifier: MIT\nconst a = 1;");

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn is_tool_directive_with_prose_returns_false() {
            let directive = is_tool_directive("// we disable eslint here");

            assert!(!directive);
        }

        #[test]
        fn plain_comment_in_a_function_body_is_flagged() {
            let diagnostics = run("const a = 1;\nfunction f() {\n  // compute\n  return 1;\n}");

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn pure_annotation_is_not_flagged() {
            let diagnostics = run("const a = 1;\nconst b = /* @__PURE__ */ create();");

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn suppressions_are_not_flagged() {
            let source = "const a = 1;\n\
                // @ts-expect-error the types lag the runtime\n\
                f(a);\n\
                /* eslint-disable no-console */\n\
                // prettier-ignore\n\
                const m = [1,0,0,1];\n\
                /* istanbul ignore next */\n\
                // biome-ignore lint: generated\n\
                const g = 1;";

            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn triple_slash_directive_is_not_flagged() {
            let diagnostics = run("const a = 1;\n/// <reference types=\"node\" />\nconst b = a;");

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn triple_slash_prose_is_flagged() {
            let diagnostics = run("const a = 1;\n/// explains b\nconst b = a;");

            assert_eq!(diagnostics.len(), 1);
        }
    }
}
