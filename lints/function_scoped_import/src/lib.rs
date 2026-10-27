use whisker_rust::decorations::ImportSource;
use whisker_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use whisker_types::{DecoratedNode, Diagnostic, LintPass, Severity};

const RULE_ID: RuleId = RuleId::new("lint.function-scoped-import");

/// Flags `use` statements inside function bodies
///
/// An import in a function body hides what the module depends on. A `cfg`
/// gate on the import or on an enclosing item silences the lint, because
/// the narrow scope is then deliberate. The lint also spares an import
/// whose qualifier resolves to an enum, because a variant import belongs
/// beside the match that uses it.
pub struct FunctionScopedImport;

impl FunctionScopedImport {
    /// Creates a boxed [`LintPass`] suitable for the whisker pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = FunctionScopedImport::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

/// Returns whether the node sits in a block rather than at module level
///
/// The walk stops at the nearest scope, so a `use` at the top of a module
/// nested in a function counts as module level.
fn is_inside_block(node: &DecoratedNode<'_>) -> bool {
    let mut current = node.parent();
    loop {
        let Some(ancestor) = current else {
            return false;
        };
        match ancestor.kind() {
            "block" => return true,
            "declaration_list" | "source_file" => return false,
            _ => current = ancestor.parent(),
        }
    }
}

/// Returns whether the node or an enclosing item carries a `cfg` attribute
///
/// An item writes its gate in front of itself, and a module or a file writes
/// it inside, so both spellings count at every level of the walk.
fn is_cfg_gated(node: &DecoratedNode<'_>) -> bool {
    let mut current = Some(*node);
    loop {
        let Some(item) = current else {
            return false;
        };
        if has_cfg_attribute(&item) || has_inner_cfg_attribute(&item) {
            return true;
        }
        current = item.parent();
    }
}

/// Returns whether a `cfg` attribute precedes the node
///
/// Tree-sitter makes each attribute a sibling of the item it decorates.
/// The search reads backward from the node over attributes and comments.
/// It stops at the first sibling that is neither.
fn has_cfg_attribute(node: &DecoratedNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let siblings = parent.named_children();
    let Some(index) = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())
    else {
        return false;
    };

    for sibling in siblings[..index].iter().rev() {
        match sibling.kind() {
            "attribute_item" => {
                if is_cfg_attribute(sibling) {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
    }
    false
}

/// Returns whether a `cfg` attribute opens the node's own children
///
/// A file and a module body gate everything under them with `#![cfg(..)]`,
/// which tree-sitter puts inside the `source_file` or the
/// `declaration_list` rather than in front of the item. The search stops at
/// the first child that is neither an inner attribute nor a comment.
fn has_inner_cfg_attribute(node: &DecoratedNode<'_>) -> bool {
    for child in node.named_children() {
        match child.kind() {
            "inner_attribute_item" => {
                if is_cfg_attribute(&child) {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
    }
    false
}

/// Returns whether an attribute item names `cfg`
///
/// An `attribute_item` and an `inner_attribute_item` both hold the
/// attribute as their first named child, so one check serves either.
/// `cfg_attr` does not count, because it applies another attribute instead
/// of removing the item.
fn is_cfg_attribute(node: &DecoratedNode<'_>) -> bool {
    let Some(attribute) = node.named_child(0) else {
        return false;
    };
    let Some(path) = attribute.named_child(0) else {
        return false;
    };
    path.kind() == "identifier" && path.text() == "cfg"
}

/// Returns whether the import names variants of an enum
///
/// Only the provider's resolution of the qualifier answers this; a
/// capitalized qualifier proves nothing on its own. An undecorated node
/// therefore keeps the diagnostic, because the exemption needs proof.
fn imports_enum_variants(node: &DecoratedNode<'_>) -> bool {
    match node.decoration::<ImportSource>() {
        Some(ImportSource::Enum) => true,
        Some(ImportSource::Module) => false,
        Some(ImportSource::Other) => false,
        Some(ImportSource::Unresolved) => false,
        None => false,
    }
}

impl RustLintPass for FunctionScopedImport {
    fn check_use_declaration(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        if !is_inside_block(node) {
            return Vec::new();
        }

        if imports_enum_variants(node) {
            return Vec::new();
        }
        if is_cfg_gated(node) {
            return Vec::new();
        }

        vec![Diagnostic::new(
            RULE_ID,
            Severity::Warn,
            "`use` sits inside a function body; move it to the top of the module".into(),
            node.span(),
        )]
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for FunctionScopedImport {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.function-scoped-import")]
    }
}

whisker_plugin::export_lints![rust: [FunctionScopedImport]];

#[cfg(test)]
mod tests {
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use whisker_types::{DecorationMap, Language};

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![FunctionScopedImport::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    /// Runs the rule with `import_source` on every `use` in `source`
    ///
    /// The rule reads a decoration the Rust provider makes, and these tests
    /// run no provider, so they attach it by hand.
    fn run_with(source: &str, import_source: ImportSource) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        let mut ids = Vec::new();
        collect_use_declaration_ids(&tree.root_node(), &mut ids);

        let mut map = DecorationMap::new();
        for id in ids {
            map.insert(id, import_source);
        }
        decorate(&mut tree, map);

        let mut passes = vec![FunctionScopedImport::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    fn collect_use_declaration_ids(node: &DecoratedNode<'_>, ids: &mut Vec<usize>) {
        if node.kind() == "use_declaration" {
            ids.push(node.id());
        }
        for child in node.named_children() {
            collect_use_declaration_ids(&child, ids);
        }
    }

    /// A struct or a trait qualifier names no variant
    ///
    /// The qualifier is capitalized like an enum, and the resolution says
    /// otherwise.
    #[test]
    fn associated_function_import_is_flagged() {
        let diagnostics = run_with("fn f() { use Type::associated_fn; }", ImportSource::Other);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_attr_on_function_is_flagged() {
        let diagnostics = run("#[cfg_attr(test, allow(dead_code))]\nfn f() { use a::b; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_gate_after_other_attribute_is_not_flagged() {
        let diagnostics = run("#[allow(dead_code)]\n#[cfg(test)]\nfn f() { use a::b; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_gate_behind_doc_comment_is_not_flagged() {
        let diagnostics = run("#[cfg(test)]\n/// Helps\nfn f() { use a::b; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_gate_on_earlier_item_is_flagged() {
        let diagnostics = run("#[cfg(test)]\nstruct A;\nfn f() { use a::b; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_gated_function_is_not_flagged() {
        let diagnostics = run("#[cfg(test)]\nfn f() { use crate::test_utils::mock_client; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_gated_import_is_not_flagged() {
        let diagnostics = run("fn f() { #[cfg(unix)] use std::os::unix::fs::PermissionsExt; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_gated_module_is_not_flagged() {
        let diagnostics = run("#[cfg(test)]\nmod tests { fn f() { use crate::a::b; } }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn closure_body_import_is_flagged() {
        let diagnostics = run("fn f() { let g = || { use a::b; }; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn enum_variant_glob_import_is_not_flagged() {
        let diagnostics = run_with(
            "fn f() { use Message::*; match m { Ping => {} } }",
            ImportSource::Enum,
        );

        assert_no_diagnostics(&diagnostics);
    }

    /// An import with no [`ImportSource`] at all is still reported
    ///
    /// The exemption needs proof from the provider, and an undecorated node
    /// carries none.
    #[test]
    fn enum_variant_import_without_a_decoration_is_flagged() {
        let diagnostics = run("fn f() { use crate::Message::{Ping, Pong}; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn enum_variant_list_import_is_not_flagged() {
        let diagnostics = run_with(
            "fn f() { use crate::Message::{Ping, Pong}; }",
            ImportSource::Enum,
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn enum_variant_single_import_is_not_flagged() {
        let diagnostics = run_with("fn f() { use self::Message::Ping; }", ImportSource::Enum);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn function_import_is_flagged_as_a_warning_pointing_to_the_module_top() {
        let diagnostics = run("fn f() { use std::collections::HashMap; }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.function-scoped-import")
            .has_severity(Severity::Warn)
            .message_contains("top of the module");
    }

    #[test]
    fn function_import_is_flagged_once() {
        let diagnostics = run("fn f() { use std::collections::HashMap; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn impl_method_import_is_flagged() {
        let diagnostics = run("impl Foo { fn f(&self) { use a::b; } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inner_allow_on_module_is_flagged() {
        let diagnostics = run("mod m { #![allow(dead_code)]\nfn f() { use a::b; } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inner_cfg_gate_on_file_is_not_flagged() {
        let diagnostics = run("#![cfg(feature = \"extra\")]\nfn f() { use a::b; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_cfg_gate_on_module_is_not_flagged() {
        let diagnostics = run("mod m { #![cfg(test)]\nfn f() { use a::b; } }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn module_glob_import_in_function_is_flagged() {
        let diagnostics = run("fn f() { use std::collections::*; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn module_level_import_is_not_flagged() {
        let diagnostics = run("use std::collections::HashMap;\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn module_nested_in_function_is_not_flagged() {
        let diagnostics = run("fn f() { mod inner { use std::collections::HashMap; } }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn multiple_function_imports_are_each_flagged() {
        let diagnostics = run("fn f() { use a::b; use c::d; }");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn nested_block_import_is_flagged() {
        let diagnostics = run("fn f() { { use a::b; } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn renamed_import_in_function_is_flagged() {
        let diagnostics = run("fn f() { use std::fmt::Write as _; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn single_segment_import_is_flagged() {
        let diagnostics = run("fn f() { use serde; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn super_glob_import_in_function_is_flagged() {
        let diagnostics = run("fn f() { use super::*; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn test_module_import_is_not_flagged() {
        let diagnostics = run("#[cfg(test)]\nmod tests { use super::*; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_method_import_is_flagged() {
        let diagnostics = run("trait T { fn f(&self) { use a::b; } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<FunctionScopedImport>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FunctionScopedImport>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<FunctionScopedImport>();
    }

    #[test]
    fn unresolved_import_is_flagged() {
        let diagnostics = run_with("fn f() { use nowhere::thing; }", ImportSource::Unresolved);

        assert_eq!(diagnostics.len(), 1);
    }

    /// A qualifier spelled like a type does not earn the exemption
    ///
    /// Only the resolution tells this apart from a variant import.
    #[test]
    fn uppercase_module_import_is_flagged() {
        let diagnostics = run_with("fn f() { use Shapes::draw; }", ImportSource::Module);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn use_list_without_path_is_flagged() {
        let diagnostics = run("fn f() { use {a::b, c::d}; }");

        assert_eq!(diagnostics.len(), 1);
    }
}
