mod test_code;

use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, Severity};

use crate::test_code::{attribute_name, is_test_code, outer_attributes};

const RULE_ID: RuleId = RuleId::new("lint.multi-param-constructor");

/// The last path segments of the wrappers a fallible constructor returns
const WRAPPERS: [&str; 2] = ["Result", "Option"];

/// Flags a constructor that takes two or more parameters by position
///
/// A positional constructor makes every caller remember the order of the
/// fields, and two fields of one type swap without a compile error. A
/// builder from `bon` names each field at the call site, and a new field
/// does not break the callers that already exist.
///
/// The rule reports an associated function of an inherent `impl` that has
/// no `self` receiver and takes two or more parameters. It also has to
/// return the type it builds: `Self` or the impl's own type, with or without
/// generics. That type may stand alone or be the first argument of a type
/// whose path ends in `Result` or `Option`. An alias such as
/// `io::Result<Self>` therefore counts. The name of the function does not
/// matter, so `new`, `try_new`, and `from_parts` count alike. A generic
/// `impl<T> Foo<T>` counts too.
///
/// The rule skips a function that carries `#[builder]`, because `bon` then
/// generates a builder from the function itself and hides the function. A
/// `#[builder]` that names a `start_fn` does not count, because `bon` then
/// keeps the function callable under its own name beside the builder. The
/// other arguments of `#[builder]` leave the function hidden. A `const fn`
/// counts like any other, because `#[builder(const)]` gives it a builder
/// that runs in a const context. The rule skips a trait impl,
/// because the trait fixes the signature. It skips test code, which is a
/// function under a test attribute or under a `cfg` that holds only under
/// `test`. Every file in a `tests` or `benches` directory is test code too.
/// Macro bodies are token trees, so the rule never sees a function inside
/// one.
///
/// # Examples
///
/// ```ignore
/// impl Field {
///     pub const fn new(name: FieldName, presence: Presence) -> Self {
///         Self { name, presence }
///     }
/// }
/// ```
///
/// The rule reports `new`. `#[derive(bon::Builder)]` on `Field` replaces it
/// with `Field::builder().name(name).presence(presence).build()`.
pub struct MultiParamConstructor;

impl MultiParamConstructor {
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = MultiParamConstructor::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

impl RustLintPass for MultiParamConstructor {
    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_function(node).into_iter().collect()
    }
}

/// Returns a diagnostic when `function` is a positional constructor
fn check_function(function: &DecoratedNode<'_>) -> Option<Diagnostic> {
    let impl_type = inherent_impl_type(function)?;
    let parameters = function.child_by_field_name("parameters")?;
    let return_type = function.child_by_field_name("return_type")?;
    let name = function.child_by_field_name("name")?;

    let mut count = 0;
    for parameter in parameters.named_children() {
        match parameter.kind() {
            "self_parameter" => return None,
            "parameter" if is_typed_self(&parameter) => return None,
            "parameter" => count += 1,
            _ => {}
        }
    }
    if count < 2 || !returns_the_type(&return_type, &impl_type) {
        return None;
    }

    let builders: Vec<_> = outer_attributes(function)
        .into_iter()
        .filter(|attribute| attribute_name(attribute) == Some("builder"))
        .collect();
    let keeps_function = builders.iter().any(names_a_start_fn);
    if (!builders.is_empty() && !keeps_function) || is_test_code(function) {
        return None;
    }

    let advice = if keeps_function {
        "remove the `start_fn` name from `#[builder]` so that the builder replaces the function"
    } else {
        "derive a builder with `bon`, or put `#[builder]` on the function"
    };
    Some(
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Error)
            .message(format!(
                "`{}` builds `{}` from {count} positional parameters; {advice}",
                name.text(),
                impl_type.text(),
            ))
            .span(name.span())
            .build(),
    )
}

/// Returns whether a `#[builder]` attribute names its start function
///
/// `bon` reads the name from `start_fn = name`, or from `name = ...` inside
/// `start_fn(...)`. With a name, it keeps the original function callable
/// beside the builder. `start_fn(vis = "...")` without a name leaves the
/// function hidden. Only the attribute's own arguments count, so a
/// `start_fn` inside another argument does not.
fn names_a_start_fn(attribute: &DecoratedNode<'_>) -> bool {
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return false;
    };
    children(&arguments).windows(2).any(|pair| {
        if pair[0].kind() != "identifier" || pair[0].text() != "start_fn" {
            return false;
        }
        match pair[1].kind() {
            "=" => true,
            "token_tree" => children(&pair[1])
                .windows(2)
                .any(|inner| inner[0].text() == "name" && inner[1].kind() == "="),
            _ => false,
        }
    })
}

/// Returns every child of `node` in order, the unnamed ones included
///
/// A token tree holds its `=` signs and commas as unnamed children.
fn children<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    (0..node.child_count() as u32)
        .filter_map(|index| node.child(index))
        .collect()
}

/// Returns whether `parameter` is a receiver with a type, as in
/// `self: Box<Self>`
///
/// Tree-sitter reads `self` and `&self` as a `self_parameter`, but a typed
/// receiver as an ordinary `parameter` whose pattern is `self`.
fn is_typed_self(parameter: &DecoratedNode<'_>) -> bool {
    parameter
        .child_by_field_name("pattern")
        .is_some_and(|pattern| pattern.kind() == "self")
}

/// Returns the type of the inherent `impl` that `function` sits directly in
///
/// A trait impl has a `trait` field. A function in a trait, a module, or a
/// function body has no `impl` around it. Both return [`None`].
///
/// [`None`]: std::option::Option::None
fn inherent_impl_type<'a>(function: &DecoratedNode<'a>) -> Option<DecoratedNode<'a>> {
    let body = function.parent()?;
    let item = body.parent()?;
    if body.kind() != "declaration_list" || item.kind() != "impl_item" {
        return None;
    }
    if item.child_by_field_name("trait").is_some() {
        return None;
    }
    item.child_by_field_name("type")
}

/// Returns whether `return_type` is the type the impl builds
///
/// `Result<Self, E>` and `Option<Self>` count through their first type
/// argument, under any path that ends in `Result` or `Option`.
fn returns_the_type(return_type: &DecoratedNode<'_>, impl_type: &DecoratedNode<'_>) -> bool {
    if names_the_type(return_type, impl_type) {
        return true;
    }
    if return_type.kind() != "generic_type" {
        return false;
    }

    let Some(base) = return_type.child_by_field_name("type") else {
        return false;
    };
    let base = base.text();
    let last = base.rsplit("::").next().unwrap_or(base).trim();
    if !WRAPPERS.contains(&last) {
        return false;
    }

    let Some(arguments) = return_type.child_by_field_name("type_arguments") else {
        return false;
    };
    let first = arguments
        .named_children()
        .into_iter()
        .find(|argument| argument.kind() != "lifetime");
    first.is_some_and(|argument| names_the_type(&argument, impl_type))
}

/// Returns whether `ty` is `Self` or the impl's own type, generics aside
///
/// The impl's type matches by the text of its path. `Foo<T>` in
/// `impl<T> Foo<T>` therefore matches `Foo<U>` and `Foo`. `other::Foo`
/// matches only an impl of `other::Foo`.
fn names_the_type(ty: &DecoratedNode<'_>, impl_type: &DecoratedNode<'_>) -> bool {
    if ty.text() == "Self" {
        return true;
    }
    let (Some(returned), Some(implemented)) = (base_path(ty), base_path(impl_type)) else {
        return false;
    };
    returned == implemented
}

/// Returns the path of a named type without its generic arguments
fn base_path<'a>(ty: &DecoratedNode<'a>) -> Option<&'a str> {
    match ty.kind() {
        "type_identifier" | "scoped_type_identifier" => Some(ty.text()),
        "generic_type" => ty.child_by_field_name("type").map(|base| base.text()),
        _ => None,
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for MultiParamConstructor {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

stiles_plugin::export_lints![rust: [MultiParamConstructor]];

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{DecoratedTree, Language};

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![MultiParamConstructor::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    /// Runs the rule on `source` as if it were the file at `path`
    ///
    /// `parse` names every file `<test>`, so this parses again with the
    /// grammar it used and a path of the test's own.
    fn run_in_file(source: &str, path: &Path) -> Vec<Diagnostic> {
        let parsed = parse(source, Language::Rust);
        let grammar = parsed.root_node().raw().language().to_owned();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .expect("the grammar should load");
        let tree = parser.parse(source, None).expect("the source should parse");
        let tree = DecoratedTree::builder()
            .tree(tree)
            .language(Language::Rust)
            .source(source.to_owned())
            .file(PathBuf::from(path))
            .build();

        let mut passes = vec![MultiParamConstructor::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    /// Wraps `function` in `impl Foo` and runs the rule
    fn run_in_impl(function: &str) -> Vec<Diagnostic> {
        run(&format!("impl Foo {{\n    {function}\n}}"))
    }

    fn assert_reports_one(diagnostics: &[Diagnostic]) {
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.multi-param-constructor")
            .has_severity(Severity::Error);
    }

    #[test]
    fn associated_function_returning_another_type_not_flagged() {
        let diagnostics = run_in_impl("fn sum(a: u8, b: u8) -> u16 { 0 }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn associated_function_returning_nothing_not_flagged() {
        let diagnostics = run_in_impl("fn log(a: u8, b: u8) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn boxed_self_not_flagged() {
        let diagnostics = run_in_impl("fn new(a: u8, b: u8) -> Box<Self> { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_attribute_naming_a_start_fn_advises_removing_the_name() {
        let diagnostics = run_in_impl(
            "#[builder(start_fn = builder)]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_diagnostic(&diagnostics[0]).message_contains("remove the `start_fn` name");
    }

    #[test]
    fn builder_attribute_naming_a_start_fn_after_another_argument_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(on(String, into), start_fn = builder)]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn builder_attribute_naming_a_start_fn_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(start_fn = builder)]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn builder_attribute_naming_a_start_fn_in_a_list_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(start_fn(vis = \"pub(crate)\", name = builder))]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn builder_attribute_naming_a_start_fn_written_in_full_flagged() {
        let diagnostics = run_in_impl(
            "#[bon::builder(start_fn = builder)]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn builder_attribute_not_flagged() {
        let diagnostics = run_in_impl("#[builder]\n    fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_attribute_on_the_previous_function_does_not_exempt_the_next() {
        let diagnostics = run_in_impl(
            "#[builder]\n    fn a(a: u8, b: u8) -> Self { todo!() }\n    fn b(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn builder_attribute_on_the_previous_function_reports_the_next_by_name() {
        let diagnostics = run_in_impl(
            "#[builder]\n    fn a(a: u8, b: u8) -> Self { todo!() }\n    fn b(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_diagnostic(&diagnostics[0]).message_contains("`b` builds `Foo`");
    }

    #[test]
    fn builder_attribute_past_a_doc_comment_not_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(on(String, into))]\n    /// Creates one\n    #[inline]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_attribute_with_a_finish_fn_list_not_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(finish_fn(name = call, vis = \"pub\"))]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_attribute_with_a_finish_fn_not_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(finish_fn = call)]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_attribute_with_a_start_fn_inside_another_argument_not_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(on(String, start_fn = builder))]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_attribute_with_a_start_fn_visibility_only_not_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(start_fn(vis = \"pub(crate)\"))]\n    fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_attribute_written_in_full_not_flagged() {
        let diagnostics =
            run_in_impl("#[bon::builder]\n    fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_const_attribute_on_a_const_fn_not_flagged() {
        let diagnostics = run_in_impl(
            "#[builder(const)]\n    pub const fn new(a: u8, b: u8) -> Self { todo!() }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn builder_named_attribute_on_the_impl_does_not_exempt_a_function() {
        let diagnostics =
            run("#[builder]\nimpl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn cfg_not_test_flagged() {
        let diagnostics =
            run("#[cfg(not(test))]\nimpl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn cfg_test_function_not_flagged() {
        let diagnostics = run_in_impl("#[cfg(test)]\n    fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_impl_not_flagged() {
        let diagnostics =
            run("#[cfg(test)]\nimpl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_module_not_flagged() {
        let diagnostics = run(
            "#[cfg(test)]\nmod tests {\n    impl Foo {\n        fn new(a: u8, b: u8) -> Self { todo!() }\n    }\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn const_fn_flagged() {
        let diagnostics = run_in_impl("pub const fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn const_fn_without_a_visibility_flagged() {
        let diagnostics = run_in_impl("const fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn constructor_in_a_benches_file_not_flagged() {
        let diagnostics = run_in_file(
            "impl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}",
            Path::new("crates/core/benches/walk.rs"),
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constructor_in_a_generic_trait_impl_not_flagged() {
        let diagnostics =
            run("impl<T> Make<T> for Foo<T> {\n    fn make(a: T, b: T) -> Self { todo!() }\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constructor_in_a_macro_body_not_flagged() {
        let diagnostics = run(
            "macro_rules! m {\n    () => { impl Foo { fn new(a: u8, b: u8) -> Self { todo!() } } };\n}\nm! { impl Foo { fn new(a: u8, b: u8) -> Self { todo!() } } }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constructor_in_a_source_file_named_tests_flagged() {
        let diagnostics = run_in_file(
            "impl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}",
            Path::new("crates/core/src/tests.rs"),
        );

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn constructor_in_a_tests_directory_not_flagged() {
        let diagnostics = run_in_file(
            "impl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}",
            Path::new("crates/core/tests/support/fixture.rs"),
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constructor_in_a_trait_default_body_not_flagged() {
        let diagnostics = run("trait T {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constructor_in_a_trait_impl_not_flagged() {
        let diagnostics =
            run("impl From<(u8, u8)> for Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constructor_in_an_impl_inside_a_function_flagged() {
        let diagnostics = run(
            "fn outer() {\n    impl Foo {\n        fn new(a: u8, b: u8) -> Self { todo!() }\n    }\n}",
        );

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn constructor_in_an_impl_inside_a_test_function_not_flagged() {
        let diagnostics = run(
            "#[test]\nfn outer() {\n    impl Foo {\n        fn new(a: u8, b: u8) -> Self { todo!() }\n    }\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn diagnostic_is_reported_once_for_the_constructor() {
        let source = "impl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}";

        let diagnostics = run(source);

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn diagnostic_points_at_the_function_name_end() {
        let source = "impl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}";
        let start = source.find("new").expect("the name is in the source");

        let diagnostics = run(source);

        assert_eq!(diagnostics[0].span().end(), start + "new".len());
    }

    #[test]
    fn diagnostic_points_at_the_function_name_start() {
        let source = "impl Foo {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}";
        let start = source.find("new").expect("the name is in the source");

        let diagnostics = run(source);

        assert_eq!(diagnostics[0].span().start(), start);
    }

    #[test]
    fn free_function_returning_a_type_not_flagged() {
        let diagnostics = run("fn make(a: u8, b: u8) -> Foo { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn function_in_an_impl_of_dyn_trait_returning_self_flagged() {
        let diagnostics = run("impl dyn Shape {\n    fn new(a: u8, b: u8) -> Self { todo!() }\n}");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn generic_impl_returning_its_type_with_other_generics_flagged() {
        let diagnostics =
            run("impl<T> Foo<T> {\n    fn map<U>(a: T, b: U) -> Foo<U> { todo!() }\n}");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn generic_impl_returning_its_type_without_generics_flagged() {
        let diagnostics =
            run("impl<'a> Foo<'a> {\n    fn new(a: &'a str, b: usize) -> Foo { todo!() }\n}");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn generic_impl_returning_self_flagged_once() {
        let diagnostics =
            run("impl<T: Clone> Foo<T> {\n    fn new(a: T, b: T) -> Self { todo!() }\n}");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn generic_impl_returning_self_names_the_generic_type() {
        let diagnostics =
            run("impl<T: Clone> Foo<T> {\n    fn new(a: T, b: T) -> Self { todo!() }\n}");

        assert_diagnostic(&diagnostics[0]).message_contains("`new` builds `Foo<T>`");
    }

    #[test]
    fn io_result_alias_of_self_flagged() {
        let diagnostics = run_in_impl("fn open(a: u8, b: u8) -> io::Result<Self> { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn message_names_the_function_the_type_and_the_count() {
        let diagnostics = run_in_impl("fn of(a: u8, b: u8, c: u8) -> Self { todo!() }");

        assert_diagnostic(&diagnostics[0])
            .message_contains("`of` builds `Foo` from 3 positional parameters")
            .message_contains("`bon`")
            .message_contains("`#[builder]`");
    }

    #[test]
    fn method_with_a_receiver_returning_self_not_flagged() {
        let diagnostics = run_in_impl("fn with(self, a: u8, b: u8) -> Self { self }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_with_a_reference_receiver_returning_self_not_flagged() {
        let diagnostics = run_in_impl("fn join(&self, a: u8, b: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_with_a_typed_mutable_receiver_returning_self_not_flagged() {
        let diagnostics =
            run_in_impl("fn with(mut self: Box<Self>, a: u8, b: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_with_a_typed_receiver_returning_self_not_flagged() {
        let diagnostics = run_in_impl("fn with(self: Box<Self>, a: u8, b: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn one_parameter_not_flagged() {
        let diagnostics = run_in_impl("fn new(a: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn option_of_another_type_not_flagged() {
        let diagnostics = run_in_impl("fn find(a: u8, b: u8) -> Option<Bar> { None }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn option_of_self_flagged() {
        let diagnostics = run_in_impl("fn find(a: u8, b: u8) -> Option<Self> { None }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn option_of_the_impl_type_flagged() {
        let diagnostics = run_in_impl("fn find(a: u8, b: u8) -> Option<Foo> { None }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn parameter_attribute_does_not_count_as_a_parameter() {
        let diagnostics = run_in_impl("fn new(#[cfg(unix)] a: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pattern_parameters_constructor_is_flagged_once() {
        let diagnostics =
            run_in_impl("fn new((a, b): (u8, u8), Bar { c }: Bar) -> Self { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn pattern_parameters_count_once_each() {
        let diagnostics =
            run_in_impl("fn new((a, b): (u8, u8), Bar { c }: Bar) -> Self { todo!() }");

        assert_diagnostic(&diagnostics[0]).message_contains("from 2 positional parameters");
    }

    #[test]
    fn result_alias_with_a_lifetime_first_flagged() {
        let diagnostics = run(
            "impl<'a> Foo<'a> {\n    fn parse(a: &'a str, b: u8) -> Result<'a, Self> { todo!() }\n}",
        );

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn result_of_another_type_not_flagged() {
        let diagnostics = run_in_impl("fn parse(a: u8, b: u8) -> Result<u8, Self> { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn result_of_self_flagged() {
        let diagnostics =
            run_in_impl("fn try_new(a: u8, b: u8) -> Result<Self, Error> { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn result_of_self_with_one_argument_flagged() {
        let diagnostics =
            run_in_impl("fn try_new(a: u8, b: u8) -> anyhow::Result<Self> { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn result_of_self_written_in_full_flagged() {
        let diagnostics =
            run_in_impl("fn try_new(a: u8, b: u8) -> std::result::Result<Self, Error> { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn result_of_the_impl_type_flagged() {
        let diagnostics = run_in_impl("fn try_new(a: u8, b: u8) -> Result<Foo, Error> { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn scoped_return_type_matching_a_scoped_impl_flagged() {
        let diagnostics =
            run("impl outer::Foo {\n    fn new(a: u8, b: u8) -> outer::Foo { todo!() }\n}");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn scoped_return_type_of_another_path_not_flagged() {
        let diagnostics = run_in_impl("fn new(a: u8, b: u8) -> other::Foo { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn self_associated_type_not_flagged() {
        let diagnostics = run_in_impl("fn new(a: u8, b: u8) -> Self::Output { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn self_reference_not_flagged() {
        let diagnostics = run_in_impl("fn pick(a: &Self, b: &Self) -> &Self { a }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn test_attribute_function_not_flagged() {
        let diagnostics = run_in_impl("#[test]\n    fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn three_parameters_returning_self_flagged_once() {
        let diagnostics = run_in_impl("fn of(a: u8, b: u8, c: u8) -> Self { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<MultiParamConstructor>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<MultiParamConstructor>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<MultiParamConstructor>();
    }

    #[test]
    fn two_parameters_returning_self_flagged_once() {
        let diagnostics = run_in_impl("fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn two_parameters_returning_self_names_the_function_the_type_and_the_count() {
        let diagnostics = run_in_impl("fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_diagnostic(&diagnostics[0])
            .message_contains("`new` builds `Foo` from 2 positional parameters");
    }

    #[test]
    fn two_parameters_returning_the_impl_type_flagged() {
        let diagnostics = run_in_impl("fn new(a: u8, b: u8) -> Foo { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn unsafe_async_fn_flagged() {
        let diagnostics = run_in_impl("pub async unsafe fn new(a: u8, b: u8) -> Self { todo!() }");

        assert_reports_one(&diagnostics);
    }

    #[test]
    fn zero_parameters_not_flagged() {
        let diagnostics = run_in_impl("fn new() -> Self { todo!() }");

        assert_no_diagnostics(&diagnostics);
    }
}
