mod doc_block;
mod inline_link;

use doc_comment::DocLine;
use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity};
use stiles_typescript::TypeScriptLintPass;

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
/// syntax says whether the name is a type, a field, a crate, or a CLI flag.
/// A rule that demanded brackets around it would be guessing.
///
/// In TypeScript the rule reads a `/** */` doc comment. A `{@link Target}`
/// tag is not Markdown, so it is left alone, and an `@example` section is
/// code, so its brackets are never read as links.
///
/// # Examples
///
/// ```
/// use inline_doc_link::InlineDocLink;
/// use stiles_rust::RustLintPassAdapter;
/// use stiles_testing::{execute, parse};
/// use stiles_types::{Language, LintPass};
///
/// let tree = parse("/// See [the guide](https://example.com) first\nfn f() {}", Language::Rust);
/// let pass: Box<dyn LintPass> = Box::new(RustLintPassAdapter::new(InlineDocLink));
/// let mut passes = vec![pass];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct InlineDocLink;

impl TypeScriptLintPass for InlineDocLink {
    fn check_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check(doc_comment::typescript(node), node)
    }
}

impl RustLintPass for InlineDocLink {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check(doc_comment::rust(node), node)
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check(doc_comment::rust(node), node)
    }
}

/// Reports every inline link in a doc comment, if `node` starts one
fn check(lines: Option<Vec<DocLine<'_>>>, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let Some(lines) = lines else {
        return Vec::new();
    };
    let Some(block) = DocBlock::builder()
        .file(node.span().file_path().clone())
        .doc(&lines)
        .build()
    else {
        return Vec::new();
    };

    report(&block)
}

/// Turns the inline links in a joined doc comment into diagnostics
fn report(block: &DocBlock) -> Vec<Diagnostic> {
    InlineLink::find_all(block.content())
        .into_iter()
        .map(|link| {
            Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Error)
                .message(link.kind().message())
                .span(block.span(link.start()..link.end()))
                .build()
        })
        .collect()
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for InlineDocLink {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.inline-doc-link")]
    }
}

stiles_plugin::export_lints![rust: [InlineDocLink], typescript: [InlineDocLink]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass};

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

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.inline-doc-link")
            .has_severity(Severity::Error)
            .message_contains("image");
    }

    #[test]
    fn image_link_is_flagged_once() {
        let source = "/// ![diagram](https://example.com/d.png)\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
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
    }

    #[test]
    fn inline_link_in_doc_comment_is_flagged_with_a_reference_link_hint() {
        let source = "/// See [the docs](https://example.com/docs) for details\nfn f() {}";

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.inline-doc-link")
            .has_severity(Severity::Error)
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
    fn inline_link_reports_one_diagnostic() {
        let source = "/// See [a](https://example.com) now\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inline_link_reports_the_span_of_the_link() {
        let source = "/// See [a](https://example.com) now\nfn f() {}";

        let diagnostics = run(source);

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
    fn link_in_fenced_block_doc_comment_is_not_flagged() {
        let source =
            "/**\n * ```\n * let a = \"[x](https://example.com)\";\n * ```\n */\nfn f() {}";

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
        assert_send::<InlineDocLink>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<InlineDocLink>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
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
        use stiles_typescript::TypeScriptLintPassAdapter;

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

            let start = source.find("[the spec]").unwrap();
            let end = source.find(") exactly").unwrap() + 1;
            assert_diagnostic(&diagnostics[0])
                .has_rule_id("lint.inline-doc-link")
                .has_span("<test>", start, end);
        }

        #[test]
        fn doc_comment_with_inline_link_is_flagged_once() {
            let source = "/**\n * Follows [the spec](https://example.com/spec) exactly\n */\nfunction f() {}";

            let diagnostics = run(source);

            assert_eq!(diagnostics.len(), 1);
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
