use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, Severity};

const RULE_ID: RuleId = RuleId::new("lint.unprefixed-submodule-path");

/// The items in a block that bind a name in the namespace of modules
///
/// A path's first segment resolves among modules, types, and crates, so only
/// these items can hide a submodule from a path inside the block. A function,
/// a `const`, or a `static` binds a value and hides nothing.
const TYPE_NAMESPACE_ITEMS: [&str; 6] = [
    "mod_item",
    "struct_item",
    "enum_item",
    "union_item",
    "trait_item",
    "type_item",
];

/// Flags a path that reaches into a submodule without a `self::` prefix
///
/// A bare first segment is how a path names a crate. When `mod error;` sits
/// in the same module, `error::Kind` reads like a dependency, and the reader
/// has to find the declaration to tell the two apart. `self::error::Kind`
/// says it at the path.
///
/// The rule collects the `mod` declarations of one module and reports a path
/// whose first segment names one of them. That covers `use` trees and every
/// other path: calls, types, patterns, struct expressions, and the path of a
/// macro invocation. Inside a `use` tree, only a segment that starts the
/// path counts. A segment inside the braces of `self::{..}` or
/// `crate::{..}` continues a path that already has its prefix. The rule
/// skips macro bodies, because a token tree holds no paths it can read.
///
/// A module's declarations count only in that module. An inline module
/// starts again from its own `mod` items and sees none of its parent's. A
/// block sees the declarations of its module, as Rust resolves it. A type, a
/// module, or a `use` in the block that binds the name again hides the
/// submodule. A type parameter of an enclosing item hides it too. `self::`
/// would change what such a path names, so the rule stays silent. A glob
/// import in a block may bring any name, so the rule reports nothing in that
/// block. A `use` in the module itself never hides a submodule. Two bindings
/// of one name among modules and types do not compile, and `self::` reaches
/// an import as well as a declaration.
///
/// # Examples
///
/// ```ignore
/// mod error;
///
/// pub use error::ParseNameError;
/// ```
///
/// The rule reports `error`. `pub use self::error::ParseNameError;` names
/// the submodule at the path.
pub struct UnprefixedSubmodulePath;

impl UnprefixedSubmodulePath {
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = UnprefixedSubmodulePath::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

impl RustLintPass for UnprefixedSubmodulePath {
    fn check_source_file(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let submodules = declared_submodules(node);
        let mut diagnostics = Vec::new();
        walk_children(node, &submodules, &mut diagnostics);
        diagnostics
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for UnprefixedSubmodulePath {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

stiles_plugin::export_lints![rust: [UnprefixedSubmodulePath]];

/// Reports the unprefixed submodule paths under one node
///
/// `submodules` holds the names that a path may not start with at this
/// node. Each scope the walk enters narrows or replaces it.
fn walk<'a>(node: &DecoratedNode<'a>, submodules: &[&'a str], diagnostics: &mut Vec<Diagnostic>) {
    match node.kind() {
        "token_tree" | "macro_definition" => {}
        "mod_item" => {
            if let Some(body) = node.child_by_field_name("body") {
                walk_children(&body, &declared_submodules(&body), diagnostics);
            }
        }
        "block" => {
            let visible = match block_bindings(node) {
                Some(names) => without(submodules, &names),
                None => Vec::new(),
            };
            walk_children(node, &visible, diagnostics);
        }
        "identifier" => {
            if let Some(diagnostic) = check_identifier(node, submodules) {
                diagnostics.push(diagnostic);
            }
        }
        _ => match node.child_by_field_name("type_parameters") {
            Some(parameters) => {
                let visible = without(submodules, &generic_names(&parameters));
                walk_children(node, &visible, diagnostics);
            }
            None => walk_children(node, submodules, diagnostics),
        },
    }
}

/// Walks every named child of a node with one set of submodules
fn walk_children<'a>(
    node: &DecoratedNode<'a>,
    submodules: &[&'a str],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for child in node.named_children() {
        walk(&child, submodules, diagnostics);
    }
}

/// Returns a diagnostic when the identifier starts a path into a submodule
fn check_identifier(identifier: &DecoratedNode<'_>, submodules: &[&str]) -> Option<Diagnostic> {
    if !submodules.contains(&unraw(identifier.text())) {
        return None;
    }
    if !starts_path(identifier) || continues_use_prefix(identifier) {
        return None;
    }

    Some(
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Warn)
            .message(format!(
                "`{name}` names a submodule, so the path reads like a crate; write `self::{name}`",
                name = identifier.text(),
            ))
            .span(identifier.span())
            .build(),
    )
}

/// Returns whether the identifier is the first segment of a path
///
/// In an expression or a type, a lone identifier is a name in scope and
/// not a path into a module. Only the `path` side of a scoped path counts
/// there. A `use` tree is all paths, so a lone identifier there counts too.
fn starts_path(identifier: &DecoratedNode<'_>) -> bool {
    let Some(parent) = identifier.parent() else {
        return false;
    };
    match parent.kind() {
        "scoped_identifier" | "scoped_type_identifier" | "scoped_use_list" | "use_as_clause" => {
            parent
                .child_by_field_name("path")
                .is_some_and(|path| path.id() == identifier.id())
        }
        "use_wildcard" | "use_list" | "use_declaration" => true,
        _ => false,
    }
}

/// Returns whether a path in a `use` tree continues the prefix of a list
///
/// In `use self::{error::Kind}`, `error` follows `self::`, so the path
/// already has its prefix. A list after a bare `::` names crates, and a
/// list with no prefix at all, as in `use {error::Kind}`, starts each of
/// its paths afresh. The walk climbs the `use` tree until it reaches the
/// declaration or leaves the tree.
fn continues_use_prefix(identifier: &DecoratedNode<'_>) -> bool {
    let mut current = *identifier;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "use_list" => {
                if parent
                    .parent()
                    .is_some_and(|owner| owner.kind() == "scoped_use_list")
                {
                    return true;
                }
            }
            "scoped_identifier" | "scoped_use_list" | "use_as_clause" | "use_wildcard" => {}
            _ => return false,
        }
        current = parent;
    }
    false
}

/// Returns the names of the submodules a module body declares
///
/// The body is a `source_file` or the `declaration_list` of an inline
/// module. Only its own items count, so a `mod` inside a function or a
/// nested module stays out.
fn declared_submodules<'a>(body: &DecoratedNode<'a>) -> Vec<&'a str> {
    body.named_children()
        .iter()
        .filter(|item| item.kind() == "mod_item")
        .filter_map(|item| item.child_by_field_name("name"))
        .map(|name| unraw(name.text()))
        .collect()
}

/// Returns the names the items of one block bind among modules and types
///
/// Rust resolves the items of a block before the items of its module.
/// An item counts wherever it sits in the block, so it hides a submodule of
/// the same name from every path in the block. Returns [`None`] when the
/// block holds a glob import. The names a glob binds are not in the source,
/// and any of them may hide a submodule.
fn block_bindings<'a>(block: &DecoratedNode<'a>) -> Option<Vec<&'a str>> {
    let mut names = Vec::new();
    for item in block.named_children() {
        match item.kind() {
            "use_declaration" => {
                let Some(argument) = item.child_by_field_name("argument") else {
                    continue;
                };
                if holds_glob(&argument) {
                    return None;
                }
                collect_use_bindings(&argument, &mut names);
            }
            "extern_crate_declaration" => {
                let name = item
                    .child_by_field_name("alias")
                    .or_else(|| item.child_by_field_name("name"));
                names.extend(name.map(|name| unraw(name.text())));
            }
            kind if TYPE_NAMESPACE_ITEMS.contains(&kind) => {
                let name = item.child_by_field_name("name");
                names.extend(name.map(|name| unraw(name.text())));
            }
            _ => {}
        }
    }
    Some(names)
}

/// Returns whether a `use` tree holds a glob anywhere in it
fn holds_glob(tree: &DecoratedNode<'_>) -> bool {
    tree.kind() == "use_wildcard" || tree.named_children().iter().any(holds_glob)
}

/// Collects the names one `use` tree binds
///
/// A path binds its last segment, a rename binds its alias, and `self` in a
/// list binds the last segment of the list's prefix.
fn collect_use_bindings<'a>(tree: &DecoratedNode<'a>, names: &mut Vec<&'a str>) {
    match tree.kind() {
        "identifier" | "scoped_identifier" => names.extend(last_segment(tree)),
        "use_as_clause" => {
            let alias = tree.child_by_field_name("alias");
            names.extend(alias.map(|alias| unraw(alias.text())));
        }
        "use_list" => {
            for child in tree.named_children() {
                collect_use_bindings(&child, names);
            }
        }
        "scoped_use_list" => {
            let Some(list) = tree.child_by_field_name("list") else {
                return;
            };
            for child in list.named_children() {
                match child.kind() {
                    "self" => {
                        let path = tree.child_by_field_name("path");
                        names.extend(path.as_ref().and_then(last_segment));
                    }
                    _ => collect_use_bindings(&child, names),
                }
            }
        }
        _ => {}
    }
}

/// Returns the last segment of a path, which is the name a `use` binds
fn last_segment<'a>(path: &DecoratedNode<'a>) -> Option<&'a str> {
    match path.kind() {
        "identifier" => Some(unraw(path.text())),
        "scoped_identifier" => path
            .child_by_field_name("name")
            .map(|name| unraw(name.text())),
        _ => None,
    }
}

/// Returns the names of the type parameters in a parameter list
///
/// A type parameter binds a name where modules live. A lifetime cannot start
/// a path, and a const parameter binds a value, so neither hides anything.
fn generic_names<'a>(parameters: &DecoratedNode<'a>) -> Vec<&'a str> {
    parameters
        .named_children()
        .iter()
        .filter(|parameter| parameter.kind() == "type_parameter")
        .filter_map(|parameter| parameter.child_by_field_name("name"))
        .map(|name| unraw(name.text()))
        .collect()
}

/// Returns the submodules that remain once a scope binds `hidden`
fn without<'a>(submodules: &[&'a str], hidden: &[&str]) -> Vec<&'a str> {
    submodules
        .iter()
        .copied()
        .filter(|name| !hidden.contains(name))
        .collect()
}

/// Returns an identifier without its `r#` prefix
///
/// `r#match` and `match` name the same item, so a raw declaration and a
/// plain path, or the other way round, still match.
fn unraw(identifier: &str) -> &str {
    identifier.strip_prefix("r#").unwrap_or(identifier)
}

#[cfg(test)]
mod tests {
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::Language;

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![UnprefixedSubmodulePath::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    /// Returns the source text under each diagnostic, in order
    fn flagged(source: &str) -> Vec<&str> {
        run(source)
            .iter()
            .map(|diagnostic| &source[diagnostic.span().start()..diagnostic.span().end()])
            .collect()
    }

    #[test]
    fn associated_function_through_a_submodule_is_flagged() {
        let source = "mod error;\nfn f() { error::Kind::new(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn attribute_arguments_are_not_flagged() {
        let source = "mod error;\n#[derive(error::Derive)]\nstruct S;";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn attribute_path_is_flagged() {
        let source = "mod attrs;\n#[attrs::marker]\nfn f() {}";

        assert_eq!(flagged(source), ["attrs"]);
    }

    #[test]
    fn block_enum_hides_the_submodule() {
        let source = "mod error;\nfn f() { enum error { A } error::A; }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn block_extern_crate_alias_hides_the_submodule() {
        let source = "mod error;\nfn f() { extern crate other as error; error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    /// A function binds a value, and a path's first segment never names one
    #[test]
    fn block_function_does_not_hide_the_submodule() {
        let source = "mod error;\nfn f() { fn error() {} error::x(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn block_glob_import_leaves_a_sibling_block_alone() {
        let source = "mod error;\nfn f() { use other::*; }\nfn g() { error::x(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn block_glob_import_silences_the_block() {
        let source = "mod error;\nfn f() { use other::*; error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    /// A block's items count wherever they sit in the block
    #[test]
    fn block_import_after_the_path_hides_the_submodule() {
        let source = "mod error;\nfn f() { error::x(); use other::error; }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn block_import_hides_the_submodule() {
        let source = "mod error;\nfn f() { use other::error; error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn block_import_hides_the_submodule_in_a_nested_block() {
        let source = "mod error;\nfn f() { use other::error; if true { error::x(); } }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn block_import_in_a_list_hides_the_submodule() {
        let source = "mod error;\nfn f() { use other::{error, Thing}; error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn block_import_of_another_name_does_not_hide_the_submodule() {
        let source = "mod error;\nfn f() { use other::Thing; error::x(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn block_import_of_self_in_a_list_hides_the_submodule() {
        let source = "mod error;\nfn f() { use other::error::{self, Kind}; error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn block_import_renamed_away_does_not_hide_the_submodule() {
        let source = "mod error;\nfn f() { use other::error as failure; error::x(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn block_import_renamed_to_the_name_hides_the_submodule() {
        let source = "mod error;\nfn f() { use other::failure as error; error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn block_import_stays_inside_its_block() {
        let source = "mod error;\nfn f() { { use other::error; } error::x(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn block_module_hides_the_submodule() {
        let source = "mod error;\nfn f() { mod error { pub fn x() {} } error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    /// `self::inner` would name the enclosing module's item, not the block's
    #[test]
    fn block_module_is_not_a_submodule_of_the_block() {
        let source = "fn f() { mod inner { pub fn x() {} } inner::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn block_struct_hides_the_submodule() {
        let source = "mod error;\nfn f() { struct error; error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn call_through_a_submodule_is_flagged() {
        let source = "mod text;\nfn f() { text::render(); }";

        assert_eq!(flagged(source), ["text"]);
    }

    #[test]
    fn closure_body_path_is_flagged() {
        let source = "mod text;\nfn f() { let g = || text::render(); }";

        assert_eq!(flagged(source), ["text"]);
    }

    /// A const parameter binds a value, and a path's first segment never names one
    #[test]
    fn const_generic_parameter_does_not_hide_the_submodule() {
        let source = "mod error;\nfn f<const error: usize>() { error::x(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn crate_prefix_is_not_flagged() {
        let source = "mod error;\nuse crate::error::Kind;\nfn f() { crate::error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn diagnostic_is_reported_once() {
        let diagnostics = run("mod error;\nuse error::Kind;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn diagnostic_names_the_fix() {
        let diagnostics = run("mod error;\nuse error::Kind;");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.unprefixed-submodule-path")
            .has_severity(Severity::Warn)
            .message_contains("`error` names a submodule")
            .message_contains("write `self::error`");
    }

    #[test]
    fn enum_variant_pattern_is_flagged() {
        let source = "mod error;\nfn f(k: K) { match k { error::Kind::A => {} _ => {} } }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn generic_argument_is_flagged() {
        let source = "mod error;\nstruct S { kinds: Vec<error::Kind> }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn generic_parameter_hides_the_submodule() {
        let source = "mod error;\nfn f<error: Tr>() { error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn generic_parameter_of_an_impl_hides_the_submodule() {
        let source = "mod error;\nimpl<error: Tr> S<error> { fn f() -> error::Out { todo!() } }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn generic_parameter_stays_on_its_item() {
        let source = "mod error;\nfn f<error: Tr>() {}\nfn g() { error::x(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn generic_type_through_a_submodule_is_flagged() {
        let source = "mod error;\nfn f(w: error::Wrapper<u8>) {}";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn impl_method_body_path_is_flagged() {
        let source = "mod text;\nimpl S { fn f(&self) { text::render(self); } }";

        assert_eq!(flagged(source), ["text"]);
    }

    #[test]
    fn impl_trait_path_is_flagged() {
        let source = "mod text;\nimpl text::Render for S {}";

        assert_eq!(flagged(source), ["text"]);
    }

    #[test]
    fn import_glob_is_flagged() {
        let source = "mod error;\nuse error::*;";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn import_group_inside_a_crate_prefix_is_not_flagged() {
        let source = "mod error;\nuse crate::{error::Kind, text::*};";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn import_group_inside_a_leading_colon_pair_is_not_flagged() {
        let source = "mod error;\nuse ::{error::Kind};";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn import_group_inside_a_self_prefix_is_not_flagged() {
        let source = "mod error;\nuse self::{error::{Kind, Reason}, text as t};";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn import_group_is_flagged_once() {
        let source = "mod error;\nuse error::{Kind, Reason};";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn import_group_nested_in_a_submodule_is_flagged_once() {
        let source = "mod error;\nuse error::{kind::{A, B}, Reason};";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn import_group_without_a_prefix_is_flagged() {
        let source = "mod error;\nmod text;\nuse {error::Kind, std::fmt, text::*, {text as t}};";

        assert_eq!(flagged(source), ["error", "text", "text"]);
    }

    #[test]
    fn import_of_a_submodule_is_flagged() {
        let source = "mod error;\nuse error::ParseNameError;";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn import_of_the_submodule_itself_renamed_is_flagged() {
        let source = "mod error;\nuse error as failure;";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn import_renamed_is_flagged() {
        let source = "mod error;\nuse error::Kind as ErrorKind;";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn import_with_a_leading_colon_pair_is_not_flagged() {
        let source = "mod error;\nuse ::error::Kind;";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn inline_module_checks_its_own_declarations() {
        let source = r#"
mod tests {
    mod helpers {
        pub fn assert_auto<T>() {}
    }

    fn trait_auto() {
        helpers::assert_auto::<Foo>();
    }
}
"#;

        assert_eq!(flagged(source), ["helpers"]);
    }

    #[test]
    fn inline_module_declaration_is_a_submodule() {
        let source = "mod text { pub fn render() {} }\nfn f() { text::render(); }";

        assert_eq!(flagged(source), ["text"]);
    }

    #[test]
    fn inline_module_declarations_stay_inside() {
        let source = "mod inner { mod error {} }\nuse error::Kind;";

        assert_no_diagnostics(&run(source));
    }

    /// Inside an inline module, a bare `error` names a crate, not the parent's
    #[test]
    fn inline_module_does_not_see_the_parent_declarations() {
        let source = "mod error;\nmod inner { use error::Kind; fn f() { error::x(); } }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn inline_module_in_a_function_checks_its_own_declarations() {
        let source = "fn f() { mod outer { mod error {} use error::Kind; } }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn last_segment_named_like_a_submodule_is_not_flagged() {
        let source = "mod error;\nuse std::error::Error;\nfn f() -> std::error::Kind {}";

        assert_no_diagnostics(&run(source));
    }

    /// A local binds a value, and a path's first segment never names one
    #[test]
    fn local_variable_does_not_hide_the_submodule() {
        let source = "mod error;\nfn f(error: E) { let error = 1; error::x(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn lone_identifier_named_like_a_submodule_is_not_flagged() {
        let source = "mod text;\nfn f(text: &str) { let x = text; g(text); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn macro_body_is_not_flagged() {
        let source = "mod error;\nfn f() { println!(\"{}\", error::x()); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn macro_definition_is_not_flagged() {
        let source = "mod error;\nmacro_rules! m { () => { error::x() }; }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn macro_invocation_path_is_flagged() {
        let source = "mod error;\nfn f() { error::bail!(\"oops\"); }";

        assert_eq!(flagged(source), ["error"]);
    }

    /// The import binds a function, so `parse::` still starts at the submodule
    #[test]
    fn module_import_of_the_same_name_does_not_hide_the_submodule() {
        let source = "mod parse;\npub use parse::parse;\nfn f() { parse::Error::new(); }";

        assert_eq!(flagged(source), ["parse", "parse"]);
    }

    #[test]
    fn multiple_paths_are_each_flagged() {
        let source =
            "mod error;\nmod text;\nuse error::Kind;\nfn f() { text::render(); error::x(); }";

        assert_eq!(flagged(source), ["error", "text", "error"]);
    }

    #[test]
    fn nested_function_path_is_flagged() {
        let source = "mod text;\nfn f() { fn g() { text::render(); } }";

        assert_eq!(flagged(source), ["text"]);
    }

    #[test]
    fn path_without_a_declared_submodule_is_not_flagged() {
        let source = "use serde::Deserialize;\nfn f() { serde_json::to_string(&1); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn public_import_is_flagged() {
        let source = "pub mod error;\npub use error::ParseNameError;";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn qualified_path_is_flagged() {
        let source = "mod error;\nfn f() -> <error::Kind as Tr>::Out { todo!() }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn raw_declaration_matches_a_plain_path() {
        let source = "mod r#error;\nuse error::Kind;";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn raw_path_matches_a_raw_declaration() {
        let source = "mod r#match;\nuse r#match::Arm;";

        assert_eq!(flagged(source), ["r#match"]);
    }

    #[test]
    fn raw_path_message_keeps_the_prefix() {
        let diagnostics = run("mod r#match;\nuse r#match::Arm;");

        assert_diagnostic(&diagnostics[0]).message_contains("write `self::r#match`");
    }

    #[test]
    fn raw_path_with_raw_declaration_is_flagged_once() {
        let diagnostics = run("mod r#match;\nuse r#match::Arm;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn return_type_is_flagged() {
        let source = "mod error;\nfn f() -> Result<(), error::Kind> { Ok(()) }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn self_prefix_is_not_flagged() {
        let source = "mod error;\npub use self::error::Kind;\nfn f() { self::error::x(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn self_type_prefix_is_not_flagged() {
        let source = "mod error;\nimpl S { fn f() -> Self::Out { Self::make() } }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn struct_expression_is_flagged() {
        let source = "mod error;\nfn f() { let e = error::Kind { code: 1 }; }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn struct_pattern_is_flagged() {
        let source = "mod error;\nfn f(e: E) { let error::Kind { code } = e; }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn super_prefix_is_not_flagged() {
        let source = "mod error;\nmod inner { use super::error::Kind; }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn trait_bound_is_flagged() {
        let source = "mod text;\nfn f<T: text::Render>(t: T) {}";

        assert_eq!(flagged(source), ["text"]);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<UnprefixedSubmodulePath>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<UnprefixedSubmodulePath>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<UnprefixedSubmodulePath>();
    }

    #[test]
    fn tuple_struct_pattern_is_flagged() {
        let source = "mod error;\nfn f(e: E) { if let error::Kind::Code(c) = e {} }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn turbofish_call_is_flagged() {
        let source = "mod error;\nfn f() { error::make::<u8>(); }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn type_alias_is_flagged() {
        let source = "mod error;\ntype Kind = error::Kind;";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn unit_struct_path_is_flagged() {
        let source = "mod error;\nfn f() -> error::Unit { error::Unit }";

        assert_eq!(flagged(source), ["error", "error"]);
    }
}
