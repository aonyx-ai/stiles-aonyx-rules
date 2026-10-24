mod doc_line;
mod sentence;

use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity, Span};
use whisker_typescript::TypeScriptLintPass;

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
        if doc_marker(node.raw()).is_none() {
            return Vec::new();
        }

        let mut lines = Vec::new();
        collect(&mut lines, node);
        report(&lines, node)
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(marker) = doc_marker(node.raw()) else {
            return Vec::new();
        };
        if continues_block(node.raw(), marker) {
            return Vec::new();
        }
        let Some(parent) = node.parent() else {
            return Vec::new();
        };

        let siblings = parent.named_children();
        let Some(index) = siblings
            .iter()
            .position(|sibling| sibling.id() == node.id())
        else {
            return Vec::new();
        };

        let mut lines = Vec::new();
        let rows = node.raw().start_position().row..;
        for (row, sibling) in rows.zip(&siblings[index..]) {
            if sibling.kind() != "line_comment" || doc_marker(sibling.raw()) != Some(marker) {
                break;
            }
            if sibling.raw().start_position().row != row {
                break;
            }
            collect(&mut lines, sibling);
        }

        report(&lines, node)
    }
}

/// The marker that opens a doc comment
///
/// A `///` run and a `//!` run document different items, so the rule reads
/// them as separate comments even when they touch.
impl TypeScriptLintPass for LongDocSentence {
    fn check_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(lines) = jsdoc::prose_lines(node.text(), node.span().start()) else {
            return Vec::new();
        };

        let lines: Vec<DocLine<'_>> = lines
            .into_iter()
            .map(|line| DocLine::new(line.text(), line.offset()))
            .collect();
        report(&lines, node)
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum Marker {
    Inner,
    Outer,
}

/// Returns the marker of a comment, or [`None`] when it carries no doc text
///
/// [`None`]: std::option::Option::None
fn doc_marker(node: tree_sitter::Node<'_>) -> Option<Marker> {
    node.child_by_field_name("doc")?;

    match node.child_by_field_name("inner") {
        Some(_) => Some(Marker::Inner),
        None => node.child_by_field_name("outer").map(|_| Marker::Outer),
    }
}

/// Returns whether the line continues a doc comment that started above it
///
/// Rustdoc joins a run of `///` lines into one comment. A sentence may cross
/// the line breaks, so only the first line of the run reports.
fn continues_block(node: tree_sitter::Node<'_>, marker: Marker) -> bool {
    let Some(previous) = node.prev_named_sibling() else {
        return false;
    };
    if previous.kind() != "line_comment" {
        return false;
    }
    if previous.start_position().row + 1 != node.start_position().row {
        return false;
    }

    doc_marker(previous) == Some(marker)
}

/// Appends every line of one comment to `lines`
///
/// The `doc` field holds the text after the marker, so its offsets already
/// skip the marker. A block comment keeps its newlines in that one field, so
/// this function splits the text rather than treating the node as one line.
fn collect<'a>(lines: &mut Vec<DocLine<'a>>, node: &DecoratedNode<'a>) {
    let Some(doc) = node.child_by_field_name("doc") else {
        return;
    };

    let offset = doc.raw().start_byte();
    let text = doc.text();
    let text = text.strip_suffix('\n').unwrap_or(text);

    let mut cursor = 0;
    for line in text.split('\n') {
        lines.push(DocLine::new(
            line.strip_suffix('\r').unwrap_or(line),
            offset + cursor,
        ));
        cursor += line.len() + 1;
    }
}

/// Reports every sentence in `lines` that goes over the word cap
fn report(lines: &[DocLine<'_>], node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let file = node.span();

    sentences(lines)
        .into_iter()
        .filter(|sentence| sentence.words() > MAX_WORDS)
        .map(|sentence| {
            Diagnostic::new(
                RuleId::new("lint.long-doc-sentence"),
                Severity::Warn,
                format!(
                    "doc sentence is {} words, over the {MAX_WORDS}-word limit",
                    sentence.words()
                ),
                Span::new(file.file_path().clone(), sentence.start(), sentence.end()),
            )
        })
        .collect()
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for LongDocSentence {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.long-doc-sentence")]
    }
}

whisker_plugin::export_lints![rust: [LongDocSentence], typescript: [LongDocSentence]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

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
    fn long_sentence_across_lines_is_flagged() {
        let source = "/// Returns the coverage verdict for this file, which is empty when no decoration\n/// providers were configured, since a provider that declines still counts as\n/// having looked and the walk needs to distinguish that from never having run.\npub struct Foo;\n";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.long-doc-sentence")
            .has_severity(Severity::Warn)
            .message_contains("37 words");
        let span = diagnostics[0].span();
        assert!(source[span.start()..span.end()].starts_with("Returns the coverage"));
        assert!(source[span.start()..span.end()].ends_with("having run."));
    }

    #[test]
    fn multibyte_prose_reports_a_span_on_a_character_boundary() {
        let source = format!("/// 日本語 {}.\npub struct Foo;\n", words(30));

        let diagnostics = check(&source);

        assert_eq!(diagnostics.len(), 1);
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
    fn sentence_over_the_limit_is_flagged() {
        let diagnostics = check(&doc_of(MAX_WORDS + 1));

        assert_eq!(diagnostics.len(), 1);
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

        assert_eq!(diagnostics.len(), 1);
        let span = diagnostics[0].span();
        let sentence = source[4..].lines().next().expect("the doc line");
        assert_eq!(&source[span.start()..span.end()], sentence);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<LongDocSentence>();
        assert_send::<Marker>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<LongDocSentence>();
        assert_sync::<Marker>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<LongDocSentence>();
        assert_unpin::<Marker>();
    }

    #[test]
    fn two_long_sentences_are_flagged_once_each() {
        let sentence = words(30);
        let source = format!("/// {sentence}. And {sentence}.\npub struct Foo;\n");

        let diagnostics = check(&source);

        assert_eq!(diagnostics.len(), 2);
    }

    mod typescript {
        use whisker_typescript::TypeScriptLintPassAdapter;

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
        fn doc_comment_over_the_limit_is_flagged_over_the_sentence() {
            let sentence = format!("{}.", words(26));
            let source = format!("/**\n * {sentence}\n */\nfunction f() {{}}");

            let diagnostics = check(&source);

            assert_eq!(diagnostics.len(), 1);
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

            assert_eq!(diagnostics.len(), 1);
            assert_diagnostic(&diagnostics[0]).message_contains("26 words");
        }
    }
}
