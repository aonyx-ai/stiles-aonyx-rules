use whisker_rust::{RuleId, RustLintPass};
use whisker_types::{DecoratedNode, Diagnostic, Severity};

const RULE_ID: RuleId = RuleId::new("lint.unseparated-test-body");

/// The fewest statements a body needs before the rule reports it
///
/// A body of two statements is an Act and an Assert, and it reads well
/// without a blank line between them.
const MIN_STATEMENTS: usize = 3;

/// Flags a test body of three or more statements without a blank line
///
/// Whisker's conventions write a test as three phases, Arrange, Act, and
/// Assert, with a blank line between each phase. A body without any blank
/// line hides where one phase ends and the next starts.
///
/// The rule cannot know which statement is the Act phase, so it does not
/// check where the blank lines are. It reports only a body that has none at
/// all. A test is a function with an attribute whose last path segment is
/// `test`, such as `#[test]` or `#[tokio::test]`. The tail expression counts
/// as a statement, and a comment or an attribute does not. A line that holds
/// only a comment is not blank. A test inside a macro body, such as
/// `proptest!`, is a token tree to the parser, so the rule never sees it.
///
/// # Examples
///
/// ```ignore
/// #[test]
/// fn parse_with_valid_input_returns_value() {
///     let input = "42";
///     let result = parse(input).expect("should succeed");
///     assert_eq!(result, 42);
/// }
/// ```
///
/// The rule reports `parse_with_valid_input_returns_value`. A blank line
/// after the Arrange phase and one after the Act phase silence it.
pub struct UnseparatedTestBody;

impl RustLintPass for UnseparatedTestBody {
    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report(node)
    }
}

/// Reports a test function whose body has three or more statements and no
/// blank line between any two of them
fn report(node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    if !outer_attributes(node).iter().any(is_test_attribute) {
        return Vec::new();
    }
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let Some(name) = node.child_by_field_name("name") else {
        return Vec::new();
    };

    let children = body.named_children();
    let statements: Vec<usize> = children
        .iter()
        .enumerate()
        .filter(|(_, child)| is_statement(child))
        .map(|(index, _)| index)
        .collect();
    let (Some(&first), Some(&last)) = (statements.first(), statements.last()) else {
        return Vec::new();
    };
    if statements.len() < MIN_STATEMENTS {
        return Vec::new();
    }

    let separated = children[first..=last]
        .windows(2)
        .any(|pair| has_blank_line_between(&body, &pair[0], &pair[1]));
    if separated {
        return Vec::new();
    }

    vec![Diagnostic::new(
        RULE_ID,
        Severity::Warn,
        format!(
            "test body of {} statements has no blank line: separate Arrange, Act, and Assert with blank lines",
            statements.len()
        ),
        name.span(),
    )]
}

/// Returns whether a named child of a block is a statement
///
/// A comment, a stray `;`, and an attribute are not. An outer attribute
/// belongs to the statement after it, and an inner attribute belongs to the
/// block.
fn is_statement(child: &DecoratedNode<'_>) -> bool {
    match child.kind() {
        "line_comment"
        | "block_comment"
        | "empty_statement"
        | "attribute_item"
        | "inner_attribute_item" => false,
        _ => true,
    }
}

/// Returns whether a line between two adjacent children of a block holds
/// only whitespace
///
/// The text between the two children starts on the line where `before`
/// ends. So its first piece is the rest of that line, not a whole line. A
/// line comment can end with its own newline, and then the first piece is
/// a whole line. The last piece is the start of the line that `after` opens,
/// so it never counts.
fn has_blank_line_between(
    block: &DecoratedNode<'_>,
    before: &DecoratedNode<'_>,
    after: &DecoratedNode<'_>,
) -> bool {
    let origin = block.span().start();
    let Some(gap) = block
        .text()
        .get(before.span().end() - origin..after.span().start() - origin)
    else {
        return false;
    };

    let pieces: Vec<&str> = gap.split('\n').collect();
    let skip = match before.text().ends_with('\n') {
        true => 0,
        false => 1,
    };
    let Some(whole_lines) = pieces.get(skip..pieces.len() - 1) else {
        return false;
    };
    whole_lines.iter().any(|line| line.trim().is_empty())
}

/// Returns the `attribute` nodes written in front of the node
///
/// Tree-sitter makes each attribute a sibling of the item it decorates. The
/// walk reads backward from the node over attributes and comments. It stops
/// at the first sibling that is neither, so an attribute on the item before
/// this one never reaches it.
fn outer_attributes<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    let siblings = parent.named_children();
    let Some(position) = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())
    else {
        return Vec::new();
    };

    let mut attributes = Vec::new();
    for sibling in siblings[..position].iter().rev() {
        match sibling.kind() {
            "attribute_item" => attributes.extend(sibling.named_child(0)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    attributes
}

/// Returns whether the attribute marks a test, such as `#[tokio::test]`
///
/// Only the last segment of the path counts, because `#[tokio::test]` and
/// `#[test]` after a `use tokio::test` name one macro.
fn is_test_attribute(attribute: &DecoratedNode<'_>) -> bool {
    let Some(path) = attribute.named_child(0) else {
        return false;
    };
    let name = match path.kind() {
        "scoped_identifier" => path.child_by_field_name("name"),
        _ => Some(path),
    };
    name.is_some_and(|name| name.text() == "test")
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for UnseparatedTestBody {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

whisker_plugin::export_lints![rust: [UnseparatedTestBody]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(UnseparatedTestBody))]
    }

    fn check(source: &str) -> Vec<Diagnostic> {
        execute(&parse(source, Language::Rust), &mut passes())
    }

    #[test]
    fn async_test_is_flagged() {
        let source = "#[tokio::test]\nasync fn t() {\n    let a = 1;\n    let b = f(a).await;\n    assert_eq!(b, 2);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn attribute_with_arguments_is_a_test() {
        let source = "#[tokio::test(flavor = \"multi_thread\")]\nasync fn t() {\n    let a = 1;\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn attribute_with_other_last_segment_is_not_a_test() {
        let source = "#[test_case(1)]\n#[rstest]\n#[test::other]\nfn t() {\n    let a = 1;\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn blank_line_after_attribute_inside_body_separates() {
        let source = "#[test]\nfn t() {\n    let a = 1;\n    #[allow(unused)]\n\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn blank_line_after_comment_separates() {
        let source = "#[test]\nfn t() {\n    let a = 1;\n    // act\n\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn blank_line_after_last_statement_does_not_separate() {
        let source = "#[test]\nfn t() {\n    let a = 1;\n    let b = a;\n    assert_eq!(a, b);\n\n    // done\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn blank_line_before_first_statement_does_not_separate() {
        let source = "#[test]\nfn t() {\n    #![allow(unused)]\n\n    let a = 1;\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn blank_line_between_comments_separates() {
        let source = "#[test]\nfn t() {\n    let a = 1;\n    // one\n\n    // two\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn blank_line_inside_block_comment_does_not_separate() {
        let source = "#[test]\nfn t() {\n    let a = 1;\n    /* one\n\n    two */\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn blank_line_inside_statement_does_not_separate() {
        let source = "#[test]\nfn t() {\n    let a = vec![\n        1,\n\n        2,\n    ];\n    let b = a.len();\n    assert_eq!(b, 2);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn blank_line_with_spaces_separates() {
        let source =
            "#[test]\nfn t() {\n    let a = 1;\n    \t \n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn body_with_crlf_line_endings_and_blank_line_is_not_flagged() {
        let source = "#[test]\r\nfn t() {\r\n    let a = 1;\r\n\r\n    let b = a;\r\n    assert_eq!(a, b);\r\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn body_with_one_blank_line_is_not_flagged() {
        let source = "#[test]\nfn t() {\n    let a = 1;\n    let b = a;\n\n    assert_eq!(a, b);\n    assert!(b > 0);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn body_with_statements_on_one_line_is_flagged() {
        let source = "#[test]\nfn t() { let a = 1; let b = a; assert_eq!(a, b); }";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn comment_line_between_statements_does_not_separate() {
        let source = "#[test]\nfn t() {\n    let a = 1;\n    // act\n    let b = a;\n    /* assert */\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn comments_and_attributes_are_not_counted() {
        let source = "#[test]\nfn t() {\n    // arrange\n    let a = 1;\n    #[allow(unused)]\n    /* assert */\n    assert_eq!(a, 1);\n    ;\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_comment_between_statements_does_not_separate() {
        let source = "#[test]\nfn t() {\n    let a = 1;\n    /// helper\n    fn h() {}\n    assert_eq!(a, 1);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn empty_test_is_not_flagged() {
        let source = "#[test]\nfn t() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn function_without_test_attribute_is_not_flagged() {
        let source =
            "#[inline]\nfn helper() {\n    let a = 1;\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn helper_inside_test_module_is_not_flagged() {
        let source = "#[cfg(test)]\nmod tests {\n    fn helper() {\n        let a = 1;\n        let b = a;\n        assert_eq!(a, b);\n    }\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn items_count_as_statements() {
        let source = "#[test]\nfn t() {\n    fn assert_send<T: Send>() {}\n    struct S;\n    assert_send::<S>();\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn line_comment_between_test_attribute_and_function_is_skipped() {
        let source = "#[test]\n// note\n#[ignore]\nfn t() {\n    let a = 1;\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn nested_function_inside_test_is_not_a_test() {
        let source = "#[test]\nfn t() {\n    fn helper() {\n        let a = 1;\n        let b = a;\n        assert_eq!(a, b);\n    }\n\n    helper();\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn proptest_macro_body_is_not_flagged() {
        let source = "proptest! {\n    #[test]\n    fn t(a in 0..10u8) {\n        let b = a;\n        let c = b;\n        prop_assert_eq!(a, c);\n    }\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn tail_expression_counts_as_statement() {
        let source =
            "#[test]\nfn t() -> Result<(), E> {\n    let a = 1;\n    check(a)?;\n    Ok(())\n}";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).message_contains("3 statements");
    }

    #[test]
    fn test_attribute_on_previous_item_does_not_reach_function() {
        let source = "#[test]\nfn first() {}\nfn second() {\n    let a = 1;\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn test_with_three_statements_and_no_blank_line_is_flagged_once() {
        let source = "#[test]\nfn parse_works() {\n    let input = \"42\";\n    let result = parse(input);\n    assert_eq!(result, 42);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn test_with_two_statements_is_not_flagged() {
        let source =
            "#[test]\nfn t() {\n    let result = parse(\"42\");\n    assert_eq!(result, 42);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn test_written_as_in_the_conventions_is_not_flagged() {
        let source = "#[test]\nfn parse_works() {\n    let input = \"42\";\n\n    let result = parse(input).expect(\"should succeed\");\n\n    assert_eq!(result, 42);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trailing_comment_before_blank_line_separates() {
        let source = "#[test]\nfn t() {\n    let a = 1; // arrange\n\n    let b = a;\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trailing_comments_without_blank_line_do_not_separate() {
        let source = "#[test]\nfn t() {\n    let a = 1; // arrange\n    let b = a; // act\n    assert_eq!(a, b);\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<UnseparatedTestBody>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<UnseparatedTestBody>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<UnseparatedTestBody>();
    }

    #[test]
    fn unseparated_body_diagnostic_has_rule_id_severity_message_and_span() {
        let source = "#[test]\nfn parse_works() {\n    let input = \"42\";\n    let result = parse(input);\n    assert_eq!(result, 42);\n}";
        let start = source.find("parse_works").unwrap();

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.unseparated-test-body")
            .has_severity(Severity::Warn)
            .message_contains("test body of 3 statements has no blank line")
            .has_span("<test>", start, start + "parse_works".len());
    }
}
