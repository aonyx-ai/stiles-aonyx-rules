use whisker_rust::{RustLintPass, RustLintPassAdapter};
use whisker_types::{DecoratedNode, Diagnostic, LintPass, RuleId, RuleOptions, Severity};

const RULE_ID: RuleId = RuleId::new("lint.repeated-primitive-params");

/// Flags a function that takes two or more parameters of one primitive type
///
/// Each parameter accepts the argument meant for the other, so a caller can
/// swap them and the code still compiles. A newtype for each parameter turns
/// the swap into a type error.
///
/// A lone primitive parameter is safe, because it has no sibling to swap
/// with. Return types and struct fields are not argument positions, so the
/// rule never looks at them. `bool` belongs to `lint.bool-param`.
///
/// The `boundary-attributes` option names the attribute macros that fix a
/// signature the way `extern` does. See [`boundary::crosses_a_boundary`].
#[derive(Default)]
pub struct RepeatedPrimitiveParams {
    boundary_attributes: Vec<String>,
}

impl RepeatedPrimitiveParams {
    /// Creates a boxed [`LintPass`] suitable for the whisker pipeline
    ///
    /// The pass starts with no boundary attributes, so it reports every
    /// signature that `extern` does not already excuse. Whisker calls
    /// `configure` on each pass it constructs; a caller that builds one
    /// directly and wants the `boundary-attributes` exemption has to call
    /// it too.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = RepeatedPrimitiveParams::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self::default()))
    }
}

/// The parameters of one signature grouped under one primitive type
struct ParameterGroup<'a> {
    type_name: String,
    type_node: DecoratedNode<'a>,
    parameter_names: Vec<&'a str>,
}

/// Returns the name to group a parameter under, or [`None`] when the rule
/// does not track the type
///
/// The rule tracks the primitive types and `String`, but not `bool`, which
/// `lint.bool-param` owns. A reference keeps its `&` and its `mut`: a
/// `String` does not fit a `&str` parameter, and a `&str` does not fit a
/// `&mut str` one. A lifetime is not part of that decision, so it drops out
/// and `&'a str` groups with `&str`.
fn primitive_type_name(node: &DecoratedNode<'_>) -> Option<String> {
    match node.kind() {
        "primitive_type" => match node.text() {
            "bool" => None,
            text => Some(text.to_string()),
        },
        "type_identifier" => match node.text() {
            "String" => Some("String".to_string()),
            _ => None,
        },
        "reference_type" => {
            let inner = node.child_by_field_name("type")?;
            let inner = primitive_type_name(&inner)?;
            let mutable = node
                .named_children()
                .iter()
                .any(|child| child.kind() == "mutable_specifier");
            match mutable {
                true => Some(format!("&mut {inner}")),
                false => Some(format!("&{inner}")),
            }
        }
        _ => None,
    }
}

/// Joins parameter names into an English list, each name in backticks
fn join_names(names: &[&str]) -> String {
    let names: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, [first])) => format!("{first} and {last}"),
        Some((last, rest)) => format!("{}, and {last}", rest.join(", ")),
    }
}

/// Reports every primitive type that the signature uses more than once
///
/// The rule skips a signature on a boundary. The diagnostic points at the type of
/// the first parameter in the group, so two repeated types give two
/// diagnostics at two places.
fn check_signature(node: &DecoratedNode<'_>, boundary: &[String]) -> Vec<Diagnostic> {
    if boundary::crosses_a_boundary(node, boundary) {
        return Vec::new();
    }

    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };

    let mut groups: Vec<ParameterGroup<'_>> = Vec::new();
    for parameter in parameters.named_children() {
        if parameter.kind() != "parameter" {
            continue;
        }
        let Some(type_node) = parameter.child_by_field_name("type") else {
            continue;
        };
        let Some(type_name) = primitive_type_name(&type_node) else {
            continue;
        };
        let Some(pattern) = parameter.child_by_field_name("pattern") else {
            continue;
        };

        match groups.iter_mut().find(|group| group.type_name == type_name) {
            Some(group) => group.parameter_names.push(pattern.text()),
            None => groups.push(ParameterGroup {
                type_name,
                type_node,
                parameter_names: vec![pattern.text()],
            }),
        }
    }

    let mut diagnostics = Vec::new();
    for ParameterGroup {
        type_name,
        type_node,
        parameter_names,
    } in groups
    {
        if parameter_names.len() < 2 {
            continue;
        }
        diagnostics.push(Diagnostic::new(
            RULE_ID,
            Severity::Warn,
            format!(
                "parameters {} share type `{type_name}`; a caller can transpose them, \
                 so give each one a newtype",
                join_names(&parameter_names)
            ),
            type_node.span(),
        ));
    }
    diagnostics
}

impl RustLintPass for RepeatedPrimitiveParams {
    fn configure(&mut self, options: &RuleOptions) {
        self.boundary_attributes = options.names(RULE_ID, boundary::OPTION).unwrap_or_default();
    }

    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_signature(node, &self.boundary_attributes)
    }

    fn check_function_signature_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_signature(node, &self.boundary_attributes)
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for RepeatedPrimitiveParams {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.repeated-primitive-params")]
    }
}

whisker_plugin::export_lints![rust: [RepeatedPrimitiveParams::default()]];

#[cfg(test)]
mod tests {
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, RuleOption, Severity};

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![RepeatedPrimitiveParams::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    /// Runs the rule as whisker runs it, with `boundary-attributes` set
    fn run_with_boundary_attributes(source: &str, boundary: &[&str]) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let options = RuleOptions::new(vec![RuleOption::new(
            RULE_ID.as_str().to_owned(),
            "boundary-attributes".to_owned(),
            boundary.iter().map(|name| (*name).to_owned()).collect(),
        )]);

        let mut pass = RepeatedPrimitiveParams::into_lint_pass();
        pass.configure(&options);
        let mut passes = vec![pass];

        execute(&tree, &mut passes)
    }

    #[test]
    fn check_signature_with_bool_pair_reports_nothing() {
        let diagnostics = run("fn f(a: bool, b: bool) {}");

        assert_no_diagnostics(&diagnostics);
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
    fn check_signature_with_a_configured_attribute_reports_nothing() {
        let diagnostics =
            run_with_boundary_attributes("#[shard]\nfn f(a: String, b: String) {}", &["shard"]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_a_configured_attribute_behind_another_reports_nothing() {
        let diagnostics = run_with_boundary_attributes(
            "#[shard]\n/// Doc\n#[inline]\nfn f(a: String, b: String) {}",
            &["shard"],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_a_configured_attribute_written_in_full_reports_nothing() {
        let diagnostics = run_with_boundary_attributes(
            "#[topcoat::shard]\nfn f(a: String, b: String) {}",
            &["shard"],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_an_attribute_nobody_configured_reports_group() {
        let diagnostics = run("#[shard]\nfn f(a: String, b: String) {}");

        assert_diagnostic(&diagnostics[0]).message_contains("share type `String`");
    }

    #[test]
    fn check_signature_with_an_attribute_nobody_configured_reports_one_diagnostic() {
        let diagnostics = run("#[shard]\nfn f(a: String, b: String) {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn check_signature_with_a_configured_attribute_on_a_method_reports_nothing() {
        let diagnostics = run_with_boundary_attributes(
            "impl S {\n    #[shard]\n    fn f(a: String, b: String) {}\n}",
            &["shard"],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_after_another_signatures_attribute_reports_group() {
        let diagnostics = run_with_boundary_attributes(
            "#[shard]\nfn a(x: String, y: String) {}\nfn b(x: String, y: String) {}",
            &["shard"],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("share type `String`");
    }

    #[test]
    fn check_signature_after_another_signatures_attribute_reports_one_diagnostic() {
        let diagnostics = run_with_boundary_attributes(
            "#[shard]\nfn a(x: String, y: String) {}\nfn b(x: String, y: String) {}",
            &["shard"],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn check_signature_with_another_attribute_reports_group() {
        let diagnostics =
            run_with_boundary_attributes("#[inline]\nfn f(a: String, b: String) {}", &["shard"]);

        assert_diagnostic(&diagnostics[0]).message_contains("share type `String`");
    }

    #[test]
    fn check_signature_with_another_attribute_reports_one_diagnostic() {
        let diagnostics =
            run_with_boundary_attributes("#[inline]\nfn f(a: String, b: String) {}", &["shard"]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn check_signature_with_extern_function_reports_nothing() {
        let diagnostics = run("pub extern \"C\" fn f(a: usize, b: usize) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_extern_block_reports_nothing() {
        let diagnostics = run("unsafe extern \"C\" { fn f(a: usize, b: usize); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_mixed_reference_and_owned_reports_nothing() {
        let diagnostics = run("fn f(a: &str, b: String) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_mutable_and_shared_reference_reports_nothing() {
        let diagnostics = run("fn f(a: &usize, b: &mut usize) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_named_types_reports_nothing() {
        let diagnostics = run("fn f(a: UserId, b: Email) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_repeated_lifetime_reference_reports_group() {
        let diagnostics = run("fn f<'a>(a: &'a str, b: &'a str) {}");

        assert_diagnostic(&diagnostics[0]).message_contains("share type `&str`");
    }

    #[test]
    fn check_signature_with_repeated_lifetime_reference_reports_one_diagnostic() {
        let diagnostics = run("fn f<'a>(a: &'a str, b: &'a str) {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn check_signature_with_repeated_mutable_reference_reports_group() {
        let diagnostics = run("fn f(functions: &mut usize, signatures: &mut usize) {}");

        assert_diagnostic(&diagnostics[0])
            .message_contains("parameters `functions` and `signatures` share type `&mut usize`");
    }

    #[test]
    fn check_signature_with_repeated_mutable_reference_reports_one_diagnostic() {
        let diagnostics = run("fn f(functions: &mut usize, signatures: &mut usize) {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn check_signature_with_repeated_string_reports_group() {
        let source = "fn send_email(to: String, from: String) {}";

        let diagnostics = run(source);

        let start = source.find("String").expect("source contains a type");
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.repeated-primitive-params")
            .has_severity(Severity::Warn)
            .message_contains("parameters `to` and `from` share type `String`")
            .has_span("<test>", start, start + "String".len());
    }

    #[test]
    fn check_signature_with_repeated_string_reports_one_diagnostic() {
        let source = "fn send_email(to: String, from: String) {}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn check_signature_with_repeated_type_on_method_reports_group() {
        let diagnostics = run("impl S { fn f(&self, a: usize, b: usize) {} }");

        assert_diagnostic(&diagnostics[0]).message_contains("parameters `a` and `b`");
    }

    #[test]
    fn check_signature_with_repeated_type_on_method_reports_one_diagnostic() {
        let diagnostics = run("impl S { fn f(&self, a: usize, b: usize) {} }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn check_signature_with_repeated_type_on_trait_method_reports_group() {
        let diagnostics = run("trait T { fn f(&self, a: usize, b: usize); }");

        assert_diagnostic(&diagnostics[0]).message_contains("parameters `a` and `b`");
    }

    #[test]
    fn check_signature_with_repeated_type_on_trait_method_reports_one_diagnostic() {
        let diagnostics = run("trait T { fn f(&self, a: usize, b: usize); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn check_signature_with_single_primitive_reports_nothing() {
        let diagnostics = run("fn f(a: String, b: usize, c: Config) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn check_signature_with_two_repeated_types_reports_string_group_second() {
        let diagnostics = run("fn f(a: usize, b: String, c: usize, d: String) {}");

        assert_diagnostic(&diagnostics[1])
            .message_contains("parameters `b` and `d` share type `String`");
    }

    #[test]
    fn check_signature_with_two_repeated_types_reports_two_diagnostics() {
        let diagnostics = run("fn f(a: usize, b: String, c: usize, d: String) {}");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn check_signature_with_two_repeated_types_reports_usize_group_first() {
        let diagnostics = run("fn f(a: usize, b: String, c: usize, d: String) {}");

        assert_diagnostic(&diagnostics[0])
            .message_contains("parameters `a` and `c` share type `usize`");
    }

    #[test]
    fn check_signature_with_unsafe_modifier_reports_group() {
        let diagnostics = run("unsafe fn f(a: usize, b: usize) {}");

        assert_diagnostic(&diagnostics[0]).message_contains("share type `usize`");
    }

    #[test]
    fn check_signature_with_unsafe_modifier_reports_one_diagnostic() {
        let diagnostics = run("unsafe fn f(a: usize, b: usize) {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn join_names_with_one_name_returns_that_name() {
        let joined = join_names(&["a"]);

        assert_eq!(joined, "`a`");
    }

    #[test]
    fn join_names_with_three_names_returns_oxford_comma_list() {
        let joined = join_names(&["a", "b", "c"]);

        assert_eq!(joined, "`a`, `b`, and `c`");
    }

    #[test]
    fn join_names_with_two_names_returns_pair() {
        let joined = join_names(&["a", "b"]);

        assert_eq!(joined, "`a` and `b`");
    }

    #[test]
    fn join_names_with_zero_names_returns_empty() {
        let joined = join_names(&[]);

        assert_eq!(joined, "");
    }

    #[test]
    fn primitive_type_name_with_nested_reference_keeps_both_markers() {
        let diagnostics = run("fn f(a: &&str, b: &&str) {}");

        assert_diagnostic(&diagnostics[0]).message_contains("share type `&&str`");
    }

    #[test]
    fn primitive_type_name_with_nested_reference_reports_one_diagnostic() {
        let diagnostics = run("fn f(a: &&str, b: &&str) {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn primitive_type_name_with_three_repeated_params_lists_all_three() {
        let diagnostics = run("fn f(a: u32, b: u32, c: u32) {}");

        assert_diagnostic(&diagnostics[0])
            .message_contains("parameters `a`, `b`, and `c` share type `u32`");
    }

    #[test]
    fn primitive_type_name_with_three_repeated_params_reports_one_diagnostic() {
        let diagnostics = run("fn f(a: u32, b: u32, c: u32) {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn primitive_type_name_with_wrapped_primitive_reports_nothing() {
        let diagnostics = run("fn f(a: Option<usize>, b: Option<usize>) {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send_parameter_group() {
        fn assert_send<T: Send>() {}
        assert_send::<ParameterGroup<'_>>();
    }

    #[test]
    fn trait_send_repeated_primitive_params() {
        fn assert_send<T: Send>() {}
        assert_send::<RepeatedPrimitiveParams>();
    }

    #[test]
    fn trait_sync_parameter_group() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ParameterGroup<'_>>();
    }

    #[test]
    fn trait_sync_repeated_primitive_params() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<RepeatedPrimitiveParams>();
    }

    #[test]
    fn trait_unpin_parameter_group() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ParameterGroup<'_>>();
    }

    #[test]
    fn trait_unpin_repeated_primitive_params() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<RepeatedPrimitiveParams>();
    }
}
