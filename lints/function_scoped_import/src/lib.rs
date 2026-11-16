mod fix;

use stiles_rust::decorations::ImportSource;
use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, Severity};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(function_scoped_import::RULE_ID.as_str(), "lint.function-scoped-import");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.function-scoped-import");

/// Flags `use` statements inside function bodies
///
/// An import in a function body hides what the module depends on. A `cfg`
/// gate on the import or on an enclosing item silences the lint, because
/// the narrow scope is then deliberate. The lint also spares an import
/// whose qualifier resolves to an enum, because a variant import belongs
/// beside the match that uses it.
///
/// `stiles check --fix` moves every import it can of one module at once.
/// Each goes on its own line after the last import there, in source order.
/// When the module has no imports, they go before the first item. An import
/// that the module already has, or that an earlier body brings, is only
/// deleted. Every finding in the module carries this same fix.
///
/// An import keeps its finding without a fix when the move could change
/// what a name means:
///
/// - The module binds the name to something else, with an import or an
///   item. A glob import can bind any name, so a module with one gets no
///   fix. The prelude binds names such as `Result`, which get none either.
/// - The import is a glob, because the move would bring its names into the
///   whole module.
/// - The path starts with a name that the function body declares.
/// - The module has only some of the names of a `use` list.
/// - An earlier body imports the same name from another path. That import
///   moves, and this one stays.
/// - The import is `pub`, which the move would spread to the module.
/// - An attribute or a doc comment sits above the import. The move would
///   leave it behind on the next statement.
///
/// # Examples
///
/// ```text
/// // DON'T
/// fn render(out: &mut String) -> std::fmt::Result {
///     use std::fmt::Write;
///     write!(out, "done")
/// }
///
/// // DO
/// use std::fmt::Write;
///
/// fn render(out: &mut String) -> std::fmt::Result {
///     write!(out, "done")
/// }
/// ```
///
/// ```
/// use function_scoped_import::FunctionScopedImport;
/// use stiles_testing::{execute, parse};
/// use stiles_types::Language;
///
/// let tree = parse("fn f() { use std::fmt::Write; }", Language::Rust);
/// let mut passes = vec![FunctionScopedImport::into_lint_pass()];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct FunctionScopedImport;

impl FunctionScopedImport {
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
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
/// it inside. Both spellings count at every level of the walk.
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
/// A file and a module body gate everything under them with `#![cfg(..)]`.
/// Tree-sitter puts that attribute inside the `source_file` or the
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

/// Returns whether the rule reports the import
///
/// It reports an import in a block, unless the import names enum variants
/// or a `cfg` gate covers it.
fn is_flagged(node: &DecoratedNode<'_>) -> bool {
    is_inside_block(node) && !imports_enum_variants(node) && !is_cfg_gated(node)
}

impl RustLintPass for FunctionScopedImport {
    fn check_use_declaration(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        if !is_flagged(node) {
            return Vec::new();
        }

        let diagnostic = Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Error)
            .message("`use` sits inside a function body; move it to the top of the module")
            .span(node.span())
            .build();
        let fixed = self::fix::fix(node).and_then(|fix| diagnostic.clone().with_fix(fix).ok());

        vec![fixed.unwrap_or(diagnostic)]
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for FunctionScopedImport {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.function-scoped-import")]
    }

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::of::<ImportSource>()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [FunctionScopedImport]];

#[cfg(test)]
mod tests {
    use stiles_testing::{
        assert_diagnostic, assert_fix, assert_no_diagnostics, decorate, execute, parse,
    };
    use stiles_types::{DecorationMap, Language};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![FunctionScopedImport::into_lint_pass()]
    }

    /// Returns `source` after the fix of its first finding, applied once
    fn fixed_once(source: &str) -> String {
        let diagnostics = run(source);
        let fix = diagnostics[0]
            .fix()
            .expect("the first finding should carry a fix");

        let mut text = source.to_owned();
        for edit in fix.edits().iter().rev() {
            text.replace_range(edit.span().start()..edit.span().end(), edit.replacement());
        }
        text
    }

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

    fn run_documented(source: &str) -> Vec<stiles_types::Diagnostic> {
        let tree = stiles_testing::parse(source, stiles_types::Language::Rust);
        stiles_testing::execute(&tree, &mut [FunctionScopedImport::into_lint_pass()])
    }

    /// Asserts that the rule reports one import in `source` and attaches
    /// no fix to it
    fn assert_unfixed(source: &str) {
        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_diagnostic(&diagnostics[0]).has_no_fix();
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
    fn fix_collapses_the_same_import_of_two_bodies() {
        assert_fix()
            .tree(parse(
                "fn f() {\n    use a::b;\n}\n\nfn g() {\n    use a::b;\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use a::b;\n\nfn f() {\n}\n\nfn g() {\n}\n");
    }

    #[test]
    fn fix_deletes_the_blank_line_after_an_import_that_opens_the_block() {
        assert_fix()
            .tree(parse(
                "fn f() {\n    use a::b;\n\n    b();\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use a::b;\n\nfn f() {\n    b();\n}\n");
    }

    #[test]
    fn fix_is_the_same_on_every_finding_of_a_module() {
        let diagnostics = run("fn f() {\n    use a::b;\n}\n\nfn g() {\n    use c::d;\n}\n");

        assert_eq!(diagnostics[0].fix(), diagnostics[1].fix());
    }

    #[test]
    fn fix_keeps_the_blank_line_after_an_import_below_a_statement() {
        assert_fix()
            .tree(parse(
                "fn f() {\n    a();\n    use a::b;\n\n    b();\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use a::b;\n\nfn f() {\n    a();\n\n    b();\n}\n");
    }

    #[test]
    fn fix_moves_a_renamed_import() {
        assert_fix()
            .tree(parse(
                "use std::io::Write;\n\nfn f() {\n    use std::fmt::Write as FmtWrite;\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use std::io::Write;\nuse std::fmt::Write as FmtWrite;\n\nfn f() {\n}\n");
    }

    #[test]
    fn fix_moves_an_import_below_a_plain_comment() {
        assert_fix()
            .tree(parse(
                "fn f() {\n    a();\n    // note\n    use a::b;\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use a::b;\n\nfn f() {\n    a();\n    // note\n}\n");
    }

    #[test]
    fn fix_moves_an_import_out_of_an_impl_method() {
        assert_fix()
            .tree(parse(
                "use a::b;\n\nimpl S {\n    fn f(&self) {\n        use c::{self, d};\n    }\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use a::b;\nuse c::{self, d};\n\nimpl S {\n    fn f(&self) {\n    }\n}\n");
    }

    #[test]
    fn fix_moves_an_import_that_shares_its_line() {
        assert_fix()
            .tree(parse("fn f() { use a::b; }", Language::Rust))
            .passes(passes)
            .produces("use a::b;\n\nfn f() { }");
    }

    #[test]
    fn fix_moves_the_import_above_the_docs_of_the_first_item() {
        assert_fix()
            .tree(parse(
                "//! Crate\n\n/// Docs\n#[inline]\nfn f() {\n    use a::b;\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("//! Crate\n\nuse a::b;\n\n/// Docs\n#[inline]\nfn f() {\n}\n");
    }

    #[test]
    fn fix_moves_the_import_after_the_imports_of_the_module() {
        assert_fix()
            .tree(parse(
                "use std::io;\n\nfn f() {\n    use std::fmt::Write;\n    g();\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use std::io;\nuse std::fmt::Write;\n\nfn f() {\n    g();\n}\n");
    }

    #[test]
    fn fix_moves_the_import_below_a_comment_set_apart_by_a_blank_line() {
        assert_fix()
            .tree(parse(
                "// Header\n\nfn f() {\n    use a::b;\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("// Header\n\nuse a::b;\n\nfn f() {\n}\n");
    }

    #[test]
    fn fix_moves_the_import_below_a_trailing_comment() {
        assert_fix()
            .tree(parse(
                "use a::b; // why\nfn f() {\n    use c::d;\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use a::b; // why\nuse c::d;\nfn f() {\n}\n");
    }

    #[test]
    fn fix_moves_the_import_into_an_inline_module() {
        assert_fix()
            .tree(parse(
                "mod m {\n    fn f() {\n        use a::b;\n        b();\n    }\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("mod m {\n    use a::b;\n\n    fn f() {\n        b();\n    }\n}\n");
    }

    #[test]
    fn fix_moves_the_movable_imports_and_leaves_the_rest() {
        let source = "use std::io::Write;\n\nfn f() {\n    use a::b;\n}\n\nfn g() {\n    use std::fmt::Write;\n}\n\nfn h() {\n    use c::*;\n}\n";

        assert_eq!(
            fixed_once(source),
            "use std::io::Write;\nuse a::b;\n\nfn f() {\n}\n\nfn g() {\n    use std::fmt::Write;\n}\n\nfn h() {\n    use c::*;\n}\n"
        );
    }

    #[test]
    fn fix_moves_twelve_imports_in_one_round() {
        let source: String = (0..12)
            .map(|index| format!("fn f{index}() {{\n    use m{index}::g{index};\n}}\n"))
            .collect();
        let imports: String = (0..12)
            .map(|index| format!("use m{index}::g{index};\n"))
            .collect();
        let bodies: String = (0..12)
            .map(|index| format!("fn f{index}() {{\n}}\n"))
            .collect();

        assert_eq!(fixed_once(&source), format!("{imports}\n{bodies}"));
    }

    #[test]
    fn fix_moves_two_imports_in_source_order() {
        assert_fix()
            .tree(parse(
                "fn f() {\n    use a::b;\n    use c::d;\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use a::b;\nuse c::d;\n\nfn f() {\n}\n");
    }

    #[test]
    fn fix_reindents_a_use_list() {
        assert_fix()
            .tree(parse("mod m {\n    fn f() {\n        use a::{\n            b,\n            c,\n        };\n    }\n}\n", Language::Rust))
            .passes(passes)
            .produces("mod m {\n    use a::{\n        b,\n        c,\n    };\n\n    fn f() {\n    }\n}\n");
    }

    #[test]
    fn fix_with_the_same_import_in_the_module_only_deletes_it() {
        assert_fix()
            .tree(parse(
                "use std::fmt::Write;\n\nfn f() {\n    use std::fmt::Write;\n    g();\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use std::fmt::Write;\n\nfn f() {\n    g();\n}\n");
    }

    #[test]
    fn fix_with_the_same_unnamed_import_in_the_module_only_deletes_it() {
        assert_fix()
            .tree(parse(
                "use std::fmt::Write as _;\n\nfn f() {\n    use std::fmt :: Write as _;\n}\n",
                Language::Rust,
            ))
            .passes(passes)
            .produces("use std::fmt::Write as _;\n\nfn f() {\n}\n");
    }

    #[test]
    fn function_import_is_flagged_as_an_error_pointing_to_the_module_top() {
        let diagnostics = run("fn f() { use std::collections::HashMap; }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.function-scoped-import")
            .has_severity(Severity::Error)
            .message_contains("top of the module");
    }

    #[test]
    fn function_import_is_flagged_once() {
        let diagnostics = run("fn f() { use std::collections::HashMap; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn glob_import_beside_a_movable_import_has_no_fix() {
        let diagnostics = run("fn f() {\n    use a::b;\n}\n\nfn g() {\n    use c::*;\n}\n");

        assert_diagnostic(&diagnostics[1]).has_no_fix();
    }

    #[test]
    fn glob_import_has_no_fix() {
        assert_unfixed("fn f() { use std::collections::*; }");
    }

    #[test]
    fn impl_method_import_is_flagged() {
        let diagnostics = run("impl Foo { fn f(&self) { use a::b; } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn import_list_that_the_module_has_in_part_has_no_fix() {
        assert_unfixed("use a::b;\nfn f() { use a::{b, c}; }");
    }

    #[test]
    fn import_with_a_doc_comment_has_no_fix() {
        assert_unfixed("fn f() {\n    /// Docs\n    use a::b;\n}\n");
    }

    #[test]
    fn import_with_an_attribute_above_a_comment_has_no_fix() {
        assert_unfixed("fn f() {\n    #[allow(unused_imports)]\n    // note\n    use a::b;\n}\n");
    }

    #[test]
    fn import_with_an_attribute_has_no_fix() {
        assert_unfixed("fn f() { #[allow(unused_imports)] use a::b; }");
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
    fn module_with_a_glob_import_has_no_fix() {
        assert_unfixed("use super::*;\nfn f() { use a::b; }");
    }

    #[test]
    fn multiple_function_imports_are_each_flagged() {
        let diagnostics = run("fn f() { use a::b; use c::d; }");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn name_bound_by_a_gated_import_of_the_same_path_has_no_fix() {
        assert_unfixed("#[cfg(test)]\nuse a::b;\nfn f() { use a::b; }");
    }

    #[test]
    fn name_bound_to_an_item_has_no_fix() {
        assert_unfixed("struct Write;\nfn f() { use std::fmt::Write; }");
    }

    #[test]
    fn name_bound_to_another_import_has_no_fix() {
        assert_unfixed("use std::io::Write;\nfn f() { use std::fmt::Write; }");
    }

    #[test]
    fn name_bound_to_another_path_by_an_earlier_body_has_no_fix() {
        let diagnostics =
            run("fn f() {\n    use std::fmt::Write;\n}\n\nfn g() {\n    use std::io::Write;\n}\n");

        assert_diagnostic(&diagnostics[1]).has_no_fix();
    }

    #[test]
    fn name_in_the_prelude_has_no_fix() {
        assert_unfixed("fn f() { use std::fmt::Result; }");
    }

    #[test]
    fn nested_block_import_is_flagged() {
        let diagnostics = run("fn f() { { use a::b; } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn path_through_a_name_the_body_declares_has_no_fix() {
        let diagnostics = run("fn f() {\n    use std::fmt;\n    use fmt::Write;\n}\n");

        assert_diagnostic(&diagnostics[1]).has_no_fix();
    }

    #[test]
    fn path_through_an_item_of_the_body_has_no_fix() {
        assert_unfixed("fn f() { mod m { pub fn g() {} } use m::g; }");
    }

    #[cfg(feature = "plugin")]
    #[test]
    fn reads_returns_the_import_source() {
        let reads = stiles_plugin::DeclaresRules::reads(&FunctionScopedImport);

        assert_eq!(reads, stiles_plugin::Reads::of::<ImportSource>());
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

    #[test]
    fn visible_import_has_no_fix() {
        assert_unfixed("fn f() { pub use a::b; }");
    }
}
