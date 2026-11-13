use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity, Span};
use stiles_typescript::TypeScriptLintPass;

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(inline_comment::RULE_ID.as_str(), "lint.inline-comment");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.inline-comment");

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
/// ```text
/// // DON'T
/// fn retry_delay(attempt: u32) -> Duration {
///     // double the wait after each failed attempt
///     Duration::from_millis(100 * 2u64.pow(attempt))
/// }
///
/// // DO
/// /// Returns the wait before a retry, which doubles after each failed attempt
/// fn retry_delay(attempt: u32) -> Duration {
///     Duration::from_millis(100 * 2u64.pow(attempt))
/// }
/// ```
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
    fn is_comment(self, node: DecoratedNode<'_>) -> bool {
        match self {
            Syntax::Rust => node.kind() == "line_comment" || node.kind() == "block_comment",
            Syntax::TypeScript => node.kind() == "comment",
        }
    }

    /// Returns whether the comment is documentation
    fn is_doc_comment(self, node: DecoratedNode<'_>, text: &str) -> bool {
        match self {
            Syntax::Rust => is_doc_comment(node),
            Syntax::TypeScript => text.starts_with("/**") && text != "/**/",
        }
    }

    /// Returns whether the node is a comment that is not documentation
    fn is_plain_comment(self, node: DecoratedNode<'_>, text: &str) -> bool {
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
fn is_doc_comment(node: DecoratedNode<'_>) -> bool {
    node.child_by_field_name("outer").is_some() || node.child_by_field_name("inner").is_some()
}

/// Returns whether the comment opens the file
///
/// A copyright or license header sits above everything else, and the tree
/// puts it first under the file's root. The check is structural, so it
/// exempts whatever opens the file, header or not. The rule still reports
/// every later run. A TypeScript file may open with a `#!` line, which a
/// header follows.
fn is_file_header(node: DecoratedNode<'_>, syntax: Syntax) -> bool {
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

/// Returns whether this comment belongs to a run that starts above it
///
/// Comments in a run are siblings with one line break between them. The rule
/// reports a run at most once, at its first comment, so the ones below it
/// produce no diagnostic. An exempt comment above starts no run unless its
/// exemption spans the lines under it.
fn continues_run(node: DecoratedNode<'_>, syntax: Syntax) -> bool {
    let Some(previous) = node.prev_sibling() else {
        return false;
    };
    let previous_text = previous.text();
    if syntax.is_exempt(previous_text) && !syntax.exemption_spans_lines() {
        return false;
    }

    syntax.is_plain_comment(previous, previous_text)
        && previous.end_position().row + 1 == node.start_position().row
}

/// Returns the end byte of the comment run that starts at this node
///
/// The run stops short of an exempt comment, so no diagnostic spans one.
fn run_end(node: DecoratedNode<'_>, syntax: Syntax) -> usize {
    let mut last = node;
    while let Some(next) = last.next_sibling() {
        let next_text = next.text();
        let joins = syntax.is_plain_comment(next, next_text)
            && last.end_position().row + 1 == next.start_position().row
            && !syntax.is_exempt(next_text);
        if !joins {
            break;
        }
        last = next;
    }

    last.span().end()
}

/// Returns the diagnostic for a comment node, if the rule reports one
fn check_comment(node: &DecoratedNode<'_>, syntax: Syntax) -> Vec<Diagnostic> {
    let text = node.text();
    if syntax.is_doc_comment(*node, text)
        || syntax.is_exempt(text)
        || is_file_header(*node, syntax)
        || continues_run(*node, syntax)
    {
        return Vec::new();
    }

    let span = node.span();
    let file = span.file_path().clone();
    let end = run_end(*node, syntax);
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
            .rule_id(RULE_ID)
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
        vec![RULE_ID]
    }
}

#[cfg(feature = "plugin")]
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

        assert_diagnostic(&diagnostics[0])
            .has_span("<test>", start..start + "// an ordinary note".len());
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

        assert_diagnostic(&diagnostics[0]).has_span("<test>", start..end);
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

    fn run_documented(source: &str) -> Vec<stiles_types::Diagnostic> {
        let tree = stiles_testing::parse(source, stiles_types::Language::Rust);
        stiles_testing::execute(
            &tree,
            &mut [Box::new(stiles_rust::RustLintPassAdapter::new(
                InlineComment,
            ))],
        )
    }

    #[test]
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_documented(pair.example());

        assert!(!diagnostics.is_empty());
    }

    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_documented(pair.fix());

        stiles_testing::assert_no_diagnostics(&diagnostics);
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

            assert_diagnostic(&diagnostics[0]).has_span("<test>", start..end);
        }

        #[test]
        fn comment_under_a_directive_is_flagged_at_its_own_span() {
            let source = "const a = 1;\n// eslint-disable-next-line no-console\n// log while debugging\nconsole.log(a);";
            let start = source.find("// log").unwrap();

            let diagnostics = run(source);

            assert_diagnostic(&diagnostics[0])
                .has_span("<test>", start..start + "// log while debugging".len());
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
