mod doc_block;
mod fix;
mod inline_link;

use doc_comment::DocLine;
use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity};
use stiles_typescript::TypeScriptLintPass;

use self::doc_block::DocBlock;
use self::fix::{Comment, Syntax};
use self::inline_link::InlineLink;

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(inline_doc_link::RULE_ID.as_str(), "lint.inline-doc-link");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.inline-doc-link");

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
/// `stiles check --fix` rewrites each link as `[text]` and adds `[text]: url`
/// at the end of the doc comment, after a blank doc line. When the comment
/// already defines `[text]` with the same URL, the fix only rewrites the
/// link. In TypeScript the definition goes before the first block tag.
///
/// The rule attaches no fix when the new label could need a choice or change
/// what other text means. That covers link text with formatting or brackets.
/// It covers a label that the comment defines with another URL, or uses in
/// other brackets. It also covers a link that spans two lines, or that
/// touches a bracket, a parenthesis, or a colon. A link in a TypeScript block
/// tag gets no fix, and neither does a comment that ends in an open code
/// fence. A block comment gets a fix only when every line between its first
/// and its last opens with `*`. Its `*/` must sit alone on the last line.
///
/// # Examples
///
/// ```text
/// // DON'T
/// /// Parses a config file in [TOML](https://toml.io) format
/// fn parse_config(text: &str) -> Config { todo!() }
///
/// // DO
/// /// Parses a config file in [TOML] format
/// ///
/// /// [TOML]: https://toml.io
/// fn parse_config(text: &str) -> Config { todo!() }
/// ```
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
        check(doc_comment::typescript(node), node, Syntax::TypeScript)
    }
}

impl RustLintPass for InlineDocLink {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check(doc_comment::rust(node), node, Syntax::Rust)
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check(doc_comment::rust(node), node, Syntax::Rust)
    }
}

/// Reports every inline link in a doc comment, if `node` starts one
fn check(
    lines: Option<Vec<DocLine<'_>>>,
    node: &DecoratedNode<'_>,
    syntax: Syntax,
) -> Vec<Diagnostic> {
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
    let comment = Comment {
        node,
        lines: &lines,
        syntax,
    };

    report(comment, &block)
}

/// Turns the inline links in a joined doc comment into diagnostics
///
/// Each finding whose link the fix of the comment rewrites carries that fix.
fn report(comment: Comment<'_, '_>, block: &DocBlock) -> Vec<Diagnostic> {
    let links = InlineLink::find_all(block.content());
    let fix = self::fix::fix(comment, block, &links);

    links
        .iter()
        .enumerate()
        .map(|(index, link)| {
            let diagnostic = Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Error)
                .message(link.kind().message())
                .span(block.span(link.start()..link.end()))
                .build();
            let fixed = fix
                .as_ref()
                .and_then(|fix| fix.covering(index))
                .and_then(|fix| diagnostic.clone().with_fix(fix.clone()).ok());
            fixed.unwrap_or(diagnostic)
        })
        .collect()
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for InlineDocLink {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.inline-doc-link")]
    }

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::none()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [InlineDocLink], typescript: [InlineDocLink]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_fix, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass};

    use super::*;

    /// Asserts that the rule reports `source` and attaches no fix to any
    /// finding
    fn assert_unfixed(source: &str) {
        let diagnostics = run(source);

        assert!(!diagnostics.is_empty(), "the rule should report {source:?}");
        for diagnostic in &diagnostics {
            assert_diagnostic(diagnostic).has_no_fix();
        }
    }

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
    fn block_doc_comment_on_one_line_has_no_fix() {
        assert_unfixed("/** See [a](https://example.com) now */\nfn f() {}");
    }

    fn run_documented(source: &str) -> Vec<stiles_types::Diagnostic> {
        let tree = stiles_testing::parse(source, stiles_types::Language::Rust);
        stiles_testing::execute(
            &tree,
            &mut [Box::new(stiles_rust::RustLintPassAdapter::new(
                InlineDocLink,
            ))],
        )
    }

    #[test]
    fn block_doc_comment_with_inline_link_is_flagged() {
        let source = "/** See [a](https://example.com) now */\nfn f() {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn block_doc_comment_without_stars_has_no_fix() {
        assert_unfixed("/**\n   See [a](https://example.com)\n */\nfn f() {}");
    }
    #[test]
    fn definition_like_line_inside_a_paragraph_has_no_fix() {
        assert_unfixed("/// See [a](https://example.com)\n/// [a]: https://example.com\nfn f() {}");
    }

    #[test]
    fn definition_with_another_url_has_no_fix() {
        assert_unfixed(
            "/// See [a](https://example.com/a)\n///\n/// [a]: https://example.com/b\nfn f() {}",
        );
    }

    #[test]
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        pair.assert_example(run_documented);
    }

    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_documented(pair.fix());

        stiles_testing::assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_destination_has_no_fix() {
        assert_unfixed("/// See [a]() now\nfn f() {}");
    }

    #[test]
    fn field_doc_comment_with_inline_link_is_flagged_once() {
        let source =
            "struct S {\n    /// See\n    /// [a](https://example.com)\n    field: u32,\n}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn fix_adds_a_definition_after_a_blank_line_that_ends_the_comment() {
        assert_fix()
            .tree(parse(
                "/// See [a](https://example.com)\n///\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a]\n///\n/// [a]: https://example.com\nfn f() {}");
    }

    #[test]
    fn fix_adds_a_definition_after_a_closed_code_fence() {
        assert_fix()
            .tree(parse(
                "/// See [a](https://example.com)\n///\n/// ```\n/// f();\n/// ```\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a]\n///\n/// ```\n/// f();\n/// ```\n///\n/// [a]: https://example.com\nfn f() {}");
    }

    #[test]
    fn fix_adds_a_definition_after_the_existing_definitions() {
        assert_fix()
            .tree(parse(
                "/// See [a](https://example.com/a) and [b]\n///\n/// [b]: https://example.com/b\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a] and [b]\n///\n/// [b]: https://example.com/b\n/// [a]: https://example.com/a\nfn f() {}");
    }

    #[test]
    fn fix_adds_one_definition_for_two_links_with_the_same_text_and_url() {
        assert_fix()
            .tree(parse(
                "/// See [a](https://example.com) and [a](https://example.com)\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a] and [a]\n///\n/// [a]: https://example.com\nfn f() {}");
    }

    #[test]
    fn fix_adds_the_definitions_of_every_link_in_one_round() {
        assert_fix()
            .tree(parse(
                "/// See [a](https://example.com/a)\n/// and [b](https://example.com/b)\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a]\n/// and [b]\n///\n/// [a]: https://example.com/a\n/// [b]: https://example.com/b\nfn f() {}");
    }

    #[test]
    fn fix_keeps_a_crlf_line_ending() {
        assert_fix()
            .tree(parse(
                "/// See [a](https://example.com)\r\nfn f() {}\r\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a]\r\n///\r\n/// [a]: https://example.com\r\nfn f() {}\r\n");
    }

    #[test]
    fn fix_keeps_an_angle_bracket_destination() {
        assert_fix()
            .tree(parse(
                "/// See [a](<https://example.com/a b>)\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a]\n///\n/// [a]: <https://example.com/a b>\nfn f() {}");
    }

    #[test]
    fn fix_keeps_the_indentation_of_a_field_doc() {
        assert_fix()
            .tree(parse(
                "struct S {\n    /// See [a](https://example.com)\n    field: u32,\n}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("struct S {\n    /// See [a]\n    ///\n    /// [a]: https://example.com\n    field: u32,\n}");
    }

    #[test]
    fn fix_keeps_the_title_of_the_link() {
        assert_fix()
            .tree(parse(
                "/// See [a](https://example.com \"The site\")\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a]\n///\n/// [a]: https://example.com \"The site\"\nfn f() {}");
    }

    #[test]
    fn fix_only_rewrites_a_link_whose_definition_exists() {
        assert_fix()
            .tree(parse(
                "/// See [a](https://example.com)\n///\n/// [a]: https://example.com\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// See [a]\n///\n/// [a]: https://example.com\nfn f() {}");
    }

    #[test]
    fn fix_rewrites_a_block_doc_comment() {
        assert_fix()
            .tree(parse(
                "/**\n * See [a](https://example.com)\n */\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/**\n * See [a]\n *\n * [a]: https://example.com\n */\nfn f() {}");
    }

    #[test]
    fn fix_rewrites_a_block_doc_comment_with_prose_on_the_first_line() {
        assert_fix()
            .tree(parse(
                "/** See [a](https://example.com)\n */\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/** See [a]\n *\n * [a]: https://example.com\n */\nfn f() {}");
    }

    #[test]
    fn fix_rewrites_an_image_link() {
        assert_fix()
            .tree(parse(
                "/// ![diagram](https://example.com/d.png)\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// ![diagram]\n///\n/// [diagram]: https://example.com/d.png\nfn f() {}");
    }

    #[test]
    fn fix_rewrites_an_inner_doc_comment() {
        assert_fix()
            .tree(parse("//! See [a](https://example.com)\n", Language::Rust))
            .passes(passes)
            .produces("//! See [a]\n//!\n//! [a]: https://example.com\n");
    }

    #[test]
    fn fix_rewrites_each_doc_comment_on_its_own() {
        assert_fix()
            .tree(parse(
                "/// [a](https://example.com/a)\nfn f() {}\n\n/// [b](https://example.com/b)\nfn g() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("/// [a]\n///\n/// [a]: https://example.com/a\nfn f() {}\n\n/// [b]\n///\n/// [b]: https://example.com/b\nfn g() {}");
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
    fn link_across_two_lines_has_no_fix() {
        assert_unfixed("/// See [the\n/// docs](https://example.com) now\nfn f() {}");
    }

    #[test]
    fn link_after_brackets_has_no_fix() {
        assert_unfixed("/// See [b][a](https://example.com) now\nfn f() {}");
    }

    #[test]
    fn link_before_a_colon_has_no_fix() {
        assert_unfixed("/// [a](https://example.com): the site\nfn f() {}");
    }

    #[test]
    fn link_before_a_parenthesis_has_no_fix() {
        assert_unfixed("/// See [a](https://example.com)(b) now\nfn f() {}");
    }

    #[test]
    fn link_before_brackets_has_no_fix() {
        assert_unfixed(
            "/// See [a](https://example.com)[b] now\n///\n/// [b]: https://example.com/b\nfn f() {}",
        );
    }

    #[test]
    fn link_beside_a_link_with_a_fix_keeps_no_fix() {
        let diagnostics =
            run("/// See [`a`](https://example.com/a) and [b](https://example.com/b)\nfn f() {}");

        assert_diagnostic(&diagnostics[0]).has_no_fix();
    }

    #[test]
    fn link_beside_a_link_without_a_fix_takes_the_fix() {
        let diagnostics =
            run("/// See [`a`](https://example.com/a) and [b](https://example.com/b)\nfn f() {}");

        assert_diagnostic(&diagnostics[1]).has_fix();
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
    fn link_text_used_as_a_shortcut_elsewhere_has_no_fix() {
        assert_unfixed("/// Holds a [Vec] and a [Vec](https://example.com)\nfn f() {}");
    }

    #[test]
    fn link_text_with_a_code_span_has_no_fix() {
        assert_unfixed("/// Uses [`HashMap`](std::collections::HashMap) internally\nfn f() {}");
    }

    #[test]
    fn link_text_with_emphasis_has_no_fix() {
        assert_unfixed("/// See [*the* docs](https://example.com)\nfn f() {}");
    }

    #[test]
    fn link_text_with_nested_brackets_has_no_fix() {
        assert_unfixed("/// See [a [b] c](https://example.com)\nfn f() {}");
    }

    #[test]
    fn links_of_one_comment_carry_the_same_fix() {
        let diagnostics =
            run("/// See [a](https://example.com/a) and [b](https://example.com/b)\nfn f() {}");

        assert_eq!(diagnostics[0].fix(), diagnostics[1].fix());
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
    fn same_text_with_two_urls_has_no_fix() {
        assert_unfixed(
            "/// See [a](https://example.com/1) and [A](https://example.com/2)\nfn f() {}",
        );
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

    #[test]
    fn unclosed_code_fence_at_the_end_has_no_fix() {
        assert_unfixed("/// See [a](https://example.com)\n/// ```\n/// f();\nfn f() {}");
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

        fn passes() -> Vec<Box<dyn LintPass>> {
            vec![Box::new(TypeScriptLintPassAdapter::new(InlineDocLink))]
        }

        #[test]
        fn doc_comment_on_one_line_has_no_fix() {
            let diagnostics = run("/** See [a](https://example.com) now */\nfunction f() {}");

            assert_diagnostic(&diagnostics[0]).has_no_fix();
        }

        #[test]
        fn doc_comment_with_inline_link_is_flagged_at_the_link() {
            let source = "/**\n * Follows [the spec](https://example.com/spec) exactly\n */\nfunction f() {}";

            let diagnostics = run(source);

            let start = source.find("[the spec]").unwrap();
            let end = source.find(") exactly").unwrap() + 1;
            assert_diagnostic(&diagnostics[0])
                .has_rule_id("lint.inline-doc-link")
                .has_span("<test>", start..end);
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
        fn fix_adds_the_definition_at_the_end_of_the_comment() {
            assert_fix()
                .tree(parse(
                    "/**\n * Follows [the spec](https://example.com/spec) exactly\n */\nfunction f() {}",
                    Language::TypeScript,
                ))
                .passes(passes)
                .produces("/**\n * Follows [the spec] exactly\n *\n * [the spec]: https://example.com/spec\n */\nfunction f() {}");
        }

        #[test]
        fn fix_adds_the_definition_before_the_first_block_tag() {
            assert_fix()
                .tree(parse(
                    "/**\n * Follows [the spec](https://example.com/spec) exactly\n * @param a - the first\n */\nfunction f(a) {}",
                    Language::TypeScript,
                ))
                .passes(passes)
                .produces("/**\n * Follows [the spec] exactly\n *\n * [the spec]: https://example.com/spec\n *\n * @param a - the first\n */\nfunction f(a) {}");
        }

        #[test]
        fn fix_keeps_the_indentation_of_a_method_doc() {
            assert_fix()
                .tree(parse(
                    "class A {\n  /**\n   * See [a](https://example.com)\n   */\n  m() {}\n}",
                    Language::TypeScript,
                ))
                .passes(passes)
                .produces("class A {\n  /**\n   * See [a]\n   *\n   * [a]: https://example.com\n   */\n  m() {}\n}");
        }

        #[test]
        fn line_comment_with_inline_link_is_not_read() {
            let source = "// see [the spec](https://example.com/spec)\nconst a = 1;";

            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn link_in_a_block_tag_has_no_fix() {
            let diagnostics = run(
                "/**\n * Formats a value\n * @param a - see [the spec](https://example.com/spec)\n */\nfunction f(a) {}",
            );

            assert_diagnostic(&diagnostics[0]).has_no_fix();
        }
    }
}
