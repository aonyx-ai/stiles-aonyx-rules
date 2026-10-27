use whisker_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use whisker_types::{DecoratedNode, Diagnostic, LintPass, Severity};

const RULE_ID: RuleId = RuleId::new("lint.pub-field");

/// Flags `pub` fields on struct definitions
///
/// A public field lets a caller read and write state the type owns. The
/// house convention is a private field with an accessor, so the type keeps
/// control of its own invariants.
///
/// Restricted forms such as `pub(crate)` narrow the exposure on purpose, so
/// the rule leaves them alone. Enums and unions are out of scope.
pub struct PubField;

impl PubField {
    /// Creates a boxed [`LintPass`] suitable for the whisker pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = PubField::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

/// Returns whether the given node is a bare `pub` visibility modifier
///
/// A restricted form such as `pub(crate)` holds its scope as a named child,
/// so a bare `pub` is the one with no named children.
fn is_bare_pub(node: &DecoratedNode<'_>) -> bool {
    node.kind() == "visibility_modifier" && node.named_child_count() == 0
}

/// Returns a diagnostic for every public field of a named-field struct
fn named_field_diagnostics(body: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for field in body.named_children() {
        if field.kind() != "field_declaration" {
            continue;
        }
        let Some(visibility) = field.named_children().into_iter().find(is_bare_pub) else {
            continue;
        };
        let Some(name) = field.child_by_field_name("name") else {
            continue;
        };

        diagnostics.push(
            Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Warn)
                .message(format!(
                    "field `{}` is public; make it private and derive a getter with `getset`",
                    name.text()
                ))
                .span(visibility.span())
                .build(),
        );
    }

    diagnostics
}

/// Returns a diagnostic for every public field of a tuple struct
///
/// The body holds no per-field node. Each `visibility_modifier` is a flat
/// sibling of the type it qualifies. The loop counts the commas before each
/// modifier to recover the field index. A comma that belongs to a type sits
/// inside that type and never reaches the loop.
fn ordered_field_diagnostics(body: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut index = 0usize;

    for position in 0..body.child_count() as u32 {
        let Some(child) = body.child(position) else {
            continue;
        };

        if child.kind() == "," {
            index += 1;
            continue;
        }
        if !is_bare_pub(&child) {
            continue;
        }

        diagnostics.push(
            Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Warn)
                .message(format!(
                    "field {index} is public; make it private and add an accessor method"
                ))
                .span(child.span())
                .build(),
        );
    }

    diagnostics
}

impl RustLintPass for PubField {
    fn check_struct_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };

        match body.kind() {
            "field_declaration_list" => named_field_diagnostics(&body),
            "ordered_field_declaration_list" => ordered_field_diagnostics(&body),
            _ => Vec::new(),
        }
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for PubField {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.pub-field")]
    }
}

whisker_plugin::export_lints![rust: [PubField]];

#[cfg(test)]
mod tests {
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn adapt() -> Box<dyn LintPass> {
        PubField::into_lint_pass()
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![adapt()];
        execute(&tree, &mut passes)
    }

    #[test]
    fn enum_variant_fields_not_flagged() {
        let diagnostics = run("pub enum Gap { Outside { pub root: u32 }, Unreachable(pub u32) }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn named_field_diagnostic_is_reported_once() {
        let diagnostics = run("struct Config { pub verbose: u32 }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn named_field_diagnostic_spans_the_visibility_modifier() {
        let diagnostics = run("struct Config { pub verbose: u32 }");

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 16, 19);
    }

    #[test]
    fn named_pub_field_flagged_as_a_warning_naming_the_field() {
        let diagnostics = run("struct Config { pub verbose: u32 }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.pub-field")
            .has_severity(Severity::Warn)
            .message_contains("field `verbose` is public")
            .message_contains("getset");
    }

    #[test]
    fn named_pub_field_flagged_once() {
        let diagnostics = run("struct Config { pub verbose: u32 }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn named_pub_fields_each_flagged() {
        let diagnostics = run("struct Config { pub a: u32, b: u32, pub c: u32 }");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn named_pub_fields_first_diagnostic_names_a() {
        let diagnostics = run("struct Config { pub a: u32, b: u32, pub c: u32 }");

        assert_diagnostic(&diagnostics[0]).message_contains("field `a` is public");
    }

    #[test]
    fn named_pub_fields_second_diagnostic_names_c() {
        let diagnostics = run("struct Config { pub a: u32, b: u32, pub c: u32 }");

        assert_diagnostic(&diagnostics[1]).message_contains("field `c` is public");
    }

    #[test]
    fn nested_struct_in_function_flagged_naming_the_field() {
        let diagnostics = run("fn main() { struct Inner { pub a: u32 } }");

        assert_diagnostic(&diagnostics[0]).message_contains("field `a` is public");
    }

    #[test]
    fn nested_struct_in_function_flagged_once() {
        let diagnostics = run("fn main() { struct Inner { pub a: u32 } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn newtype_diagnostic_is_reported_once() {
        let diagnostics = run("pub struct ProviderName(pub &'static str);");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn newtype_diagnostic_spans_the_visibility_modifier() {
        let diagnostics = run("pub struct ProviderName(pub &'static str);");

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 24, 27);
    }

    #[test]
    fn newtype_pub_field_flagged_as_a_warning_naming_the_index() {
        let diagnostics = run("pub struct ProviderName(pub &'static str);");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.pub-field")
            .has_severity(Severity::Warn)
            .message_contains("field 0 is public")
            .message_contains("accessor method");
    }

    #[test]
    fn newtype_pub_field_flagged_once() {
        let diagnostics = run("pub struct ProviderName(pub &'static str);");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn private_named_field_not_flagged() {
        let diagnostics = run("pub struct Config { verbose: u32 }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn private_tuple_field_not_flagged() {
        let diagnostics = run("pub struct RuleId(&'static str);");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_crate_named_field_not_flagged() {
        let diagnostics = run("struct Config { pub(crate) verbose: u32 }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_crate_tuple_field_not_flagged() {
        let diagnostics = run("struct RuleId(pub(crate) &'static str);");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_function_not_flagged() {
        let diagnostics = run("pub fn foo(x: u32) -> u32 { x }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_in_path_tuple_field_not_flagged() {
        let diagnostics = run("struct RuleId(pub(in crate::rules) u32);");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_super_named_field_not_flagged() {
        let diagnostics = run("struct Config { pub(super) verbose: u32 }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn spaced_pub_crate_tuple_field_not_flagged() {
        let diagnostics = run("struct RuleId(pub (crate) u32);");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<PubField>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<PubField>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<PubField>();
    }

    #[test]
    fn tuple_field_after_attribute_flagged_as_field_zero() {
        let diagnostics = run("struct D(#[serde(default)] pub Vec<u32>,);");

        assert_diagnostic(&diagnostics[0]).message_contains("field 0 is public");
    }

    #[test]
    fn tuple_field_after_attribute_flagged_once() {
        let diagnostics = run("struct D(#[serde(default)] pub Vec<u32>,);");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn tuple_field_after_comment_flagged_as_field_zero() {
        let diagnostics = run("struct A(\n    /// the count\n    pub u32,\n    u8,\n);");

        assert_diagnostic(&diagnostics[0]).message_contains("field 0 is public");
    }

    #[test]
    fn tuple_field_after_comment_flagged_once() {
        let diagnostics = run("struct A(\n    /// the count\n    pub u32,\n    u8,\n);");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn tuple_field_with_comma_inside_type_first_diagnostic_names_field_one() {
        let diagnostics = run("struct A(u32, pub Vec<(u8, u8)>, pub fn(u8, u8));");

        assert_diagnostic(&diagnostics[0]).message_contains("field 1 is public");
    }

    #[test]
    fn tuple_field_with_comma_inside_type_flagged_twice() {
        let diagnostics = run("struct A(u32, pub Vec<(u8, u8)>, pub fn(u8, u8));");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn tuple_field_with_comma_inside_type_second_diagnostic_names_field_two() {
        let diagnostics = run("struct A(u32, pub Vec<(u8, u8)>, pub fn(u8, u8));");

        assert_diagnostic(&diagnostics[1]).message_contains("field 2 is public");
    }

    #[test]
    fn tuple_field_with_where_clause_flagged_as_field_zero() {
        let diagnostics = run("pub struct Wrap<T>(pub T) where T: Copy;");

        assert_diagnostic(&diagnostics[0]).message_contains("field 0 is public");
    }

    #[test]
    fn tuple_field_with_where_clause_flagged_once() {
        let diagnostics = run("pub struct Wrap<T>(pub T) where T: Copy;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn tuple_pub_field_before_restricted_field_flagged_as_field_zero() {
        let diagnostics = run("struct B(pub u32, String, pub(crate) i8);");

        assert_diagnostic(&diagnostics[0]).message_contains("field 0 is public");
    }

    #[test]
    fn tuple_pub_field_before_restricted_field_flagged_once() {
        let diagnostics = run("struct B(pub u32, String, pub(crate) i8);");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn tuple_pub_fields_first_diagnostic_reports_index_one() {
        let diagnostics = run("struct B(u32, pub String, pub u8);");

        assert_diagnostic(&diagnostics[0]).message_contains("field 1 is public");
    }

    #[test]
    fn tuple_pub_fields_flagged_twice() {
        let diagnostics = run("struct B(u32, pub String, pub u8);");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn tuple_pub_fields_second_diagnostic_reports_index_two() {
        let diagnostics = run("struct B(u32, pub String, pub u8);");

        assert_diagnostic(&diagnostics[1]).message_contains("field 2 is public");
    }

    #[test]
    fn tuple_struct_with_private_fields_not_flagged() {
        let diagnostics = run("pub struct Span(usize, usize);");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn union_pub_field_not_flagged() {
        let diagnostics = run("pub union Raw { pub bits: u32 }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn unit_struct_not_flagged() {
        let diagnostics = run("pub struct Marker;");

        assert_no_diagnostics(&diagnostics);
    }
}
