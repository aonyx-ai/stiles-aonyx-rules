mod doc_block;
mod inline_link;

use std::slice;

use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity};
use whisker_typescript::TypeScriptLintPass;

use self::doc_block::DocBlock;
use self::inline_link::InlineLink;

const RULE_ID: RuleId = RuleId::new("lint.inline-doc-link");

/// Flags inline Markdown links in doc comments
///
/// An inline link buries the URL in the prose and repeats it at every
/// mention. A reference-style link keeps the sentence readable and gives
/// the target one definition.
///
/// The rule reads the link syntax only. It leaves `[text]`, `[text][]`, and
/// `[text][label]` alone, because those are the forms it asks for. Rustdoc
/// resolves most bracketed paths on its own, so a doc comment that uses one
/// needs no definition at the bottom.
///
/// A backticked identifier in running prose is out of scope. Nothing in the
/// syntax says whether the name is a type, a field, a crate, or a CLI flag,
/// so a rule that demanded brackets around it would be guessing.
///
/// In TypeScript the rule reads a `/** */` doc comment. A `{@link Target}`
/// tag is not Markdown, so it is left alone, and an `@example` section is
/// code, so its brackets are never read as links.
pub struct InlineDocLink;

impl TypeScriptLintPass for InlineDocLink {
    fn check_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_typescript(node)
    }
}

impl RustLintPass for InlineDocLink {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check(slice::from_ref(node))
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(group) = doc_comment_group(node) else {
            return Vec::new();
        };

        check(&group)
    }
}

/// Which marker introduces a doc comment
///
/// A `///` run and a `//!` run never form one document, so the rule must
/// not join them.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum DocMarker {
    /// The `//!` marker
    Inner,
    /// The `///` marker
    Outer,
}

impl DocMarker {
    /// Returns the marker of a doc comment line, if the node is one
    fn of(node: &DecoratedNode<'_>) -> Option<Self> {
        if node.kind() != "line_comment" {
            return None;
        }
        node.child_by_field_name("doc")?;

        match node.child_by_field_name("inner") {
            Some(_) => Some(DocMarker::Inner),
            None => node.child_by_field_name("outer").map(|_| DocMarker::Outer),
        }
    }
}

/// Reports every inline link in one doc comment
fn check(comments: &[DecoratedNode<'_>]) -> Vec<Diagnostic> {
    let Some(block) = DocBlock::from_comments(comments) else {
        return Vec::new();
    };

    report(&block)
}

/// Reports every inline link in one TypeScript doc comment
///
/// A `//` or `/* */` comment carries no documentation, so it reports
/// nothing. Neither does an `@example` section, which is code.
fn check_typescript(node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let Some(lines) = jsdoc::prose_lines(node.text(), node.span().start()) else {
        return Vec::new();
    };
    let lines = lines
        .into_iter()
        .map(|line| (line.offset(), line.text()))
        .collect();
    let Some(block) = DocBlock::from_lines(node.span().file_path().clone(), lines) else {
        return Vec::new();
    };

    report(&block)
}

/// Turns the inline links in a joined doc comment into diagnostics
fn report(block: &DocBlock) -> Vec<Diagnostic> {
    InlineLink::find_all(block.content())
        .into_iter()
        .map(|link| {
            Diagnostic::new(
                RULE_ID,
                Severity::Warn,
                link.kind().message().into(),
                block.span(link.start(), link.end()),
            )
        })
        .collect()
}

/// Returns the run of doc comment lines that `node` starts
///
/// Returns [`None`] when `node` is not a doc comment, or when the line
/// above it belongs to the same run. The walker visits every line of a run,
/// so only the first line may report; otherwise one link would produce a
/// diagnostic on every line of its doc comment.
///
/// [`None`]: std::option::Option::None
fn doc_comment_group<'a>(node: &DecoratedNode<'a>) -> Option<Vec<DecoratedNode<'a>>> {
    DocMarker::of(node)?;

    let parent = node.parent()?;
    let siblings = parent.named_children();
    let index = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())?;

    if index > 0 && continues(&siblings[index - 1], node) {
        return None;
    }

    let mut group = vec![*node];
    for candidate in siblings.iter().skip(index + 1) {
        if !continues(
            group.last().expect("the run starts with one line"),
            candidate,
        ) {
            break;
        }
        group.push(*candidate);
    }

    Some(group)
}

/// Returns whether `candidate` carries on the doc comment `previous` started
///
/// The two lines must use the same marker and sit on adjacent rows. A blank
/// source line and an ordinary comment both end a run. Rustdoc joins those
/// runs back together, so a code fence that spans the gap escapes the rule.
///
/// The comparison uses the start rows. A `line_comment` node covers the
/// newline that ends it, so its end row is already the row below.
fn continues(previous: &DecoratedNode<'_>, candidate: &DecoratedNode<'_>) -> bool {
    let Some(previous_marker) = DocMarker::of(previous) else {
        return false;
    };
    let Some(candidate_marker) = DocMarker::of(candidate) else {
        return false;
    };
    if previous_marker != candidate_marker {
        return false;
    }

    previous.raw().start_position().row + 1 == candidate.raw().start_position().row
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for InlineDocLink {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.inline-doc-link")]
    }
}

whisker_plugin::export_lints![rust: [InlineDocLink], typescript: [InlineDocLink]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(InlineDocLink))]
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);

        execute(&tree, &mut passes())
    }

    #[test]
    fn attribute_doc_is_not_flagged() {
        let source = "#[doc = \"See [a](https://example.com)\"]\nfn f() {}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn block_doc_comment_with_inline_link_is_flagged() {
        let source = "/** See [a](https://example.com) now */\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn field_doc_comment_with_inline_link_is_flagged_once() {
        let source =
            "struct S {\n    /// See\n    /// [a](https://example.com)\n    field: u32,\n}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn image_link_is_flagged_as_an_image() {
        let source = "/// ![diagram](https://example.com/d.png)\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.inline-doc-link")
            .has_severity(Severity::Warn)
            .message_contains("image");
    }

    #[test]
    fn inline_intra_doc_link_is_flagged() {
        let source = "/// Uses [`HashMap`](std::collections::HashMap) internally\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inline_link_in_doc_comment_is_flagged() {
        let source = "/// See [the docs](https://example.com/docs) for details\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.inline-doc-link")
            .has_severity(Severity::Warn)
            .message_contains("reference-style link");
    }

    #[test]
    fn inline_link_in_plain_block_comment_is_not_flagged() {
        let source = "/* See [a](https://example.com) */\nfn f() {}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inline_link_in_plain_comment_is_not_flagged() {
        let source = "// See [a](https://example.com)\nfn f() {}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inline_link_reports_the_span_of_the_link() {
        let source = "/// See [a](https://example.com) now\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
        let span = diagnostics[0].span();
        assert_eq!(
            &source[span.start()..span.end()],
            "[a](https://example.com)"
        );
    }

    #[test]
    fn inner_and_outer_doc_comments_are_separate_blocks() {
        let source = "//! ```\n/// [a](https://example.com)\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inner_doc_comment_with_inline_link_is_flagged() {
        let source = "//! See [a](https://example.com)\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn link_across_two_doc_comment_lines_is_flagged_once() {
        let source = "/// See [the\n/// docs](https://example.com) now\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn link_in_code_span_is_not_flagged() {
        let source = "/// Write `[text](url)` for a link\nfn f() {}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn link_in_fenced_example_is_not_flagged() {
        let source = "/// # Examples\n///\n/// ```\n/// let a = \"[x](https://example.com)\";\n/// ```\nfn f() {}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn plain_comment_between_doc_lines_splits_the_blocks() {
        let source = "/// ```\n// break\n/// [a](https://example.com)\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn reference_definition_is_not_flagged() {
        let source = "/// See [the docs]\n///\n/// [the docs]: https://example.com/docs\nfn f() {}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn shortcut_reference_link_is_not_flagged() {
        let source = "/// Returns [`Option<T>`] if the value exists\n///\n/// [`Option<T>`]: std::option::Option\nfn f() {}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DocMarker>();
        assert_send::<InlineDocLink>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DocMarker>();
        assert_sync::<InlineDocLink>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DocMarker>();
        assert_unpin::<InlineDocLink>();
    }

    #[test]
    fn two_doc_comments_each_report_their_link() {
        let source = "/// [a](https://example.com/a)\nfn f() {}\n\n/// [b](https://example.com/b)\nfn g() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn two_links_in_one_doc_comment_report_twice() {
        let source =
            "/// See [a](https://example.com/a)\n/// and [b](https://example.com/b)\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 2);
    }

    mod typescript {
        use whisker_typescript::TypeScriptLintPassAdapter;

        use super::*;

        fn run(source: &str) -> Vec<Diagnostic> {
            let tree = parse(source, Language::TypeScript);
            let mut passes: Vec<Box<dyn LintPass>> =
                vec![Box::new(TypeScriptLintPassAdapter::new(InlineDocLink))];

            execute(&tree, &mut passes)
        }

        #[test]
        fn doc_comment_with_inline_link_is_flagged_at_the_link() {
            let source = "/**\n * Follows [the spec](https://example.com/spec) exactly\n */\nfunction f() {}";

            let diagnostics = run(source);

            assert_eq!(diagnostics.len(), 1);
            let start = source.find("[the spec]").unwrap();
            let end = source.find(") exactly").unwrap() + 1;
            assert_diagnostic(&diagnostics[0])
                .has_rule_id("lint.inline-doc-link")
                .has_span("<test>", start, end);
        }

        #[test]
        fn doc_comment_with_link_tag_is_not_flagged() {
            let source = "/**\n * Wraps {@link Reader} and {@link Writer | the writer}\n */\nfunction f() {}";

            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn doc_comment_with_reference_link_is_not_flagged() {
            let source = "/**\n * Follows [the spec] exactly\n *\n * [the spec]: https://example.com/spec\n */\nfunction f() {}";

            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn example_section_is_not_read() {
            let source =
                "/**\n * Formats a link\n * @example\n * link([a](b))\n */\nfunction link() {}";

            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn line_comment_with_inline_link_is_not_read() {
            let source = "// see [the spec](https://example.com/spec)\nconst a = 1;";

            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }
    }
}
