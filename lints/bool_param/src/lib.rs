use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, RuleOptions, Severity};

const RULE_ID: RuleId = RuleId::new("lint.bool-param");

/// Flags `bool` parameters in function signatures and `bool` fields in
/// struct definitions
///
/// Boolean parameters and fields obscure intent at call sites and in data
/// models. An enum with meaningful variant names makes the code
/// self-documenting and prevents accidental transposition of arguments.
///
/// The rule reads a declaration and a definition alike, so a `bool` on a
/// trait method is reported where the trait fixes it.
///
/// The rule skips a signature that sits on a boundary, because a caller
/// outside Rust fixes it. An `extern` ABI, an `extern` block, and an
/// attribute macro the project named in `boundary-attributes` are such
/// boundaries. `lint.repeated-primitive-params` reads the same option, and
/// it means the same thing; a project that sets one usually sets both.
///
/// A struct is not a signature, so a `bool` field is reported whatever the
/// struct carries.
///
/// # Examples
///
/// ```
/// use bool_param::BoolParam;
/// use stiles_testing::{execute, parse};
/// use stiles_types::Language;
///
/// let tree = parse("fn toggle(enabled: bool) {}", Language::Rust);
/// let mut passes = vec![BoolParam::into_lint_pass()];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
#[derive(Default)]
pub struct BoolParam {
    boundary_attributes: Vec<String>,
}

impl BoolParam {
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
    ///
    /// The pass starts with no boundary attributes, so it reports every
    /// signature that `extern` does not already excuse. Stiles calls
    /// `configure` on each pass it constructs; a caller that builds one
    /// directly and wants the `boundary-attributes` exemption has to call
    /// it too.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = BoolParam::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self::default()))
    }
}

/// Returns whether the given node is a `primitive_type` with text `"bool"`
fn is_bool_type(node: &DecoratedNode<'_>) -> bool {
    node.kind() == "primitive_type" && node.text() == "bool"
}

/// Reports every `bool` parameter the signature declares
///
/// A declaration and a definition are one position for this rule. An
/// implementor writes a body against the signature a trait fixed, and every
/// caller reads the trait. So a `bool` on a declaration reaches further
/// than one on a free function.
///
/// The rule skips a signature on a boundary, because a caller outside Rust
/// fixes it.
fn check_signature(node: &DecoratedNode<'_>, boundary_attributes: &[String]) -> Vec<Diagnostic> {
    if boundary::crosses_a_boundary(node, boundary_attributes) {
        return Vec::new();
    }

    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();
    for param in parameters.named_children() {
        if param.kind() != "parameter" {
            continue;
        }
        let Some(ty) = param.child_by_field_name("type") else {
            continue;
        };
        if is_bool_type(&ty) {
            diagnostics.push(
                Diagnostic::builder()
                    .rule_id(RULE_ID)
                    .severity(Severity::Error)
                    .message("parameter has type `bool`; use an enum with meaningful variants")
                    .span(ty.span())
                    .build(),
            );
        }
    }
    diagnostics
}

impl RustLintPass for BoolParam {
    fn configure(&mut self, options: &RuleOptions) {
        self.boundary_attributes = options.names(RULE_ID, boundary::OPTION).unwrap_or_default();
    }

    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_signature(node, &self.boundary_attributes)
    }

    fn check_function_signature_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_signature(node, &self.boundary_attributes)
    }

    fn check_struct_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };

        let mut diagnostics = Vec::new();
        for child in body.named_children() {
            let ty = match child.kind() {
                "field_declaration" => child.child_by_field_name("type"),
                _ => None,
            };
            let Some(ty) = ty else {
                continue;
            };
            if is_bool_type(&ty) {
                diagnostics.push(
                    Diagnostic::builder()
                        .rule_id(RULE_ID)
                        .severity(Severity::Error)
                        .message(
                            "struct field has type `bool`; use an enum with meaningful variants",
                        )
                        .span(ty.span())
                        .build(),
                );
            }
        }
        diagnostics
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for BoolParam {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.bool-param")]
    }
}

stiles_plugin::export_lints![rust: [BoolParam::default()]];

#[cfg(test)]
mod tests {
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, RuleOption, Severity};

    use super::*;

    fn adapt() -> Box<dyn LintPass> {
        BoolParam::into_lint_pass()
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![adapt()];
        execute(&tree, &mut passes)
    }

    /// Runs the rule as stiles runs it, with `boundary-attributes` set
    fn run_with_boundary_attributes(source: &str, boundary: &[&str]) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let options = RuleOptions::new(vec![
            RuleOption::builder()
                .rule(RULE_ID.as_str().to_owned())
                .name("boundary-attributes")
                .values(boundary.iter().map(|name| (*name).to_owned()).collect())
                .build(),
        ]);

        let mut pass = adapt();
        pass.configure(&options);
        let mut passes = vec![pass];

        execute(&tree, &mut passes)
    }

    #[test]
    fn bool_local_variable_not_flagged() {
        let diagnostics = run("fn foo() { let x: bool = true; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bool_param_behind_a_configured_attribute_not_flagged() {
        let diagnostics = run_with_boundary_attributes("#[shard]\nfn foo(x: bool) {}", &["shard"]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bool_param_behind_a_configured_attribute_written_in_full_not_flagged() {
        let diagnostics =
            run_with_boundary_attributes("#[topcoat::shard]\nfn foo(x: bool) {}", &["shard"]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bool_param_behind_an_attribute_nobody_configured_flagged_once() {
        let diagnostics = run("#[shard]\nfn foo(x: bool) {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bool_param_behind_an_attribute_nobody_configured_flagged_with_the_parameter_message() {
        let diagnostics = run("#[shard]\nfn foo(x: bool) {}");

        assert_diagnostic(&diagnostics[0]).message_contains("parameter has type `bool`");
    }

    #[test]
    fn bool_param_behind_another_attribute_flagged_once() {
        let diagnostics = run_with_boundary_attributes("#[inline]\nfn foo(x: bool) {}", &["shard"]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bool_param_behind_another_attribute_flagged_with_the_parameter_message() {
        let diagnostics = run_with_boundary_attributes("#[inline]\nfn foo(x: bool) {}", &["shard"]);

        assert_diagnostic(&diagnostics[0]).message_contains("parameter has type `bool`");
    }

    #[test]
    fn bool_param_in_an_extern_block_not_flagged() {
        let diagnostics = run("unsafe extern \"C\" { fn f(x: bool); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bool_param_in_an_extern_function_not_flagged() {
        let diagnostics = run("pub extern \"C\" fn foo(x: bool) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bool_param_in_function_flagged_once() {
        let diagnostics = run("fn foo(x: bool) {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bool_param_in_function_flagged_with_rule_id_severity_and_message() {
        let diagnostics = run("fn foo(x: bool) {}");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.bool-param")
            .has_severity(Severity::Error)
            .message_contains("parameter has type `bool`");
    }

    #[test]
    fn bool_param_on_a_trait_declaration_flagged_once() {
        let diagnostics = run("trait T {\n    fn f(&self, x: bool);\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bool_param_on_a_trait_declaration_flagged_with_rule_id_and_message() {
        let diagnostics = run("trait T {\n    fn f(&self, x: bool);\n}");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.bool-param")
            .message_contains("parameter has type `bool`");
    }

    #[test]
    fn bool_param_on_a_trait_declaration_with_a_default_body_flagged_once() {
        let diagnostics = run("trait T {\n    fn f(&self, x: bool) {}\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bool_return_type_not_flagged() {
        let diagnostics = run("fn foo() -> bool { true }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bool_struct_field_behind_a_configured_attribute_flagged_once() {
        let diagnostics =
            run_with_boundary_attributes("#[shard]\nstruct Config { verbose: bool }", &["shard"]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bool_struct_field_behind_a_configured_attribute_flagged_with_the_field_message() {
        let diagnostics =
            run_with_boundary_attributes("#[shard]\nstruct Config { verbose: bool }", &["shard"]);

        assert_diagnostic(&diagnostics[0]).message_contains("struct field has type `bool`");
    }

    #[test]
    fn bool_struct_field_flagged_once() {
        let diagnostics = run("struct Config { verbose: bool }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bool_struct_field_flagged_with_rule_id_severity_and_message() {
        let diagnostics = run("struct Config { verbose: bool }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.bool-param")
            .has_severity(Severity::Error)
            .message_contains("struct field has type `bool`");
    }

    /// Pins that this rule ignores every signature the shared corpus holds
    ///
    /// The corpus is what stops two rules reading one option from drifting
    /// apart. `bool_param` once reported an `extern` signature that
    /// `repeated_primitive_params` skipped, and no test failed, because
    /// only one of them had `extern` cases.
    #[test]
    fn every_boundary_in_the_shared_corpus_reports_nothing() {
        for source in boundary::corpus::EXEMPT {
            let diagnostics = run_with_boundary_attributes(source, boundary::corpus::ATTRIBUTES);

            assert!(
                diagnostics.is_empty(),
                "should report nothing for a boundary: {source}"
            );
        }
    }

    /// Pins that this rule still reports what the corpus says it must
    ///
    /// A rule that exempts too much reports nothing, which reads exactly
    /// like a rule that found no fault.
    #[test]
    fn every_signature_off_the_boundary_in_the_shared_corpus_reports() {
        for source in boundary::corpus::REPORTED {
            let diagnostics = run_with_boundary_attributes(source, boundary::corpus::ATTRIBUTES);

            assert!(
                !diagnostics.is_empty(),
                "should report a signature on no boundary: {source}"
            );
        }
    }

    #[test]
    fn multiple_bool_params_each_flagged_first_with_rule_id_severity_and_message() {
        let diagnostics = run("fn foo(a: bool, b: bool) {}");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.bool-param")
            .has_severity(Severity::Error)
            .message_contains("parameter has type `bool`");
    }

    #[test]
    fn multiple_bool_params_each_flagged_once() {
        let diagnostics = run("fn foo(a: bool, b: bool) {}");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn multiple_bool_params_each_flagged_second_with_rule_id_severity_and_message() {
        let diagnostics = run("fn foo(a: bool, b: bool) {}");

        assert_diagnostic(&diagnostics[1])
            .has_rule_id("lint.bool-param")
            .has_severity(Severity::Error)
            .message_contains("parameter has type `bool`");
    }

    #[test]
    fn non_bool_param_not_flagged() {
        let diagnostics = run("fn foo(x: i32, y: String) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<BoolParam>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<BoolParam>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<BoolParam>();
    }
}
