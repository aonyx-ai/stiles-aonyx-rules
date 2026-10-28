mod doc_line;
mod sentence;

use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity, Span};
use stiles_typescript::TypeScriptLintPass;

use crate::doc_line::DocLine;
use crate::sentence::sentences;

/// The most words one doc comment sentence may have
const MAX_WORDS: usize = 25;

/// Flags a doc comment sentence longer than 25 words
///
/// ASD-STE100 caps a descriptive sentence at 25 words, past which a reader
/// has to hold too much at once.
///
/// The rule reads the comment text, never the item it documents. It skips
/// what rustdoc does not render as prose: fenced code blocks, section
/// headings, link definitions, and table rows.
///
/// In TypeScript the rule reads a `/** */` doc comment. A block tag's
/// syntax, such as the name and hyphen of `@param a - the first`, is not
/// counted, and an `@example` section is code, so it is not read.
pub struct LongDocSentence;

impl RustLintPass for LongDocSentence {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report_doc(doc_comment::rust(node), node)
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report_doc(doc_comment::rust(node), node)
    }
}

impl TypeScriptLintPass for LongDocSentence {
    fn check_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report_doc(doc_comment::typescript(node), node)
    }
}

/// Reports the long sentences of a doc comment, if `node` starts one
fn report_doc(
    lines: Option<Vec<doc_comment::DocLine<'_>>>,
    node: &DecoratedNode<'_>,
) -> Vec<Diagnostic> {
    let Some(lines) = lines else {
        return Vec::new();
    };

    let lines: Vec<DocLine<'_>> = lines
        .into_iter()
        .map(|line| {
            DocLine::builder()
                .text(line.text())
                .offset(line.offset())
                .build()
        })
        .collect();
    report(&lines, node)
}

/// Reports every sentence in `lines` that goes over the word cap
fn report(lines: &[DocLine<'_>], node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let file = node.span();

    sentences(lines)
        .into_iter()
        .filter(|sentence| sentence.words() > MAX_WORDS)
        .map(|sentence| {
            Diagnostic::builder()
                .rule_id(RuleId::new("lint.long-doc-sentence"))
                .severity(Severity::Warn)
                .message(format!(
                    "doc sentence is {} words, over the {MAX_WORDS}-word limit",
                    sentence.words()
                ))
                .span(
                    Span::builder()
                        .file(file.file_path().clone())
                        .start(sentence.start())
                        .end(sentence.end())
                        .build(),
                )
                .build()
        })
        .collect()
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for LongDocSentence {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.long-doc-sentence")]
    }
}

stiles_plugin::export_lints![rust: [LongDocSentence], typescript: [LongDocSentence]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(LongDocSentence))]
    }

    fn check(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);

        execute(&tree, &mut passes())
    }

    fn words(count: usize) -> String {
        let words: Vec<String> = (0..count).map(|index| format!("word{index}")).collect();

        words.join(" ")
    }

    fn doc_of(count: usize) -> String {
        format!("/// {}.\npub struct Foo;\n", words(count))
    }

    #[test]
    fn block_doc_comment_over_the_limit_is_flagged() {
        let source = format!("/** {}. */\npub struct Foo;\n", words(30));

        let diagnostics = check(&source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn block_doc_comment_stars_are_not_counted() {
        let line = words(MAX_WORDS / 2);
        let source = format!("/**\n * {line}\n * {line}.\n */\npub struct Foo;\n");

        let diagnostics = check(&source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn block_inner_doc_comment_over_the_limit_is_flagged() {
        let source = format!("/*! {}. */\npub struct Foo;\n", words(30));

        let diagnostics = check(&source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn code_span_counts_as_one_word() {
        let source = "/// Reads `a b c d e f`, `g h i j k l`, `m n o p q r`, and `s t u v w x` now.\npub struct Foo;\n";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_comment_inside_an_impl_is_flagged() {
        let source = format!(
            "impl Foo {{\n    /// {}.\n    fn f(&self) {{}}\n}}\n",
            words(30)
        );

        let diagnostics = check(&source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn fenced_code_block_is_not_flagged() {
        let source = format!(
            "/// Summary\n///\n/// ```text\n/// {}.\n/// ```\npub struct Foo;\n",
            words(30)
        );

        let diagnostics = check(&source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_and_outer_markers_are_not_joined() {
        let half = words(15);
        let source = format!("//! {half}\n/// {half}\npub struct Foo;\n");

        let diagnostics = check(&source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_comment_over_the_limit_is_flagged() {
        let source = format!("//! {}.\n", words(30));

        let diagnostics = check(&source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn long_sentence_across_lines_is_flagged_as_a_warning_with_its_word_count() {
        let source = "/// Returns the coverage verdict for this file, which is empty when no decoration\n/// providers were configured, since a provider that declines still counts as\n/// having looked and the walk needs to distinguish that from never having run.\npub struct Foo;\n";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.long-doc-sentence")
            .has_severity(Severity::Warn)
            .message_contains("37 words");
    }

    #[test]
    fn long_sentence_across_lines_is_flagged_once() {
        let source = "/// Returns the coverage verdict for this file, which is empty when no decoration\n/// providers were configured, since a provider that declines still counts as\n/// having looked and the walk needs to distinguish that from never having run.\npub struct Foo;\n";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn long_sentence_across_lines_span_ends_at_the_sentence_end() {
        let source = "/// Returns the coverage verdict for this file, which is empty when no decoration\n/// providers were configured, since a provider that declines still counts as\n/// having looked and the walk needs to distinguish that from never having run.\npub struct Foo;\n";

        let diagnostics = check(source);

        let span = diagnostics[0].span();
        assert!(source[span.start()..span.end()].ends_with("having run."));
    }

    #[test]
    fn long_sentence_across_lines_span_starts_at_the_sentence_start() {
        let source = "/// Returns the coverage verdict for this file, which is empty when no decoration\n/// providers were configured, since a provider that declines still counts as\n/// having looked and the walk needs to distinguish that from never having run.\npub struct Foo;\n";

        let diagnostics = check(source);

        let span = diagnostics[0].span();
        assert!(source[span.start()..span.end()].starts_with("Returns the coverage"));
    }

    #[test]
    fn multibyte_prose_is_flagged_once() {
        let source = format!("/// 日本語 {}.\npub struct Foo;\n", words(30));

        let diagnostics = check(&source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn multibyte_prose_reports_a_span_on_a_character_boundary() {
        let source = format!("/// 日本語 {}.\npub struct Foo;\n", words(30));

        let diagnostics = check(&source);

        let span = diagnostics[0].span();
        assert!(source[span.start()..span.end()].starts_with("日本語"));
    }

    #[test]
    fn multibyte_text_is_not_flagged() {
        let source = "/// Résumé — 日本語 `多バイト` ünïcödé here.\n/// ``unbalanced ` span\npub struct Foo;\n";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn non_doc_comment_is_not_flagged() {
        let source = format!("// {}.\npub struct Foo;\n", words(40));

        let diagnostics = check(&source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn section_heading_is_not_flagged() {
        let source = format!("/// Summary\n///\n/// # {}\npub struct Foo;\n", words(30));

        let diagnostics = check(&source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn sentence_at_the_limit_is_not_flagged() {
        let diagnostics = check(&doc_of(MAX_WORDS));

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn sentence_over_the_limit_is_flagged_once() {
        let diagnostics = check(&doc_of(MAX_WORDS + 1));

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn sentence_over_the_limit_is_flagged_with_its_word_count() {
        let diagnostics = check(&doc_of(MAX_WORDS + 1));

        assert_diagnostic(&diagnostics[0]).message_contains("26 words");
    }

    #[test]
    fn separate_doc_blocks_are_not_joined() {
        let half = words(15);
        let source = format!("/// {half}\npub struct Foo;\n/// {half}\npub struct Bar;\n");

        let diagnostics = check(&source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn short_sentences_are_not_flagged() {
        let source = "/// Returns the coverage verdict for this file\n///\n/// The map is empty when no providers were configured. A provider that\n/// declines still counts as having looked.\npub struct Foo;\n";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn span_covers_one_sentence_only() {
        let source = doc_of(MAX_WORDS + 1);

        let diagnostics = check(&source);

        let span = diagnostics[0].span();
        let sentence = source[4..].lines().next().expect("the doc line");
        assert_eq!(&source[span.start()..span.end()], sentence);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<LongDocSentence>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<LongDocSentence>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<LongDocSentence>();
    }

    #[test]
    fn two_long_sentences_are_flagged_once_each() {
        let sentence = words(30);
        let source = format!("/// {sentence}. And {sentence}.\npub struct Foo;\n");

        let diagnostics = check(&source);

        assert_eq!(diagnostics.len(), 2);
    }

    mod typescript {
        use stiles_typescript::TypeScriptLintPassAdapter;

        use super::*;

        fn check(source: &str) -> Vec<Diagnostic> {
            let tree = parse(source, Language::TypeScript);
            let mut passes: Vec<Box<dyn LintPass>> =
                vec![Box::new(TypeScriptLintPassAdapter::new(LongDocSentence))];

            execute(&tree, &mut passes)
        }

        #[test]
        fn doc_comment_at_the_limit_is_not_flagged() {
            let source = format!("/**\n * {}.\n */\nfunction f() {{}}", words(25));

            let diagnostics = check(&source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn doc_comment_over_the_limit_is_flagged_once() {
            let sentence = format!("{}.", words(26));
            let source = format!("/**\n * {sentence}\n */\nfunction f() {{}}");

            let diagnostics = check(&source);

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn doc_comment_over_the_limit_is_flagged_over_the_sentence() {
            let sentence = format!("{}.", words(26));
            let source = format!("/**\n * {sentence}\n */\nfunction f() {{}}");

            let diagnostics = check(&source);

            let start = source.find("word0").unwrap();
            assert_diagnostic(&diagnostics[0])
                .has_rule_id("lint.long-doc-sentence")
                .has_severity(Severity::Warn)
                .message_contains("26 words")
                .has_span("<test>", start, start + sentence.len());
        }

        #[test]
        fn example_section_is_not_counted() {
            let source = format!(
                "/**\n * Adds.\n * @example\n * {}\n */\nfunction f() {{}}",
                words(40)
            );

            let diagnostics = check(&source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn line_comment_over_the_limit_is_not_read() {
            let source = format!("// {}.\nconst a = 1;", words(40));

            let diagnostics = check(&source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn param_name_is_not_counted() {
            let source = format!(
                "/**\n * @param count - {}.\n */\nfunction f(count: number) {{}}",
                words(25)
            );

            let diagnostics = check(&source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn sentence_across_lines_is_counted_whole() {
            let source = format!(
                "/**\n * {}\n * {}.\n */\nfunction f() {{}}",
                words(13),
                words(13)
            );

            let diagnostics = check(&source);

            assert_diagnostic(&diagnostics[0]).message_contains("26 words");
        }

        #[test]
        fn sentence_across_lines_is_flagged_once() {
            let source = format!(
                "/**\n * {}\n * {}.\n */\nfunction f() {{}}",
                words(13),
                words(13)
            );

            let diagnostics = check(&source);

            assert_eq!(diagnostics.len(), 1);
        }
    }
}
