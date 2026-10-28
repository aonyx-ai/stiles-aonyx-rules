use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity};

const RULE_ID: RuleId = RuleId::new("lint.multi-assert-test");

/// Flags a test function whose body holds more than one assertion
///
/// A test checks one fact. A test that asserts on a `Result` and then on
/// its contents checks two, and its failure does not say which one broke.
/// One test per assertion names each fact, and each fact fails on its own.
///
/// A test is a function with an attribute whose last path segment is
/// `test`, so `#[test]` and `#[tokio::test]` count alike.
///
/// The rule counts call sites, not calls. An assertion inside a loop or a
/// closure is one call site. An assertion is a macro or a function whose
/// name is `assert` or `debug_assert`, or starts with `assert_` or
/// `debug_assert_`. Only the last segment of the path counts, so
/// `std::assert_eq!`, `assert_matches!`, and a helper such as
/// `stiles_testing::assert_diagnostic` count alike. A method call never
/// counts, because the `.assert()` of `assert_cmd` runs the command rather
/// than checking it.
///
/// A function, an `impl`, or another item inside the test body is not part
/// of the test, so the rule does not read it. The rule cannot see into a
/// macro argument or a macro body either. A test that `proptest!`
/// generates is therefore never read.
///
/// The rule reports each test once, at its second assertion, and names the
/// number of assertions in the body.
///
/// # Examples
///
/// ```ignore
/// #[test]
/// fn parse_with_valid_input_returns_value() {
///     let result = parse("42");
///
///     assert!(result.is_ok());
///     assert_eq!(result.unwrap(), 42);
/// }
/// ```
///
/// The rule reports the `assert_eq!`. Taking the value with `.expect()`
/// in the Act phase leaves one assertion.
pub struct MultiAssertTest;

impl RustLintPass for MultiAssertTest {
    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_function(node).into_iter().collect()
    }
}

/// Returns a diagnostic when `function` is a test with two or more
/// assertions
fn check_function(function: &DecoratedNode<'_>) -> Option<Diagnostic> {
    if !is_test(function) {
        return None;
    }
    let name = function.child_by_field_name("name")?;
    let body = function.child_by_field_name("body")?;

    let mut assertions = Vec::new();
    collect_assertions(&body, &mut assertions);
    let second = assertions.get(1)?;

    Some(
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Warn)
            .message(format!(
                "test `{}` makes {} assertions; split it into one test per assertion",
                name.text(),
                assertions.len(),
            ))
            .span(second.span())
            .build(),
    )
}

/// Returns whether an attribute in front of `function` marks it as a test
///
/// Tree-sitter leaves an item's attributes beside it rather than inside it.
/// The walk goes backwards over the siblings and stops at the first one
/// that is neither an attribute nor a comment.
fn is_test(function: &DecoratedNode<'_>) -> bool {
    let Some(parent) = function.parent() else {
        return false;
    };
    let siblings = parent.named_children();
    let Some(position) = siblings
        .iter()
        .position(|sibling| sibling.id() == function.id())
    else {
        return false;
    };

    for sibling in siblings[..position].iter().rev() {
        match sibling.kind() {
            "attribute_item" => {
                let marks_test = sibling
                    .named_child(0)
                    .and_then(|attribute| attribute.named_child(0))
                    .is_some_and(|path| last_segment(&path) == Some("test"));
                if marks_test {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
    }
    false
}

/// Pushes the callee of each assertion under `node`, in source order
///
/// The walk does not enter a token tree, which holds the argument of a macro.
/// It does not enter an item either, because an item is not part of the body.
fn collect_assertions<'a>(node: &DecoratedNode<'a>, assertions: &mut Vec<DecoratedNode<'a>>) {
    for child in node.named_children() {
        let kind = child.kind();
        if kind == "token_tree" || kind.ends_with("_item") {
            continue;
        }
        let callee = match kind {
            "macro_invocation" => child.child_by_field_name("macro"),
            "call_expression" => child.child_by_field_name("function"),
            _ => None,
        };
        if let Some(callee) = callee
            && callee_name(&callee).is_some_and(is_assertion_name)
        {
            assertions.push(callee);
        }
        collect_assertions(&child, assertions);
    }
}

/// Returns the name a call or a macro invocation calls, such as
/// `assert_eq` for `std::assert_eq` or `assert_send` for
/// `assert_send::<T>`
///
/// A callee that is no path, such as a closure or a field, has no name.
fn callee_name<'a>(callee: &DecoratedNode<'a>) -> Option<&'a str> {
    match callee.kind() {
        "generic_function" => callee_name(&callee.child_by_field_name("function")?),
        _ => last_segment(callee),
    }
}

/// Returns the last segment of a path, or [`None`] when `path` is no path
///
/// [`None`]: std::option::Option::None
fn last_segment<'a>(path: &DecoratedNode<'a>) -> Option<&'a str> {
    match path.kind() {
        "identifier" => Some(path.text()),
        "scoped_identifier" => path.child_by_field_name("name").map(|name| name.text()),
        _ => None,
    }
}

/// Returns whether a macro or a function by this name asserts
fn is_assertion_name(name: &str) -> bool {
    let name = name.strip_prefix("debug_").unwrap_or(name);
    name == "assert" || name.starts_with("assert_")
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for MultiAssertTest {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

stiles_plugin::export_lints![rust: [MultiAssertTest]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, Severity};

    use super::*;

    fn check(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(MultiAssertTest))];

        execute(&tree, &mut passes)
    }

    /// Returns the source text each diagnostic points at, in order
    fn flagged(source: &str) -> Vec<&str> {
        check(source)
            .iter()
            .map(|diagnostic| {
                let span = diagnostic.span();
                &source[span.start()..span.end()]
            })
            .collect()
    }

    #[test]
    fn assert_matches_counts_as_an_assertion() {
        let source = "#[test]\nfn t() {\n    assert!(a);\n    assert_matches!(b, Some(_));\n}";

        assert_eq!(flagged(source), ["assert_matches"]);
    }

    #[test]
    fn assertion_helper_counts_as_an_assertion() {
        let source = "#[test]\nfn t() {\n    assert_eq!(diagnostics.len(), 1);\n    \
                      assert_diagnostic(&diagnostics[0]).message_contains(\"x\");\n}";

        assert_eq!(flagged(source), ["assert_diagnostic"]);
    }

    #[test]
    fn assertion_in_a_closure_counts_once() {
        let source = "#[test]\nfn t() {\n    items.iter().for_each(|item| assert!(item.ok()));\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn assertion_in_a_loop_counts_once() {
        let source =
            "#[test]\nfn t() {\n    for item in items {\n        assert!(item.ok());\n    }\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn assertion_in_a_macro_argument_is_not_read() {
        let source = "#[test]\nfn t() {\n    let _ = vec![{ assert!(a); 1 }];\n    assert!(b);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn assertion_in_a_nested_function_is_not_read() {
        let source =
            "#[test]\nfn t() {\n    fn helper() {\n        assert!(a);\n    }\n    assert!(b);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn assertion_in_an_impl_in_the_body_is_not_read() {
        let source = "#[test]\nfn t() {\n    impl S {\n        fn f() { assert!(a); }\n    }\n    \
                      assert!(b);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn assertion_in_each_branch_counts_as_two() {
        let source = "#[test]\nfn t() {\n    if c {\n        assert!(a);\n    } else {\n        \
                      assert!(b);\n    }\n}";

        assert_eq!(flagged(source), ["assert"]);
    }

    #[test]
    fn assertion_on_another_item_does_not_mark_the_function() {
        let source = "#[test]\nstruct S;\nfn t() {\n    assert!(a);\n    assert!(b);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn assertion_with_a_similar_name_is_not_counted() {
        let source = "#[test]\nfn t() {\n    assert!(a);\n    asserted(b);\n    assertion!(c);\n    \
                      debug_assertion(d);\n    reassert(e);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn attribute_named_like_test_does_not_mark_the_function() {
        let source = "#[testing]\n#[test_case(1)]\n#[cfg(test)]\n#[cfg_attr(test, test)]\nfn t() {\n    \
                      assert!(a);\n    assert!(b);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn comment_between_attribute_and_function_keeps_the_mark() {
        let source = "#[test]\n// note\n/* more */\nfn t() {\n    assert!(a);\n    assert!(b);\n}";

        assert_eq!(flagged(source), ["assert"]);
    }

    #[test]
    fn debug_assertion_counts_as_an_assertion() {
        let source = "#[test]\nfn t() {\n    debug_assert!(a);\n    debug_assert_eq!(b, c);\n}";

        assert_eq!(flagged(source), ["debug_assert_eq"]);
    }

    #[test]
    fn diagnostic_names_the_rule_and_the_test() {
        let source = "#[test]\nfn parses() {\n    assert!(a);\n    assert!(b);\n}";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.multi-assert-test")
            .has_severity(Severity::Warn)
            .message_contains("test `parses` makes 2 assertions");
    }

    #[test]
    fn function_without_test_attribute_is_not_flagged() {
        let source = "fn helper() {\n    assert!(a);\n    assert!(b);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn generic_assertion_helper_counts_as_an_assertion() {
        let source = "#[test]\nfn t() {\n    fn assert_send<T: Send>() {}\n    assert_send::<A>();\n    \
                      assert_send::<B>();\n}";

        assert_eq!(flagged(source), ["assert_send::<B>"]);
    }

    #[test]
    fn method_named_assert_is_not_counted() {
        let source = "#[test]\nfn t() {\n    let output = command.assert();\n    \
                      output.assert_success();\n    assert!(a);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn qualified_assertion_counts_as_an_assertion() {
        let source = "#[test]\nfn t() {\n    core::assert!(a);\n    std::assert_eq!(b, c);\n}";

        assert_eq!(flagged(source), ["std::assert_eq"]);
    }

    #[test]
    fn qualified_assertion_helper_counts_as_an_assertion() {
        let source = "#[test]\nfn t() {\n    assert!(a);\n    \
                      stiles_testing::assert_no_diagnostics(&diagnostics);\n}";

        assert_eq!(flagged(source), ["stiles_testing::assert_no_diagnostics"]);
    }

    #[test]
    fn runtime_test_attribute_marks_the_function() {
        let source = "#[tokio::test(flavor = \"multi_thread\")]\nasync fn t() {\n    assert!(a);\n    \
                      assert!(b);\n}";

        assert_eq!(flagged(source), ["assert"]);
    }

    #[test]
    fn single_assertion_is_not_flagged() {
        let source =
            "#[test]\nfn t() {\n    let value = parse(\"42\");\n\n    assert_eq!(value, 42);\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn test_in_a_proptest_macro_is_not_read() {
        let source = "proptest! {\n    #[test]\n    fn t(x in 0..10) {\n        assert!(x >= 0);\n        \
                      assert!(x < 10);\n    }\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn test_in_a_test_module_is_flagged() {
        let source = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert!(a);\n        \
                      assert_eq!(b, c);\n    }\n}";

        assert_eq!(flagged(source), ["assert_eq"]);
    }

    #[test]
    fn test_inside_a_test_body_is_read_on_its_own() {
        let source = "#[test]\nfn outer() {\n    #[test]\n    fn inner() {\n        assert!(a);\n        \
                      assert!(b);\n    }\n    assert!(c);\n}";

        assert_diagnostic(&check(source)[0]).message_contains("test `inner`");
    }

    #[test]
    fn test_with_other_attributes_is_flagged() {
        let source = "/// Checks two things\n#[test]\n#[should_panic]\nfn t() {\n    assert!(a);\n    \
                      assert!(b);\n}";

        assert_eq!(flagged(source), ["assert"]);
    }

    #[test]
    fn test_without_assertions_is_not_flagged() {
        let source = "#[test]\nfn t() {\n    parse(\"42\").expect(\"should parse\");\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn three_assertions_are_counted_in_the_message() {
        let source =
            "#[test]\nfn t() {\n    assert!(a);\n    assert_eq!(b, c);\n    assert_ne!(d, e);\n}";

        assert_diagnostic(&check(source)[0]).message_contains("makes 3 assertions");
    }

    #[test]
    fn three_assertions_are_reported_once_at_the_second() {
        let source =
            "#[test]\nfn t() {\n    assert!(a);\n    assert_eq!(b, c);\n    assert_ne!(d, e);\n}";

        assert_eq!(flagged(source), ["assert_eq"]);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<MultiAssertTest>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<MultiAssertTest>();
    }

    #[test]
    fn trait_test_with_one_assertion_is_not_flagged() {
        let source = "#[test]\nfn trait_send() {\n    fn assert_send<T: Send>() {}\n    \
                      assert_send::<S>();\n}";

        assert_no_diagnostics(&check(source));
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<MultiAssertTest>();
    }

    #[test]
    fn two_assertions_in_one_loop_are_flagged() {
        let source = "#[test]\nfn t() {\n    for item in items {\n        assert!(item.ok());\n        \
                      assert_eq!(item.len(), 1);\n    }\n}";

        assert_eq!(flagged(source), ["assert_eq"]);
    }

    #[test]
    fn two_flagged_tests_are_reported_once_each() {
        let source = "#[test]\nfn a() {\n    assert!(a);\n    assert!(b);\n}\n#[test]\nfn b() {\n    \
                      assert!(c);\n    assert!(d);\n}";

        assert_eq!(check(source).len(), 2);
    }
}
