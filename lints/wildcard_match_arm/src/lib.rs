use whisker_rust::decorations::{AdtFlags, ResolvedType};
use whisker_rust::{RuleId, RustLintPass};
use whisker_types::{DecoratedNode, Diagnostic, Severity};

/// Flags wildcard (`_`) patterns in match arms when the scrutinee is an enum
///
/// Wildcard arms hide missing cases when new variants are added to an
/// enum. Explicit matching forces you to handle each variant
/// deliberately. The lint allows wildcards on non-enum types and on
/// external `#[non_exhaustive]` enums where a wildcard is required.
pub struct WildcardMatchArm;

impl RustLintPass for WildcardMatchArm {
    fn check_match_expression(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(scrutinee) = node.child_by_field_name("value") else {
            return Vec::new();
        };

        let Some(resolved) = scrutinee.decoration::<ResolvedType>() else {
            return Vec::new();
        };
        if !resolved.is_enum() {
            return Vec::new();
        }

        if let Some(flags) = scrutinee.decoration::<AdtFlags>()
            && flags.non_exhaustive_external()
        {
            return Vec::new();
        }

        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };

        let mut diagnostics = Vec::new();

        for arm in body.named_children() {
            if arm.kind() != "match_arm" {
                continue;
            }
            let Some(pattern) = arm.child_by_field_name("pattern") else {
                continue;
            };
            if is_wildcard_pattern(&pattern) {
                diagnostics.push(Diagnostic::new(
                    RuleId::new("lint.wildcard-match-arm"),
                    Severity::Warn,
                    "wildcard match arm hides unhandled variants".into(),
                    pattern.span(),
                ));
            }
        }

        diagnostics
    }
}

/// Returns whether a match pattern node represents a wildcard `_`
///
/// Inspects all children (including anonymous nodes) of the
/// `match_pattern` node for a `_` token, which tree-sitter-rust
/// emits as an anonymous node.
fn is_wildcard_pattern(match_pattern: &DecoratedNode<'_>) -> bool {
    for i in 0..match_pattern.child_count() as u32 {
        let Some(child) = match_pattern.child(i) else {
            continue;
        };
        if !child.is_named() && child.text() == "_" {
            return true;
        }
    }
    false
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for WildcardMatchArm {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.wildcard-match-arm")]
    }
}

whisker_plugin::export_lints![rust: [WildcardMatchArm]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use whisker_types::{DecorationMap, Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(WildcardMatchArm))]
    }

    fn find_scrutinee_id(tree: &whisker_types::DecoratedTree) -> usize {
        fn walk(node: &DecoratedNode<'_>) -> Option<usize> {
            if node.kind() == "match_expression" {
                return node.child_by_field_name("value").map(|n| n.id());
            }
            for child in node.named_children() {
                if let Some(id) = walk(&child) {
                    return Some(id);
                }
            }
            None
        }
        walk(&tree.root_node()).expect("source should contain a match expression")
    }

    #[test]
    fn detect_with_enum_scrutinee_flags_wildcard_once() {
        let source = "fn f() { match x { A => {} _ => {} } }";
        let mut tree = parse(source, Language::Rust);
        let scrutinee_id = find_scrutinee_id(&tree);
        let mut map = DecorationMap::new();
        map.insert(
            scrutinee_id,
            ResolvedType::new("MyEnum".into()).with_enum(true),
        );
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn detect_with_enum_scrutinee_flags_wildcard_with_a_warning() {
        let source = "fn f() { match x { A => {} _ => {} } }";
        let mut tree = parse(source, Language::Rust);
        let scrutinee_id = find_scrutinee_id(&tree);
        let mut map = DecorationMap::new();
        map.insert(
            scrutinee_id,
            ResolvedType::new("MyEnum".into()).with_enum(true),
        );
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.wildcard-match-arm")
            .has_severity(Severity::Warn)
            .message_contains("wildcard");
    }

    #[test]
    fn detect_without_decoration_does_nothing() {
        let source = "fn f() { match x { A => {} _ => {} } }";
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn if_let_does_not_trigger_lint() {
        let source = "fn f() { if let Some(x) = opt { x } else { 0 } }";
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn no_wildcard_arm_is_not_flagged() {
        let source = "fn f() { match x { A => {} B => {} } }";
        let mut tree = parse(source, Language::Rust);
        let scrutinee_id = find_scrutinee_id(&tree);
        let mut map = DecorationMap::new();
        map.insert(
            scrutinee_id,
            ResolvedType::new("MyEnum".into()).with_enum(true),
        );
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn non_enum_type_is_not_flagged() {
        let source = "fn f() { match x { 0 => {} _ => {} } }";
        let mut tree = parse(source, Language::Rust);
        let scrutinee_id = find_scrutinee_id(&tree);
        let mut map = DecorationMap::new();
        map.insert(
            scrutinee_id,
            ResolvedType::new("i32".into()).with_enum(false),
        );
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn non_exhaustive_external_enum_is_not_flagged() {
        let source = "fn f() { match x { A => {} _ => {} } }";
        let mut tree = parse(source, Language::Rust);
        let scrutinee_id = find_scrutinee_id(&tree);
        let mut map = DecorationMap::new();
        map.insert(
            scrutinee_id,
            ResolvedType::new("ExternalEnum".into()).with_enum(true),
        );
        map.insert(scrutinee_id, AdtFlags::new(true));
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn non_exhaustive_local_enum_is_flagged() {
        let source = "fn f() { match x { A => {} _ => {} } }";
        let mut tree = parse(source, Language::Rust);
        let scrutinee_id = find_scrutinee_id(&tree);
        let mut map = DecorationMap::new();
        map.insert(
            scrutinee_id,
            ResolvedType::new("LocalEnum".into()).with_enum(true),
        );
        map.insert(scrutinee_id, AdtFlags::new(false));
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn non_exhaustive_local_enum_is_flagged_as_a_warning() {
        let source = "fn f() { match x { A => {} _ => {} } }";
        let mut tree = parse(source, Language::Rust);
        let scrutinee_id = find_scrutinee_id(&tree);
        let mut map = DecorationMap::new();
        map.insert(
            scrutinee_id,
            ResolvedType::new("LocalEnum".into()).with_enum(true),
        );
        map.insert(scrutinee_id, AdtFlags::new(false));
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.wildcard-match-arm")
            .has_severity(Severity::Warn);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<WildcardMatchArm>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<WildcardMatchArm>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<WildcardMatchArm>();
    }
}
