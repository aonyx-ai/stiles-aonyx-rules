use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity};

const RULE_ID: RuleId = RuleId::new("lint.unsorted-test");

/// The last path segments that mark a function as a test
///
/// `test` covers `#[test]` and every runtime's own form of it, such as
/// `#[tokio::test]`. `rstest` and `test_case` each turn one function into
/// one or more tests, and the function is still written among the others.
const TEST_ATTRIBUTES: &[&str] = &["rstest", "test", "test_case"];

/// Flags a test function whose name sorts before the test above it
///
/// The convention orders the test functions of a module alphabetically.
/// The rule reads the tests of one module in source order and compares each
/// one with the test before it. A helper or a fixture between two tests
/// does not take part, and the tests of a nested module form their own list.
///
/// Names compare byte by byte, as `str` does and as `LC_ALL=C sort` does.
/// That is also the order in which the test runner lists them. So `a_b`
/// sorts before `ab`, and `case_10` sorts before `case_2`. A raw identifier
/// sorts by the name without its `r#`, which is the name the runner shows.
///
/// A test is a function with an attribute whose last path segment is
/// `test`, `rstest`, or `test_case`. A test inside a macro call such as
/// `proptest!` is not read. Its order is in tokens that the syntax tree does
/// not show as items.
///
/// # Examples
///
/// ```ignore
/// #[test]
/// fn parse_with_valid_input_returns_value() {}
///
/// #[test]
/// fn parse_with_empty_input_returns_error() {}
/// ```
///
/// The rule reports `parse_with_empty_input_returns_error` and asks to move
/// it before `parse_with_valid_input_returns_value`.
pub struct UnsortedTest;

impl RustLintPass for UnsortedTest {
    fn check_mod_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        match node.child_by_field_name("body") {
            Some(body) => check_scope(&body),
            None => Vec::new(),
        }
    }

    fn check_source_file(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_scope(node)
    }
}

/// Reports each test in one module that sorts before the test above it
///
/// The message names the first test above it whose name sorts after its
/// own. Among the tests above it, that is where the test belongs.
fn check_scope(scope: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let names = test_names(scope);
    let mut diagnostics = Vec::new();

    for (position, name) in names.iter().enumerate().skip(1) {
        let key = sort_key(name.text());
        let previous = sort_key(names[position - 1].text());
        if key >= previous {
            continue;
        }
        let target = names[..position]
            .iter()
            .map(|earlier| sort_key(earlier.text()))
            .find(|earlier| *earlier > key)
            .unwrap_or(previous);

        diagnostics.push(
            Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Error)
                .message(format!(
                    "test `{key}` is out of alphabetical order; move it before `{target}`"
                ))
                .span(name.span())
                .build(),
        );
    }

    diagnostics
}

/// Returns the name node of each test function that is a direct item of
/// the scope, in source order
///
/// Tree-sitter makes each attribute a sibling of the item it decorates. The
/// walk notes a test attribute in front of an item and skips comments. Any
/// item clears the note.
fn test_names<'a>(scope: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let mut names = Vec::new();
    let mut marked = false;

    for child in scope.named_children() {
        match child.kind() {
            "attribute_item" => {
                marked |= child
                    .named_child(0)
                    .is_some_and(|attribute| is_test_attribute(&attribute));
            }
            "line_comment" | "block_comment" => {}
            kind => {
                if marked && kind == "function_item" {
                    names.extend(child.child_by_field_name("name"));
                }
                marked = false;
            }
        }
    }

    names
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
    name.is_some_and(|name| TEST_ATTRIBUTES.contains(&name.text()))
}

/// Returns the name a test sorts by, which drops the `r#` of a raw
/// identifier
fn sort_key(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for UnsortedTest {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

stiles_plugin::export_lints![rust: [UnsortedTest]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, Severity};

    use super::*;

    fn check(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(UnsortedTest))];

        execute(&tree, &mut passes)
    }

    /// Returns the name each diagnostic points at, in order
    fn flagged(source: &str) -> Vec<String> {
        check(source)
            .iter()
            .map(|diagnostic| {
                let span = diagnostic.span();
                source[span.start()..span.end()].to_owned()
            })
            .collect()
    }

    #[test]
    fn attribute_named_like_a_test_does_not_mark_the_function() {
        let source = "#[test]\nfn b() {}\n#[testing]\nfn a() {}\n#[test_log]\nfn a2() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn attribute_on_another_item_does_not_mark_the_function() {
        let source = "#[test]\nfn b() {}\n#[test]\nstruct S;\nfn a() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn cfg_attr_test_does_not_mark_the_function() {
        let source = "#[test]\nfn b() {}\n#[cfg_attr(test, test)]\nfn a() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn comment_between_attribute_and_function_keeps_the_mark() {
        let source = "#[test]\nfn b() {}\n#[test]\n// note\n/* more */\nfn a() {}";

        assert_eq!(flagged(source), ["a"]);
    }

    #[test]
    fn digits_sort_by_byte_not_by_value() {
        let source = "#[test]\nfn case_10() {}\n#[test]\nfn case_2() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn digits_sorted_by_value_are_flagged() {
        let source = "#[test]\nfn case_2() {}\n#[test]\nfn case_10() {}";

        assert_eq!(flagged(source), ["case_10"]);
    }

    #[test]
    fn doc_comment_and_extra_attributes_keep_the_mark() {
        let source = "#[test]\nfn b() {}\n/// Docs\n#[ignore]\n#[test]\n#[should_panic]\nfn a() {}";

        assert_eq!(flagged(source), ["a"]);
    }

    #[test]
    fn duplicate_names_are_not_flagged() {
        let source = "#[cfg(unix)]\n#[test]\nfn a() {}\n#[cfg(windows)]\n#[test]\nfn a() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn each_test_compares_with_the_one_above_it_only() {
        let source =
            "#[test]\nfn d() {}\n#[test]\nfn b() {}\n#[test]\nfn c() {}\n#[test]\nfn a() {}";

        assert_eq!(flagged(source), ["b", "a"]);
    }

    #[test]
    fn file_scope_tests_are_compared() {
        let source = "#[test]\nfn b() {}\n#[test]\nfn a() {}";

        assert_eq!(flagged(source), ["a"]);
    }

    #[test]
    fn function_inside_a_function_body_is_not_read() {
        let source = "#[test]\nfn b() {\n    #[test]\n    fn a() {}\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn helper_between_tests_is_not_compared() {
        let source = "#[test]\nfn b() {}\nfn a_helper() {}\n#[test]\nfn c() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn helper_between_unsorted_tests_does_not_hide_them() {
        let source = "#[test]\nfn c() {}\nfn z_helper() {}\nconst X: u8 = 0;\n#[test]\nfn b() {}";

        assert_eq!(flagged(source), ["b"]);
    }

    #[test]
    fn message_names_the_first_test_above_that_sorts_after() {
        let source =
            "#[test]\nfn a() {}\n#[test]\nfn c() {}\n#[test]\nfn d() {}\n#[test]\nfn b() {}";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("test `b` is out of alphabetical order; move it before `c`");
    }

    #[test]
    fn module_body_test_is_flagged_with_rule_id_severity_and_span() {
        let source = "mod tests {\n    #[test]\n    fn b() {}\n\n    #[test]\n    fn a() {}\n}";
        let start = source.rfind("a()").unwrap();

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.unsorted-test")
            .has_severity(Severity::Error)
            .has_span("<test>", start, start + 1);
    }

    #[test]
    fn module_body_tests_are_compared() {
        let source = "mod tests {\n    #[test]\n    fn b() {}\n\n    #[test]\n    fn a() {}\n}";

        assert_eq!(flagged(source), ["a"]);
    }

    #[test]
    fn module_declaration_without_body_is_not_read() {
        let source = "#[test]\nfn a() {}\nmod tests;\n#[test]\nfn b() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn nested_module_tests_form_their_own_list() {
        let source = "mod tests {\n    #[test]\n    fn m() {}\n    mod inner {\n        #[test]\n        fn a() {}\n        #[test]\n        fn z() {}\n    }\n    #[test]\n    fn n() {}\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn nested_module_with_unsorted_tests_is_flagged_once() {
        let source = "mod tests {\n    mod inner {\n        #[test]\n        fn z() {}\n        #[test]\n        fn a() {}\n    }\n}";

        assert_eq!(flagged(source), ["a"]);
    }

    #[test]
    fn prefix_after_the_longer_name_is_flagged() {
        let source = "#[test]\nfn parse_empty() {}\n#[test]\nfn parse() {}";

        assert_eq!(flagged(source), ["parse"]);
    }

    #[test]
    fn prefix_sorts_before_the_longer_name() {
        let source = "#[test]\nfn parse() {}\n#[test]\nfn parse_empty() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn proptest_macro_body_is_not_read() {
        let source = "proptest! {\n    #[test]\n    fn b(x in 0..1u8) {}\n\n    #[test]\n    fn a(x in 0..1u8) {}\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn raw_identifier_out_of_order_is_named_without_its_prefix() {
        let source = "#[test]\nfn parse() {}\n#[test]\nfn r#match() {}";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("test `match` is out of alphabetical order; move it before `parse`");
    }

    #[test]
    fn raw_identifier_sorts_without_its_prefix() {
        let source = "#[test]\nfn r#match() {}\n#[test]\nfn parse() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn rstest_attribute_marks_a_test() {
        let source = "#[test]\nfn b() {}\n#[rstest]\n#[case(1)]\nfn a(#[case] x: u8) {}";

        assert_eq!(flagged(source), ["a"]);
    }

    #[test]
    fn scoped_test_attribute_marks_a_test() {
        let source =
            "#[test]\nfn b() {}\n#[tokio::test(flavor = \"multi_thread\")]\nasync fn a() {}";

        assert_eq!(flagged(source), ["a"]);
    }

    #[test]
    fn sorted_tests_are_not_flagged() {
        let source = "#[test]\nfn alpha() {}\n#[test]\nfn beta() {}\n#[test]\nfn gamma() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn test_case_attribute_marks_a_test() {
        let source = "#[test]\nfn b() {}\n#[test_case(1 ; \"one\")]\n#[test_case(2 ; \"two\")]\nfn a(x: u8) {}";

        assert_eq!(flagged(source), ["a"]);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<UnsortedTest>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<UnsortedTest>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<UnsortedTest>();
    }

    #[test]
    fn underscore_after_a_letter_is_flagged() {
        let source = "#[test]\nfn ab() {}\n#[test]\nfn a_b() {}";

        assert_eq!(flagged(source), ["a_b"]);
    }

    #[test]
    fn underscore_sorts_before_a_letter() {
        let source = "#[test]\nfn trait_send() {}\n#[test]\nfn trait_send_marker() {}\n#[test]\nfn trait_sync() {}\n#[test]\nfn traits() {}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn uppercase_sorts_before_lowercase() {
        let source = "#[test]\nfn Zeta() {}\n#[test]\nfn alpha() {}";

        assert_no_diagnostics(&check(source));
    }
}
