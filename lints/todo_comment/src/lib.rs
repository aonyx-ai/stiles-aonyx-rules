use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity, Span};
use whisker_typescript::TypeScriptLintPass;

const RULE_ID: RuleId = RuleId::new("lint.todo-comment");

/// The markers this rule looks for
///
/// No marker is a prefix of another, so at most one can match a line.
const MARKERS: &[&str] = &["TODO", "FIXME", "HACK", "XXX"];

/// Flags `TODO`, `FIXME`, `HACK`, and `XXX` markers in comments
///
/// A marker left in the source is invisible to anyone who does not open
/// that file, so the work is easy to lose. The issue tracker holds that
/// work instead.
///
/// The rule matches an uppercase marker at the start of a comment line and
/// nowhere else. A sentence or a URL that contains the word does not match.
/// It reads Rust and TypeScript comments alike, because a marker means the
/// same thing in either.
pub struct TodoComment;

/// A marker that a comment contains
///
/// The offset counts bytes from the start of the comment, so a caller adds
/// the comment's own start to reach a file offset.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
struct FoundMarker {
    offset: usize,
    marker: &'static str,
}

/// Returns whether `rest` starts at a word boundary
///
/// `TODOS` must not match, so `rest` must be empty or start with a
/// character that is not a letter, a digit, or an underscore.
fn is_word_break(rest: &str) -> bool {
    match rest.chars().next() {
        None => true,
        Some(character) => !character.is_alphanumeric() && character != '_',
    }
}

/// Returns each marker that starts a line of `text`, with its byte offset
///
/// The scan reads one line at a time. It removes the leading whitespace,
/// the comment punctuation (`/`, `*`, and `!`), and the whitespace again.
/// Then it checks whether what remains starts with a marker.
///
/// The punctuation trim stops at the first character that is not `/`, `*`,
/// or `!`. So `/// // TODO` keeps its inner `//` and does not match.
fn find_markers(text: &str) -> Vec<FoundMarker> {
    let mut found = Vec::new();
    let mut line_start = 0;

    for line in text.split_inclusive('\n') {
        let content = line.trim_start();
        let content = content.trim_start_matches(['/', '*', '!']);
        let content = content.trim_start();

        for marker in MARKERS.iter().copied() {
            let Some(rest) = content.strip_prefix(marker) else {
                continue;
            };
            if !is_word_break(rest) {
                continue;
            }

            found.push(FoundMarker {
                offset: line_start + line.len() - content.len(),
                marker,
            });
            break;
        }

        line_start += line.len();
    }

    found
}

/// Returns one diagnostic for each marker in a comment
///
/// Tree-sitter nests a `doc_comment` node inside the `line_comment` or
/// `block_comment` that carries it, and the walker visits both nodes. This
/// pass hooks only the outer two kinds, because hooking the child as well
/// would report every marker in a doc comment twice.
fn check_comment(node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let span = node.span();
    let mut diagnostics = Vec::new();

    for FoundMarker { offset, marker } in find_markers(node.text()) {
        let start = span.start() + offset;
        diagnostics.push(Diagnostic::new(
            RULE_ID,
            Severity::Warn,
            format!("`{marker}` comment: track this work in the issue tracker instead"),
            Span::new(span.file_path().clone(), start, start + marker.len()),
        ));
    }

    diagnostics
}

impl RustLintPass for TodoComment {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_comment(node)
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_comment(node)
    }
}

impl TypeScriptLintPass for TodoComment {
    fn check_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_comment(node)
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for TodoComment {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.todo-comment")]
    }
}

whisker_plugin::export_lints![rust: [TodoComment], typescript: [TodoComment]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(TodoComment))]
    }

    #[test]
    fn block_comment_with_marker_on_inner_line_is_flagged_once() {
        let source = "/*\n * FIXME: this breaks on empty input\n */\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn block_comment_with_marker_on_inner_line_is_flagged_with_the_marker_in_the_message() {
        let source = "/*\n * FIXME: this breaks on empty input\n */\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).message_contains("`FIXME`");
    }

    #[test]
    fn block_comment_with_mid_line_marker_is_not_flagged() {
        let source = "/* we removed the TODO already */\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn block_comment_with_two_markers_is_flagged_twice() {
        let source = "/*\n * TODO: one\n * HACK: two\n */\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn block_comment_with_two_markers_is_flagged_with_hack_in_the_second_message() {
        let source = "/*\n * TODO: one\n * HACK: two\n */\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[1]).message_contains("`HACK`");
    }

    #[test]
    fn block_comment_with_two_markers_is_flagged_with_todo_in_the_first_message() {
        let source = "/*\n * TODO: one\n * HACK: two\n */\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).message_contains("`TODO`");
    }

    #[test]
    fn doc_attribute_with_marker_is_not_flagged() {
        let source = "#[doc = \"TODO: document this\"]\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_comment_with_marker_is_flagged_at_the_marker() {
        let source = "/// TODO: document this\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 4, 8);
    }

    #[test]
    fn doc_comment_with_marker_is_flagged_once() {
        let source = "/// TODO: document this\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn find_markers_with_inner_comment_punctuation_returns_empty() {
        let markers = find_markers("/// // TODO: refactor this later");

        assert!(markers.is_empty());
    }

    #[test]
    fn find_markers_with_lowercase_marker_returns_empty() {
        let markers = find_markers("// todo: refactor this later");

        assert!(markers.is_empty());
    }

    #[test]
    fn find_markers_with_marker_after_text_returns_empty() {
        let markers = find_markers("// see the TODO in the tracker");

        assert!(markers.is_empty());
    }

    #[test]
    fn find_markers_with_marker_at_end_of_text_returns_marker() {
        let markers = find_markers("// XXX");

        assert_eq!(
            markers,
            vec![FoundMarker {
                offset: 3,
                marker: "XXX",
            }]
        );
    }

    #[test]
    fn find_markers_with_marker_prefix_of_word_returns_empty() {
        let markers = find_markers("// TODOS live in the tracker\n// XXXX\n// HACKY");

        assert!(markers.is_empty());
    }

    #[test]
    fn find_markers_with_no_delimiter_returns_marker() {
        let markers = find_markers("//TODO: fix");

        assert_eq!(
            markers,
            vec![FoundMarker {
                offset: 2,
                marker: "TODO",
            }]
        );
    }

    #[test]
    fn find_markers_with_repeated_punctuation_returns_marker() {
        let markers = find_markers("//// TODO: fix");

        assert_eq!(
            markers,
            vec![FoundMarker {
                offset: 5,
                marker: "TODO",
            }]
        );
    }

    #[test]
    fn find_markers_with_star_prefixed_line_returns_offset_of_marker() {
        let markers = find_markers("/*\n * TODO: fix\n */");

        assert_eq!(
            markers,
            vec![FoundMarker {
                offset: 6,
                marker: "TODO",
            }]
        );
    }

    #[test]
    fn hack_comment_is_flagged_once() {
        let source = "// HACK: works around a borrow checker limit\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn hack_comment_is_flagged_with_the_marker_in_the_message() {
        let source = "// HACK: works around a borrow checker limit\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).message_contains("`HACK`");
    }

    #[test]
    fn inner_doc_comment_with_marker_is_flagged_at_the_marker() {
        let source = "//! XXX: this module needs a rewrite\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 4, 7);
    }

    #[test]
    fn inner_doc_comment_with_marker_is_flagged_once() {
        let source = "//! XXX: this module needs a rewrite\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn marker_in_doc_example_is_not_flagged() {
        let source = "/// ```ignore\n/// // TODO: refactor this later\n/// ```\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn marker_in_prose_is_not_flagged() {
        let source =
            "/// Returns the TODO count for a user\nfn f() {}\n// we removed the FIXME already\n";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn marker_in_string_literal_is_not_flagged() {
        let source = "fn f() { let _ = \"// TODO: fix\"; let _ = \"FIXME\"; }";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn marker_in_url_is_not_flagged() {
        let source =
            "/// See https://example.com/issues/TODO\n// https://example.com/FIXME\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn marker_inside_function_body_is_flagged() {
        let source = "fn f() {\n    // TODO: handle the empty case\n}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn nested_block_comment_with_marker_is_flagged_at_the_inner_marker() {
        let source = "/* outer\n/* TODO: inner */\n*/\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 12, 16);
    }

    #[test]
    fn nested_block_comment_with_marker_is_flagged_once() {
        let source = "/* outer\n/* TODO: inner */\n*/\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn outer_block_doc_comment_with_marker_is_flagged_at_the_marker() {
        let source = "/** TODO: document this */\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 4, 8);
    }

    #[test]
    fn outer_block_doc_comment_with_marker_is_flagged_once() {
        let source = "/** TODO: document this */\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn plain_comment_is_not_flagged() {
        let source = "// this function adds two numbers\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn todo_comment_is_flagged_once() {
        let source = "// TODO: refactor this later\nfn helper() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn todo_comment_is_flagged_with_rule_id_severity_message_and_span() {
        let source = "// TODO: refactor this later\nfn helper() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.todo-comment")
            .has_severity(Severity::Warn)
            .message_contains("issue tracker")
            .has_span("<test>", 3, 7);
    }

    #[test]
    fn todo_macro_is_not_flagged() {
        let source = "fn f() { todo!() }";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn todo_with_author_is_flagged_at_the_marker() {
        let source = "// TODO(marts): finish this\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 3, 7);
    }

    #[test]
    fn todo_with_author_is_flagged_once() {
        let source = "// TODO(marts): finish this\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<TodoComment>();
    }

    #[test]
    fn trait_send_found_marker() {
        fn assert_send<T: Send>() {}
        assert_send::<FoundMarker>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<TodoComment>();
    }

    #[test]
    fn trait_sync_found_marker() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FoundMarker>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<TodoComment>();
    }

    #[test]
    fn trait_unpin_found_marker() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<FoundMarker>();
    }

    #[test]
    fn two_line_comments_with_markers_are_flagged_at_the_fixme_second() {
        let source = "// TODO: one\n// FIXME: two\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[1]).has_span("<test>", 16, 21);
    }

    #[test]
    fn two_line_comments_with_markers_are_flagged_at_the_todo_first() {
        let source = "// TODO: one\n// FIXME: two\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 3, 7);
    }

    #[test]
    fn two_line_comments_with_markers_are_flagged_twice() {
        let source = "// TODO: one\n// FIXME: two\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn xxx_comment_is_flagged_once() {
        let source = "// XXX\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn xxx_comment_is_flagged_with_the_marker_in_the_message() {
        let source = "// XXX\nfn f() {}";

        let diagnostics = execute(&parse(source, Language::Rust), &mut passes());

        assert_diagnostic(&diagnostics[0]).message_contains("`XXX`");
    }

    mod typescript {
        use whisker_typescript::TypeScriptLintPassAdapter;

        use super::*;

        fn passes() -> Vec<Box<dyn LintPass>> {
            vec![Box::new(TypeScriptLintPassAdapter::new(TodoComment))]
        }

        #[test]
        fn block_comment_with_marker_on_inner_line_is_flagged_once() {
            let source = "/*\n * FIXME: this breaks on empty input\n */\nlet a = 1;";

            let diagnostics = execute(&parse(source, Language::TypeScript), &mut passes());

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn block_comment_with_marker_on_inner_line_is_flagged_with_the_marker_in_the_message() {
            let source = "/*\n * FIXME: this breaks on empty input\n */\nlet a = 1;";

            let diagnostics = execute(&parse(source, Language::TypeScript), &mut passes());

            assert_diagnostic(&diagnostics[0]).message_contains("`FIXME`");
        }

        #[test]
        fn doc_comment_with_marker_is_flagged_at_the_marker() {
            let source = "/**\n * TODO: describe the return value\n */\nfunction f() {}";
            let start = source.find("TODO").unwrap();

            let diagnostics = execute(&parse(source, Language::TypeScript), &mut passes());

            assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + 4);
        }

        #[test]
        fn doc_comment_with_marker_is_flagged_once() {
            let source = "/**\n * TODO: describe the return value\n */\nfunction f() {}";

            let diagnostics = execute(&parse(source, Language::TypeScript), &mut passes());

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn line_comment_with_marker_is_flagged_once() {
            let source = "// HACK: work around the parser\nconst a = 1;";

            let diagnostics = execute(&parse(source, Language::TypeScript), &mut passes());

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn line_comment_with_marker_is_flagged_with_rule_id_severity_and_marker() {
            let source = "// HACK: work around the parser\nconst a = 1;";

            let diagnostics = execute(&parse(source, Language::TypeScript), &mut passes());

            assert_diagnostic(&diagnostics[0])
                .has_rule_id("lint.todo-comment")
                .has_severity(Severity::Warn)
                .message_contains("`HACK`");
        }

        #[test]
        fn line_comment_with_mid_line_marker_is_not_flagged() {
            let source = "// the TODO list lives in the tracker\nconst a = 1;";

            let diagnostics = execute(&parse(source, Language::TypeScript), &mut passes());

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn string_with_marker_is_not_flagged() {
            let source = "const label = \"TODO: not a comment\";";

            let diagnostics = execute(&parse(source, Language::TypeScript), &mut passes());

            assert_no_diagnostics(&diagnostics);
        }
    }
}
