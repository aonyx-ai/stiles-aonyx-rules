use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{
    AttachFixError, DecoratedNode, Diagnostic, Edit, FileText, Fix, Severity, Span,
};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(unsorted_test::RULE_ID.as_str(), "lint.unsorted-test");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.unsorted-test");

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
/// `proptest!` or `cfg_if!` is not read. Its order is in tokens that the
/// syntax tree does not show as items, so it gets no finding and no fix.
///
/// `stiles check --fix` sorts all the tests of the module at once. A test
/// moves with its attributes, its doc comments, and the comments directly
/// above it. A comment that a blank line sets apart stays in place. Helpers
/// and other items between the tests stay in place too. The tests fill the
/// slots between them in sorted order. Every finding in one module carries
/// this same fix.
///
/// The rule attaches no fix when a `macro_rules!` sits between the tests. A
/// macro is visible only below its definition, so a move could break a test
/// that calls it.
///
/// # Examples
///
/// ```text
/// // DON'T
/// #[test]
/// fn parse_with_valid_input_returns_value() {}
///
/// #[test]
/// fn parse_with_empty_input_returns_error() {}
///
/// // DO
/// #[test]
/// fn parse_with_empty_input_returns_error() {}
///
/// #[test]
/// fn parse_with_valid_input_returns_value() {}
/// ```
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
/// own. Among the tests above it, that is where the test belongs. Every
/// diagnostic of the module carries the same fix, which sorts all its tests.
fn check_scope(scope: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let tests = tests(scope);
    let names: Vec<DecoratedNode<'_>> = tests
        .iter()
        .filter_map(|test| test.child_by_field_name("name"))
        .collect();
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

    let Some(fix) = diagnostics.first().and_then(|_| sorting_fix(scope, &tests)) else {
        return diagnostics;
    };
    diagnostics
        .into_iter()
        .map(|diagnostic| {
            diagnostic
                .with_fix(fix.clone())
                .unwrap_or_else(AttachFixError::into_diagnostic)
        })
        .collect()
}

/// Returns the function item of each test that is a direct item of the
/// scope, in source order
///
/// Tree-sitter makes each attribute a sibling of the item it decorates. The
/// walk notes a test attribute in front of an item and skips comments. Any
/// item clears the note.
fn tests<'a>(scope: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let mut tests = Vec::new();
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
                if marked && kind == "function_item" && child.child_by_field_name("name").is_some()
                {
                    tests.push(child);
                }
                marked = false;
            }
        }
    }

    tests
}

/// Returns the fix that puts the tests of the scope in sorted order, or
/// [`None`] when a move could change what the code means
///
/// Each test owns a slot of the source, which [`slot`] describes. The
/// slots keep their places, and the tests fill them in sorted order. The
/// text between two slots stays where it is. A `macro_rules!` between the
/// tests makes their order matter, because a macro is visible only below
/// its definition, so the scope then gets no fix.
///
/// [`None`]: std::option::Option::None
fn sorting_fix(scope: &DecoratedNode<'_>, tests: &[DecoratedNode<'_>]) -> Option<Fix> {
    let source = scope.file_text();
    let first = tests.first()?.span().start();
    let last = tests.last()?.span().end();
    let defines_macro = scope.named_children().iter().any(|child| {
        child.kind() == "macro_definition"
            && child.span().start() > first
            && child.span().end() < last
    });
    if defines_macro {
        return None;
    }

    let slots: Vec<(usize, usize)> = tests
        .iter()
        .map(|test| slot(&source, scope, test))
        .collect::<Option<_>>()?;
    let mut order: Vec<usize> = (0..tests.len()).collect();
    order.sort_by_key(|&index| {
        tests[index]
            .child_by_field_name("name")
            .map_or("", |name| sort_key(name.text()))
    });

    let file = scope.span().file_path().clone();
    let edits: Vec<Edit> = slots
        .iter()
        .zip(&order)
        .enumerate()
        .filter(|&(position, (_, &test))| position != test)
        .map(|(_, (&(start, end), &test))| {
            let (from, to) = slots[test];
            Some(
                Edit::builder()
                    .span(
                        Span::builder()
                            .file(file.clone())
                            .start(start)
                            .end(end)
                            .build(),
                    )
                    .replacement(source.get(from..to)?)
                    .build(),
            )
        })
        .collect::<Option<_>>()?;

    Fix::builder()
        .message("sort the tests of the module")
        .edits(edits)
        .build()
        .ok()
}

/// Returns the start and the end of the slot that `test` owns
///
/// The slot holds the attributes, the doc comments, and the comments
/// directly above the function, and a comment that trails it on its last
/// line. A comment above them belongs to the test only when no blank line
/// lies between. An attribute always belongs to it. An inner attribute or
/// an inner doc comment belongs to the module, so the slot stops there.
fn slot(
    source: &FileText<'_>,
    scope: &DecoratedNode<'_>,
    test: &DecoratedNode<'_>,
) -> Option<(usize, usize)> {
    let siblings = scope.named_children();
    let index = siblings
        .iter()
        .position(|sibling| sibling.id() == test.id())?;

    let run: Vec<&DecoratedNode<'_>> = siblings[..index]
        .iter()
        .rev()
        .take_while(|sibling| match sibling.kind() {
            "attribute_item" => true,
            "line_comment" | "block_comment" => {
                sibling.child_by_field_name("inner").is_none()
                    && source.indent(sibling.span().start()).is_some()
            }
            _ => false,
        })
        .collect();
    let attributes = run
        .iter()
        .rposition(|sibling| sibling.kind() == "attribute_item")
        .map_or(0, |position| position + 1);

    let mut start = run[..attributes]
        .last()
        .map_or(test.span().start(), |sibling| sibling.span().start());
    for sibling in &run[attributes..] {
        if source
            .get(sibling.span().end()..start)?
            .matches('\n')
            .count()
            > 1
        {
            break;
        }
        start = sibling.span().start();
    }

    let end = siblings
        .get(index + 1)
        .filter(|next| match next.kind() {
            "line_comment" | "block_comment" => source
                .get(test.span().end()..next.span().start())
                .is_some_and(|between| !between.contains('\n')),
            _ => false,
        })
        .map_or(test.span().end(), |next| next.span().end());

    Some((start, end))
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

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::none()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [UnsortedTest]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_fix, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(UnsortedTest))]
    }

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

    fn run_documented(source: &str) -> Vec<stiles_types::Diagnostic> {
        let tree = stiles_testing::parse(source, stiles_types::Language::Rust);
        stiles_testing::execute(
            &tree,
            &mut [Box::new(stiles_rust::RustLintPassAdapter::new(
                UnsortedTest,
            ))],
        )
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
    fn cfg_if_macro_body_is_not_read() {
        let source = "cfg_if::cfg_if! {\n    if #[cfg(unix)] {\n        #[test]\n        fn b() {}\n        #[test]\n        fn a() {}\n    }\n}\n";

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
    fn fix_is_the_same_on_every_finding_of_a_module() {
        let source =
            "#[test]\nfn d() {}\n#[test]\nfn b() {}\n#[test]\nfn c() {}\n#[test]\nfn a() {}\n";
        let diagnostics = check(source);

        assert_eq!(diagnostics[0].fix(), diagnostics[1].fix());
    }

    #[test]
    fn fix_keeps_a_comment_that_a_blank_line_sets_apart() {
        assert_fix()
            .source(
                "// Parser tests\n\n#[test]\nfn b() {}\n\n#[test]\nfn a() {}\n",
                Language::Rust,
            )
            .passes(passes)
            .produces("// Parser tests\n\n#[test]\nfn a() {}\n\n#[test]\nfn b() {}\n");
    }

    #[test]
    fn fix_keeps_helpers_in_place() {
        assert_fix()
            .source("#[test]\nfn c() {}\n\nfn helper() {}\n\n#[test]\nfn b() {}\n\nconst X: u8 = 0;\n\n#[test]\nfn a() {}\n", Language::Rust)
            .passes(passes)
            .produces("#[test]\nfn a() {}\n\nfn helper() {}\n\n#[test]\nfn b() {}\n\nconst X: u8 = 0;\n\n#[test]\nfn c() {}\n");
    }

    #[test]
    fn fix_keeps_the_inner_docs_of_the_module_in_place() {
        assert_fix()
            .source("mod tests {\n    //! Tests\n    #[test]\n    fn b() {}\n\n    #[test]\n    fn a() {}\n}\n", Language::Rust)
            .passes(passes)
            .produces("mod tests {\n    //! Tests\n    #[test]\n    fn a() {}\n\n    #[test]\n    fn b() {}\n}\n");
    }

    #[test]
    fn fix_keeps_the_order_of_tests_with_the_same_name() {
        assert_fix()
            .source("#[test]\nfn b() {}\n#[cfg(unix)]\n#[test]\nfn a() {}\n#[cfg(windows)]\n#[test]\nfn a() {}\n", Language::Rust)
            .passes(passes)
            .produces("#[cfg(unix)]\n#[test]\nfn a() {}\n#[cfg(windows)]\n#[test]\nfn a() {}\n#[test]\nfn b() {}\n");
    }

    #[test]
    fn fix_moves_a_trailing_comment_with_the_test() {
        assert_fix()
            .source(
                "#[test]\nfn b() {} // slow\n#[test]\nfn a() {}\n",
                Language::Rust,
            )
            .passes(passes)
            .produces("#[test]\nfn a() {}\n#[test]\nfn b() {} // slow\n");
    }

    #[test]
    fn fix_moves_an_attribute_that_a_blank_line_sets_apart() {
        assert_fix()
            .source(
                "#[test]\nfn b() {}\n#[test]\n// note\n\n#[ignore]\nfn a() {}\n",
                Language::Rust,
            )
            .passes(passes)
            .produces("#[test]\n// note\n\n#[ignore]\nfn a() {}\n#[test]\nfn b() {}\n");
    }

    #[test]
    fn fix_moves_attributes_and_doc_comments_with_the_test() {
        assert_fix()
            .source(
                "#[test]\nfn b() {}\n\n/// Docs\n#[ignore]\n#[test]\n#[should_panic]\nfn a() {}\n",
                Language::Rust,
            )
            .passes(passes)
            .produces(
                "/// Docs\n#[ignore]\n#[test]\n#[should_panic]\nfn a() {}\n\n#[test]\nfn b() {}\n",
            );
    }

    #[test]
    fn fix_moves_the_comment_directly_above_the_test() {
        assert_fix()
            .source(
                "#[test]\nfn b() {}\n\n// About a\n#[test]\nfn a() {}\n",
                Language::Rust,
            )
            .passes(passes)
            .produces("// About a\n#[test]\nfn a() {}\n\n#[test]\nfn b() {}\n");
    }

    #[test]
    fn fix_sorts_a_long_reversed_list_in_one_round() {
        let names: Vec<String> = (0..15).rev().map(|index| format!("t{index:02}")).collect();
        let source: String = names
            .iter()
            .map(|name| format!("#[test]\nfn {name}() {{}}\n"))
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        let expected: String = sorted
            .iter()
            .map(|name| format!("#[test]\nfn {name}() {{}}\n"))
            .collect();
        let tree = parse(&source, Language::Rust);
        let diagnostics = execute(&tree, &mut passes());
        let fix = diagnostics[0]
            .fix()
            .expect("the finding should carry a fix");

        let mut text = source.clone();
        for edit in fix.edits().iter().rev() {
            text.replace_range(edit.span().start()..edit.span().end(), edit.replacement());
        }

        assert_eq!(text, expected);
    }
    #[test]
    fn fix_sorts_a_nested_module_on_its_own() {
        assert_fix()
            .source("mod tests {\n    #[test]\n    fn z() {}\n    mod inner {\n        #[test]\n        fn y() {}\n        #[test]\n        fn x() {}\n    }\n    #[test]\n    fn a() {}\n}\n", Language::Rust)
            .passes(passes)
            .produces("mod tests {\n    #[test]\n    fn a() {}\n    mod inner {\n        #[test]\n        fn x() {}\n        #[test]\n        fn y() {}\n    }\n    #[test]\n    fn z() {}\n}\n");
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
    fn macro_definition_above_the_tests_keeps_the_fix() {
        let diagnostics =
            check("macro_rules! m { () => {} }\n#[test]\nfn b() {}\n#[test]\nfn a() { m!(); }\n");

        assert_diagnostic(&diagnostics[0]).has_fix();
    }

    #[test]
    fn macro_definition_between_the_tests_has_no_fix() {
        let diagnostics =
            check("#[test]\nfn b() {}\nmacro_rules! m { () => {} }\n#[test]\nfn a() { m!(); }\n");

        assert_diagnostic(&diagnostics[0]).has_no_fix();
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
            .has_span("<test>", start..start + 1);
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
