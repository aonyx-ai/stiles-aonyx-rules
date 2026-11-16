use stiles_rust::decorations::{Edition, Target};
use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, Edit, Fix, LintPass, Severity, Span};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(unprefixed_submodule_path::RULE_ID.as_str(), "lint.unprefixed-submodule-path");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.unprefixed-submodule-path");

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
/// The edition 2015 starts a `use` path at the crate root, not at the
/// module. The rule reads the edition from the `Target` decoration, and
/// stays silent on a `use` path of a 2015 crate.
///
/// `stiles check --fix` inserts `self::` before the segment. Outside a `use`
/// tree, `self::` means the same in every edition. Inside one, the rule
/// attaches the fix only when the decoration proves the edition 2018 or later.
///
/// # Examples
///
/// ```text
/// // DON'T
/// mod error;
///
/// pub use error::ParseNameError;
///
/// // DO
/// mod error;
///
/// pub use self::error::ParseNameError;
/// ```
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

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::of::<Target>()
    }
}

#[cfg(feature = "plugin")]
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
    let outcome = outcome(placement(identifier), edition(identifier));

    let diagnostic = Diagnostic::builder()
        .rule_id(RULE_ID)
        .severity(Severity::Error)
        .message(format!(
            "`{name}` names a submodule, so the path reads like a crate; write `self::{name}`",
            name = identifier.text(),
        ))
        .span(identifier.span())
        .build();
    match outcome {
        Outcome::Fixed => {}
        Outcome::Silent => return None,
        Outcome::Unfixed => return Some(diagnostic),
    }

    let start = identifier.span().start();
    let span = Span::builder()
        .file(identifier.span().file_path().clone())
        .start(start)
        .end(start)
        .build();
    let insertion = Edit::builder().span(span).replacement("self::").build();
    let fixed = Fix::builder()
        .message("prefix the path with `self::`")
        .edits(vec![insertion])
        .build()
        .ok()
        .and_then(|fix| diagnostic.clone().with_fix(fix).ok());

    Some(fixed.unwrap_or(diagnostic))
}

/// Where a path sits, which decides what its first segment resolves against
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Placement {
    /// Anywhere outside a `use` tree, such as an expression or a type
    Elsewhere,
    /// Inside the tree of a `use` declaration
    UseTree,
}

/// Returns whether the identifier lies in the tree of a `use` declaration
fn placement(identifier: &DecoratedNode<'_>) -> Placement {
    let mut current = *identifier;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "use_declaration" => return Placement::UseTree,
            "scoped_identifier" | "scoped_use_list" | "use_as_clause" | "use_list"
            | "use_wildcard" => current = parent,
            _ => return Placement::Elsewhere,
        }
    }
    Placement::Elsewhere
}

/// Returns the edition of the crate that compiles the file, when the
/// `Target` decoration of the file names it
fn edition(node: &DecoratedNode<'_>) -> Option<Edition> {
    let mut root = *node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    root.decoration::<Target>().map(Target::edition)
}

/// What the rule does with a path into a submodule
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Outcome {
    /// Reports the path and attaches the fix that inserts `self::`
    Fixed,
    /// Reports nothing, because the path does not name the submodule
    Silent,
    /// Reports the path without a fix
    Unfixed,
}

/// Returns what the rule does with a path at `placement` in `edition`
///
/// Outside a `use` tree, a path starts at the module in every edition, so
/// `self::` keeps what it names. A `use` path does that from the edition
/// 2018 on. In the edition 2015 it starts at the crate root, so it does not
/// name the submodule. Without an edition, the rule cannot tell.
fn outcome(placement: Placement, edition: Option<Edition>) -> Outcome {
    match placement {
        Placement::Elsewhere => Outcome::Fixed,
        Placement::UseTree => match edition {
            Some(Edition::Edition2015) => Outcome::Silent,
            Some(Edition::Edition2018 | Edition::Edition2021 | Edition::Edition2024) => {
                Outcome::Fixed
            }
            None => Outcome::Unfixed,
        },
    }
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
    use stiles_testing::{assert_diagnostic, assert_fix, assert_no_diagnostics, execute, parse};
    use stiles_types::DecoratedTree;
    use stiles_types::Language;

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![UnprefixedSubmodulePath::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![UnprefixedSubmodulePath::into_lint_pass()]
    }

    /// Parses `source` as a file of a crate in `edition`
    ///
    /// The provider puts the edition on the `Target` decoration of the file.
    /// These tests run no provider, so they attach it by hand.
    fn parse_in(source: &str, edition: Edition) -> DecoratedTree {
        let mut tree = parse(source, Language::Rust);
        let target = Target::builder()
            .krate("sample")
            .kinds(stiles_rust::decorations::TargetKinds::default())
            .edition(edition)
            .build();
        let root = tree.root_node().id();
        tree.decorations_mut().insert(root, target);
        tree
    }

    fn run_in(source: &str, edition: Edition) -> Vec<Diagnostic> {
        execute(&parse_in(source, edition), &mut passes())
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
            .has_severity(Severity::Error)
            .message_contains("`error` names a submodule")
            .message_contains("write `self::error`");
    }

    fn run_documented(source: &str) -> Vec<stiles_types::Diagnostic> {
        run_in(source, Edition::Edition2021)
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
    fn edition_2015_path_outside_a_use_tree_is_fixed() {
        assert_fix()
            .tree(parse_in(
                "mod error;\nfn f() { error::Kind::new(); }",
                Edition::Edition2015,
            ))
            .passes(passes)
            .produces("mod error;\nfn f() { self::error::Kind::new(); }");
    }

    #[test]
    fn edition_2015_use_path_is_not_flagged() {
        let diagnostics = run_in("mod error;\nuse error::Kind;", Edition::Edition2015);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn enum_variant_pattern_is_flagged() {
        let source = "mod error;\nfn f(k: K) { match k { error::Kind::A => {} _ => {} } }";

        assert_eq!(flagged(source), ["error"]);
    }

    #[test]
    fn fix_prefixes_a_glob_import() {
        assert_fix()
            .tree(parse_in("mod error;\nuse error::*;", Edition::Edition2021))
            .passes(passes)
            .produces("mod error;\nuse self::error::*;");
    }

    #[test]
    fn fix_prefixes_a_macro_invocation_path() {
        assert_fix()
            .tree(parse(
                "mod macros;\nfn f() { macros::make!(); }",
                Language::Rust,
            ))
            .passes(passes)
            .produces("mod macros;\nfn f() { self::macros::make!(); }");
    }

    #[test]
    fn fix_prefixes_a_path_in_a_block() {
        assert_fix()
            .tree(parse(
                "mod error;\nfn f() { let k: error::Kind = error::Kind::new(); }",
                Language::Rust,
            ))
            .passes(passes)
            .produces(
                "mod error;\nfn f() { let k: self::error::Kind = self::error::Kind::new(); }",
            );
    }

    #[test]
    fn fix_prefixes_a_path_in_an_inline_module() {
        assert_fix()
            .tree(parse_in(
                "mod outer {\n    mod error;\n    use error::Kind;\n}",
                Edition::Edition2024,
            ))
            .passes(passes)
            .produces("mod outer {\n    mod error;\n    use self::error::Kind;\n}");
    }

    #[test]
    fn fix_prefixes_a_pattern() {
        assert_fix()
            .tree(parse(
                "mod error;\nfn f(k: K) { match k { error::Kind::A => {} } }",
                Language::Rust,
            ))
            .passes(passes)
            .produces("mod error;\nfn f(k: K) { match k { self::error::Kind::A => {} } }");
    }

    #[test]
    fn fix_prefixes_a_raw_identifier() {
        assert_fix()
            .tree(parse(
                "mod r#match;\nfn f() { r#match::x(); }",
                Language::Rust,
            ))
            .passes(passes)
            .produces("mod r#match;\nfn f() { self::r#match::x(); }");
    }

    #[test]
    fn fix_prefixes_a_renamed_import() {
        assert_fix()
            .tree(parse_in(
                "mod error;\nuse error as e;",
                Edition::Edition2018,
            ))
            .passes(passes)
            .produces("mod error;\nuse self::error as e;");
    }

    #[test]
    fn fix_prefixes_a_struct_expression() {
        assert_fix()
            .tree(parse(
                "mod error;\nfn f() { error::Kind { code: 1 }; }",
                Language::Rust,
            ))
            .passes(passes)
            .produces("mod error;\nfn f() { self::error::Kind { code: 1 }; }");
    }

    #[test]
    fn fix_prefixes_an_attribute_path() {
        assert_fix()
            .tree(parse(
                "mod attrs;\n#[attrs::marker]\nfn f() {}",
                Language::Rust,
            ))
            .passes(passes)
            .produces("mod attrs;\n#[self::attrs::marker]\nfn f() {}");
    }

    #[test]
    fn fix_prefixes_each_path_of_a_list_without_a_prefix() {
        assert_fix()
            .tree(parse_in(
                "mod a;\nmod b;\nuse {a::X, b::Y};",
                Edition::Edition2021,
            ))
            .passes(passes)
            .produces("mod a;\nmod b;\nuse {self::a::X, self::b::Y};");
    }

    #[test]
    fn fix_prefixes_the_path_of_a_grouped_import() {
        assert_fix()
            .tree(parse_in(
                "mod error;\npub use error::{Kind, ParseError};",
                Edition::Edition2021,
            ))
            .passes(passes)
            .produces("mod error;\npub use self::error::{Kind, ParseError};");
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

    #[cfg(feature = "plugin")]
    #[test]
    fn reads_returns_the_target() {
        let reads = stiles_plugin::DeclaresRules::reads(&UnprefixedSubmodulePath);

        assert_eq!(reads, stiles_plugin::Reads::of::<Target>());
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
    fn trait_send_outcome() {
        fn assert_send<T: Send>() {}
        assert_send::<Outcome>();
    }

    #[test]
    fn trait_send_placement() {
        fn assert_send<T: Send>() {}
        assert_send::<Placement>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<UnprefixedSubmodulePath>();
    }

    #[test]
    fn trait_sync_outcome() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Outcome>();
    }

    #[test]
    fn trait_sync_placement() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Placement>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<UnprefixedSubmodulePath>();
    }

    #[test]
    fn trait_unpin_outcome() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Outcome>();
    }

    #[test]
    fn trait_unpin_placement() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Placement>();
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

    #[test]
    fn use_path_without_an_edition_has_no_fix() {
        let diagnostics = run("mod error;\nuse error::Kind;");

        assert_diagnostic(&diagnostics[0]).has_no_fix();
    }
}
