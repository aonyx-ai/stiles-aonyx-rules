mod test_code;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use whisker_rust::{RuleId, RustLintPass};
use whisker_types::{DecoratedNode, Diagnostic, Severity};

use crate::test_code::is_test_code;

const RULE_ID: RuleId = RuleId::new("lint.external-type-in-port");

/// The crates every Rust toolchain ships, which a port may name freely
const TOOLCHAIN_CRATES: [&str; 3] = ["std", "core", "alloc"];

/// How many `use` declarations one name may pass through before the lookup
/// gives up
///
/// Rust rejects a cycle of imports, but the rule reads one file and cannot
/// rely on the compiler having seen it. The bound keeps a cycle from
/// looping and leaves the name unreported.
const MAX_IMPORT_HOPS: usize = 16;

/// Flags an external crate's type in the signature of a public trait's method
///
/// A port is a `pub trait` that an adapter implements with a toolchain. Once
/// a method names a type from an external crate, every implementation and
/// every caller depends on that crate. The port then no longer keeps the
/// toolchain on the adapter's side, and the compiler does not object.
///
/// The rule reads the parameter types, the return type, and the bounds a
/// method puts on its generics or in its `where` clause. It resolves the
/// first segment of each type path against the `use` declarations of the
/// module that holds the trait. So `anyhow::Result`, an imported `VfsPath`,
/// and an alias of either all count. A bare name that no `use` imports is a
/// local type. A qualified path names an external crate when no `use` or
/// `mod` explains its first segment. A glob import could supply any
/// such segment, so a module with one reports only what its imports prove.
///
/// `std`, `core`, `alloc`, `crate`, `self`, and `super` are not external.
/// Nor is a workspace sibling, whose name shares its prefix before the
/// first `-` or `_` with the directory above the file's `src`. In
/// `crates/whisker-rust/src`, `whisker_types` is therefore not external. That
/// is a naming convention, and a workspace that does not follow it gets false
/// positives.
///
/// Inherent methods, restricted traits such as `pub(crate)`, the trait's own
/// bounds, associated types, and macro bodies are out of scope. So is test
/// code, which is an item under a test attribute or under a `cfg` that holds
/// only under `test`. Every file in a `tests` or `benches` directory is test
/// code too.
///
/// # Examples
///
/// ```ignore
/// pub trait DecorationProvider {
///     fn decorate(&self, tree: &DecoratedTree) -> anyhow::Result<Coverage>;
/// }
/// ```
///
/// The rule reports `anyhow::Result`. A `DecorateError` that the domain
/// declares takes its place, and the adapter converts its own errors into
/// it.
pub struct ExternalTypeInPort;

impl RustLintPass for ExternalTypeInPort {
    fn check_source_file(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_file(node)
    }
}

/// Reports every external type in the ports one file declares
///
/// A trait resolves names against the module that holds it, and only
/// `source_file` sees every module of the file. The walker visits it once,
/// and the pass runs its own traversal from there.
fn check_file(root: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    check_module(root, workspace_prefix(root.span().file()), &mut diagnostics);
    diagnostics
}

/// Checks the traits of one module and recurses into its inline modules
///
/// A module does not inherit its parent's imports, so each one builds its
/// own [`Scope`].
fn check_module(
    module: &DecoratedNode<'_>,
    prefix: Option<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let items = shipped_items(module);
    let scope = Scope::of(&items);

    for item in &items {
        match item.kind() {
            "trait_item" => check_trait(item, &scope, prefix, diagnostics),
            "mod_item" => {
                if let Some(body) = item.child_by_field_name("body") {
                    check_module(&body, prefix, diagnostics);
                }
            }
            _ => {}
        }
    }
}

/// Checks every method of a `pub trait`
///
/// A method with a default body is still part of the contract, so the rule
/// reads its signature like any other. The body itself is not part of the
/// port.
fn check_trait<'a>(
    item: &DecoratedNode<'a>,
    scope: &Scope<'a>,
    prefix: Option<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !is_bare_pub(item) {
        return;
    }
    let Some(body) = item.child_by_field_name("body") else {
        return;
    };

    let trait_generics = generic_names(item);

    for method in shipped_items(&body) {
        match method.kind() {
            "function_signature_item" | "function_item" => {}
            _ => continue,
        }
        let Some(name) = method.child_by_field_name("name") else {
            continue;
        };

        let mut generics = trait_generics.clone();
        generics.extend(generic_names(&method));

        let signature = Signature {
            scope,
            generics,
            method: name.text(),
            prefix,
        };
        signature.check(&method, diagnostics);
    }
}

/// Returns the items of a module or trait body that a normal build compiles
///
/// An attribute or a comment is no item. An item that only a test build
/// compiles is neither a port nor an import that a port can name.
fn shipped_items<'a>(container: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    container
        .named_children()
        .into_iter()
        .filter(|child| match child.kind() {
            "attribute_item" | "inner_attribute_item" | "line_comment" | "block_comment" => false,
            _ => !is_test_code(child),
        })
        .collect()
}

/// Returns whether an item carries a bare `pub`
///
/// A restricted form such as `pub(crate)` holds its scope as a named child,
/// so a bare `pub` is the one with no named children. A restricted trait
/// cannot be implemented outside its crate, so it is not a port.
fn is_bare_pub(item: &DecoratedNode<'_>) -> bool {
    item.named_children()
        .iter()
        .any(|child| child.kind() == "visibility_modifier" && child.named_child_count() == 0)
}

/// Returns the names of the type parameters an item declares
///
/// A parameter named `T` shadows any `T` a `use` imports, and `T::Output`
/// names an associated type rather than a crate.
fn generic_names<'a>(item: &DecoratedNode<'a>) -> HashSet<&'a str> {
    let Some(parameters) = item.child_by_field_name("type_parameters") else {
        return HashSet::new();
    };

    parameters
        .named_children()
        .iter()
        .filter(|parameter| parameter.kind() == "type_parameter")
        .filter_map(|parameter| parameter.child_by_field_name("name"))
        .map(|name| name.text())
        .collect()
}

/// Where the first segment of a path points before the scope resolves it
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
enum Root<'a> {
    /// `crate`, `self`, or `super`, which stay inside the crate
    Local,
    /// A name after a leading `::` or in an `extern crate`, which only a
    /// crate answers to
    Crate(&'a str),
    /// A name the scope has to look up
    Name(&'a str),
}

/// Whether a module imports with a glob such as `use super::*`
///
/// A glob can bring any name into scope, so a module with one leaves an
/// unexplained first segment unreported.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
enum Glob {
    /// Every `use` in the module names what it imports
    #[default]
    Absent,
    /// At least one `use` in the module ends in `*`
    Present,
}

/// The names one module brings into scope
///
/// Rust resolves a path's first segment against the module it is written
/// in. A scope therefore covers one module's direct items and nothing from
/// its parent.
#[derive(Clone, Eq, PartialEq, Debug, Default)]
struct Scope<'a> {
    imports: HashMap<&'a str, Root<'a>>,
    modules: HashSet<&'a str>,
    glob: Glob,
}

impl<'a> Scope<'a> {
    /// Builds the scope of a module from its shipped items
    fn of(items: &[DecoratedNode<'a>]) -> Self {
        let mut scope = Scope::default();

        for item in items {
            match item.kind() {
                "use_declaration" => {
                    if let Some(argument) = item.child_by_field_name("argument") {
                        scope.collect_use(&argument, None, None);
                    }
                }
                "extern_crate_declaration" => scope.collect_extern_crate(item),
                "mod_item" => {
                    if let Some(name) = item.child_by_field_name("name") {
                        scope.modules.insert(name.text());
                    }
                }
                _ => {}
            }
        }

        scope
    }

    /// Records the names one node of a `use` tree imports
    ///
    /// `inherited` is the root of the enclosing `a::{...}` list, which every
    /// entry in it shares. `parent` is that list's own path, which names the
    /// module a `self` entry imports.
    fn collect_use(
        &mut self,
        node: &DecoratedNode<'a>,
        inherited: Option<Root<'a>>,
        parent: Option<&DecoratedNode<'a>>,
    ) {
        match node.kind() {
            "identifier" => {
                let name = node.text();
                self.imports
                    .insert(name, inherited.unwrap_or(Root::Name(name)));
            }
            "scoped_identifier" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return;
                };
                let Some(root) = inherited.or_else(|| path_root(node)) else {
                    return;
                };
                self.imports.insert(name.text(), root);
            }
            "self" => {
                let Some(name) = parent.and_then(last_segment) else {
                    return;
                };
                let Some(root) = inherited else {
                    return;
                };
                self.imports.insert(name, root);
            }
            "use_as_clause" => self.collect_alias(node, inherited),
            "scoped_use_list" => {
                let path = node.child_by_field_name("path");
                let root = inherited.or_else(|| path.as_ref().and_then(path_root));
                let Some(list) = node.child_by_field_name("list") else {
                    return;
                };
                for entry in list.named_children() {
                    self.collect_use(&entry, root, path.as_ref());
                }
            }
            "use_list" => {
                for entry in node.named_children() {
                    self.collect_use(&entry, inherited, parent);
                }
            }
            "use_wildcard" => self.glob = Glob::Present,
            _ => {}
        }
    }

    /// Records the alias of a `use ... as name` entry
    ///
    /// `as _` imports a trait's methods without a name, so it binds nothing a
    /// signature could write.
    fn collect_alias(&mut self, node: &DecoratedNode<'a>, inherited: Option<Root<'a>>) {
        let Some(alias) = node.child_by_field_name("alias") else {
            return;
        };
        let Some(path) = node.child_by_field_name("path") else {
            return;
        };
        if alias.text() == "_" {
            return;
        }

        let root = match path.kind() {
            "self" => inherited,
            _ => inherited.or_else(|| path_root(&path)),
        };
        let Some(root) = root else {
            return;
        };
        self.imports.insert(alias.text(), root);
    }

    /// Records the name an `extern crate` declaration binds
    fn collect_extern_crate(&mut self, item: &DecoratedNode<'a>) {
        let Some(name) = item.child_by_field_name("name") else {
            return;
        };
        let crate_name = name.text();
        let bound = match item.child_by_field_name("alias") {
            Some(alias) => alias.text(),
            None => crate_name,
        };
        let root = match crate_name {
            "self" => Root::Local,
            _ => Root::Crate(crate_name),
        };
        self.imports.insert(bound, root);
    }

    /// Returns the crate a root resolves to, or [`None`] when it stays local
    ///
    /// The lookup follows imports until it reaches `crate`, `self`, `super`,
    /// a local module, or a name nothing in the module explains. That last name
    /// is a crate, because Rust 2018 resolves an unexplained first segment
    /// against the crates the package depends on. A glob import could explain
    /// it as well, so a module with one yields [`None`] there. The result may
    /// be a toolchain crate or a sibling; [`Signature::is_external`] decides.
    fn crate_of(&self, root: Root<'a>) -> Option<&'a str> {
        let mut root = root;

        for _ in 0..MAX_IMPORT_HOPS {
            let name = match root {
                Root::Local => return None,
                Root::Crate(name) => return Some(name),
                Root::Name(name) => name,
            };

            if self.modules.contains(name) {
                return None;
            }

            match self.imports.get(name) {
                Some(Root::Name(next)) if *next == name => return Some(name),
                Some(next) => {
                    root = *next;
                    continue;
                }
                None => {}
            }

            if self.glob == Glob::Present {
                return None;
            }
            return Some(name);
        }

        None
    }
}

/// The names one port method's signature resolves against
struct Signature<'s, 'a> {
    scope: &'s Scope<'a>,
    generics: HashSet<&'a str>,
    method: &'a str,
    prefix: Option<&'s str>,
}

impl<'a> Signature<'_, 'a> {
    /// Checks the parameter types, the return type, and the generic bounds
    /// of one method
    ///
    /// A `self` receiver with no type names nothing. A typed receiver such
    /// as `self: Box<Self>` is a `parameter` and is read like the rest.
    fn check(&self, method: &DecoratedNode<'a>, diagnostics: &mut Vec<Diagnostic>) {
        for child in method.named_children() {
            match child.kind() {
                "parameters" => {
                    for parameter in child.named_children() {
                        if parameter.kind() != "parameter" {
                            continue;
                        }
                        if let Some(ty) = parameter.child_by_field_name("type") {
                            self.visit_type(&ty, diagnostics);
                        }
                    }
                }
                "type_parameters" => {
                    for parameter in child.named_children() {
                        if parameter.kind() != "type_parameter" {
                            continue;
                        }
                        if let Some(bounds) = parameter.child_by_field_name("bounds") {
                            self.visit_type(&bounds, diagnostics);
                        }
                    }
                }
                "where_clause" => {
                    for predicate in child.named_children() {
                        if let Some(left) = predicate.child_by_field_name("left") {
                            self.visit_type(&left, diagnostics);
                        }
                        if let Some(bounds) = predicate.child_by_field_name("bounds") {
                            self.visit_type(&bounds, diagnostics);
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(return_type) = method.child_by_field_name("return_type") {
            self.visit_type(&return_type, diagnostics);
        }
    }

    /// Checks every type path inside one type or bound
    ///
    /// The `name` of a type binding, such as the `Item` in
    /// `Iterator<Item = T>`, belongs to the trait and is skipped. So is the
    /// length of an array, which is an expression rather than a type. A macro
    /// in type position is opaque, so the walk stops there.
    fn visit_type(&self, node: &DecoratedNode<'a>, diagnostics: &mut Vec<Diagnostic>) {
        match node.kind() {
            "type_identifier" => self.check_bare(node, diagnostics),
            "scoped_type_identifier" | "scoped_identifier" => {
                self.check_qualified(node, diagnostics);
                self.visit_path_arguments(node, diagnostics);
            }
            "generic_type" => {
                if let Some(base) = node.child_by_field_name("type") {
                    match base.kind() {
                        "identifier" => self.check_bare(&base, diagnostics),
                        _ => self.visit_type(&base, diagnostics),
                    }
                }
                if let Some(arguments) = node.child_by_field_name("type_arguments") {
                    self.visit_type(&arguments, diagnostics);
                }
            }
            "type_binding" => {
                if let Some(arguments) = node.child_by_field_name("type_arguments") {
                    self.visit_type(&arguments, diagnostics);
                }
                if let Some(ty) = node.child_by_field_name("type") {
                    self.visit_type(&ty, diagnostics);
                }
            }
            "array_type" => {
                if let Some(element) = node.child_by_field_name("element") {
                    self.visit_type(&element, diagnostics);
                }
            }
            "macro_invocation" => {}
            _ => {
                for child in node.named_children() {
                    self.visit_type(&child, diagnostics);
                }
            }
        }
    }

    /// Checks the type arguments written inside a path, such as the `X` in
    /// `<T as Trait<X>>::Output`
    ///
    /// The path's root was already checked with the whole path, so the walk
    /// visits only what sits in brackets or angle brackets along the way.
    fn visit_path_arguments(&self, node: &DecoratedNode<'a>, diagnostics: &mut Vec<Diagnostic>) {
        match node.kind() {
            "scoped_type_identifier" | "scoped_identifier" => {
                if let Some(path) = node.child_by_field_name("path") {
                    self.visit_path_arguments(&path, diagnostics);
                }
            }
            "generic_type" => {
                if let Some(arguments) = node.child_by_field_name("type_arguments") {
                    self.visit_type(&arguments, diagnostics);
                }
                if let Some(base) = node.child_by_field_name("type") {
                    self.visit_path_arguments(&base, diagnostics);
                }
            }
            "bracketed_type" => self.visit_type(node, diagnostics),
            _ => {}
        }
    }

    /// Reports a bare name that a `use` imports from an external crate
    ///
    /// A bare name that no `use` imports is defined in the module or comes
    /// from the prelude, so it stays unreported.
    fn check_bare(&self, node: &DecoratedNode<'a>, diagnostics: &mut Vec<Diagnostic>) {
        let name = node.text();
        if self.is_parameter(name) {
            return;
        }
        let Some(root) = self.scope.imports.get(name) else {
            return;
        };
        self.report_if_external(node, *root, diagnostics);
    }

    /// Reports a qualified path whose first segment is an external crate
    fn check_qualified(&self, node: &DecoratedNode<'a>, diagnostics: &mut Vec<Diagnostic>) {
        let Some(root) = path_root(node) else {
            return;
        };
        if let Root::Name(name) = root
            && self.is_parameter(name)
        {
            return;
        }
        self.report_if_external(node, root, diagnostics);
    }

    /// Pushes a diagnostic when a root resolves to an external crate
    fn report_if_external(
        &self,
        node: &DecoratedNode<'a>,
        root: Root<'a>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let Some(krate) = self.scope.crate_of(root) else {
            return;
        };
        if !self.is_external(krate) {
            return;
        }

        diagnostics.push(Diagnostic::builder().rule_id(RULE_ID).severity(Severity::Warn).message(format!(
                "port method `{}` names `{}` from the external crate `{krate}`; use a type the domain owns",
                self.method,
                node.text()
            )).span(node.span()).build());
    }

    /// Returns whether a name is `Self` or a type parameter in scope
    fn is_parameter(&self, name: &str) -> bool {
        name == "Self" || self.generics.contains(name)
    }

    /// Returns whether a crate is neither a toolchain crate nor a sibling
    ///
    /// A sibling shares the prefix before its first `-` or `_` with the
    /// directory of the crate being checked. Without that directory there is
    /// no proof of a sibling, so every other crate is external.
    fn is_external(&self, krate: &str) -> bool {
        if TOOLCHAIN_CRATES.contains(&krate) {
            return false;
        }

        match self.prefix {
            Some(prefix) => crate_prefix(krate) != prefix,
            None => true,
        }
    }
}

/// Returns the first segment of a path, unless a macro or a bracketed type
/// stands there
fn path_root<'a>(node: &DecoratedNode<'a>) -> Option<Root<'a>> {
    match node.kind() {
        "identifier" | "type_identifier" => Some(Root::Name(node.text())),
        "crate" | "self" | "super" => Some(Root::Local),
        "scoped_identifier" | "scoped_type_identifier" => match node.child_by_field_name("path") {
            Some(path) => path_root(&path),
            None => node
                .child_by_field_name("name")
                .map(|name| Root::Crate(name.text())),
        },
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|base| path_root(&base)),
        _ => None,
    }
}

/// Returns the last segment of a `use` path, which a `self` entry imports
fn last_segment<'a>(node: &DecoratedNode<'a>) -> Option<&'a str> {
    match node.kind() {
        "identifier" => Some(node.text()),
        "scoped_identifier" => node.child_by_field_name("name").map(|name| name.text()),
        _ => None,
    }
}

/// Returns the crate prefix of the directory that holds a file's `src`
///
/// `crates/whisker-rust/src/lib.rs` yields `whisker`. A path with no `src`
/// directory yields [`None`], and then no crate counts as a sibling.
fn workspace_prefix(path: &Path) -> Option<&str> {
    let names: Vec<&str> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    let src = names.iter().rposition(|name| *name == "src")?;
    let directory = names.get(src.checked_sub(1)?)?;
    Some(crate_prefix(directory))
}

/// Returns the part of a crate or directory name before its first `-` or `_`
fn crate_prefix(name: &str) -> &str {
    match name.split_once(['-', '_']) {
        Some((prefix, _)) => prefix,
        None => name,
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for ExternalTypeInPort {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

whisker_plugin::export_lints![rust: [ExternalTypeInPort]];

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{DecoratedTree, Language, LintPass};

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(ExternalTypeInPort))];
        execute(&tree, &mut passes)
    }

    /// Runs the rule on `source` as if it were the file at `path`
    ///
    /// `parse` names every file `<test>`, so this parses again with the
    /// grammar it used and a path of the test's own.
    fn run_at(path: &Path, source: &str) -> Vec<Diagnostic> {
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

        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(ExternalTypeInPort))];
        execute(&tree, &mut passes)
    }

    #[test]
    fn aliased_crate_import_is_flagged_once() {
        let source = r#"
use ra_ap_vfs as vfs;

pub trait Port {
    fn read(&self, file: &vfs::VfsPath);
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn aliased_crate_import_message_names_the_alias_path_and_real_crate() {
        let source = r#"
use ra_ap_vfs as vfs;

pub trait Port {
    fn read(&self, file: &vfs::VfsPath);
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `vfs::VfsPath` from the external crate `ra_ap_vfs`");
    }

    #[test]
    fn aliased_type_import_is_flagged_once() {
        let source = r#"
use ra_ap_vfs::VfsPath as File;

pub trait Port {
    fn read(&self, file: File);
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn aliased_type_import_message_names_the_alias_and_real_crate() {
        let source = r#"
use ra_ap_vfs::VfsPath as File;

pub trait Port {
    fn read(&self, file: File);
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `File` from the external crate `ra_ap_vfs`");
    }

    #[test]
    fn anyhow_result_return_diagnostic_has_rule_severity_and_message() {
        let source = r#"
pub trait DecorationProvider {
    fn decorate(&self, tree: &DecoratedTree) -> anyhow::Result<Coverage>;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.external-type-in-port")
            .has_severity(Severity::Warn)
            .message_contains(
                "port method `decorate` names `anyhow::Result` from the external crate `anyhow`; use a type the domain owns",
            );
    }

    #[test]
    fn anyhow_result_return_is_flagged_once() {
        let source = r#"
pub trait DecorationProvider {
    fn decorate(&self, tree: &DecoratedTree) -> anyhow::Result<Coverage>;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn array_length_is_not_flagged() {
        let diagnostics = run("pub trait Port { fn read(&self) -> [u8; limits::LEN]; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn associated_type_of_generic_is_not_flagged() {
        let source = r#"
pub trait Port {
    fn first<I: Iterator>(&self, items: I) -> I::Item;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn associated_type_of_trait_generic_is_not_flagged() {
        let diagnostics = run("pub trait Port<S> { fn read(&self) -> S::Output; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bare_local_type_is_not_flagged() {
        let diagnostics = run("pub trait Port { fn read(&self, tree: &Tree) -> Coverage; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bound_in_type_parameter_is_flagged_once() {
        let source = r#"
pub trait Port {
    fn run<S: ra_ap_hir::Semantics>(&self, semantics: S);
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bound_in_type_parameter_message_names_the_bound_and_crate() {
        let source = r#"
pub trait Port {
    fn run<S: ra_ap_hir::Semantics>(&self, semantics: S);
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `ra_ap_hir::Semantics` from the external crate `ra_ap_hir`");
    }

    #[test]
    fn bound_in_where_clause_is_flagged_once() {
        let source = r#"
use serde::Serialize;

pub trait Port {
    fn write<T>(&self, value: T)
    where
        T: Serialize;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bound_in_where_clause_message_names_the_bound_and_crate() {
        let source = r#"
use serde::Serialize;

pub trait Port {
    fn write<T>(&self, value: T)
    where
        T: Serialize;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `Serialize` from the external crate `serde`");
    }

    #[test]
    fn bracketed_type_reports_one_diagnostic() {
        let source = r#"
pub trait Port {
    fn read<T>(&self) -> <T as serde::Deserialize>::Output;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bracketed_type_reports_the_trait_inside() {
        let source = r#"
pub trait Port {
    fn read<T>(&self) -> <T as serde::Deserialize>::Output;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("names `serde::Deserialize`");
    }

    #[test]
    fn crate_prefix_with_hyphen_returns_first_word() {
        assert_eq!(crate_prefix("whisker-rust"), "whisker");
    }

    #[test]
    fn crate_prefix_with_underscore_returns_first_word() {
        assert_eq!(crate_prefix("ra_ap_vfs"), "ra");
    }

    #[test]
    fn crate_prefix_without_separator_returns_whole_name() {
        assert_eq!(crate_prefix("whisker"), "whisker");
    }

    #[test]
    fn default_method_signature_is_flagged() {
        let source = r#"
pub trait Port {
    fn read(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn diagnostic_span_covers_the_type_path() {
        let source = "pub trait Port { fn read(&self) -> anyhow::Result<()>; }";
        let start = source
            .find("anyhow")
            .expect("source should contain the path");

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).has_span(
            "<test>",
            start,
            start + "anyhow::Result".len(),
        );
    }

    #[test]
    fn diagnostic_span_source_reports_one_diagnostic() {
        let source = "pub trait Port { fn read(&self) -> anyhow::Result<()>; }";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn dyn_and_impl_trait_bounds_are_flagged_twice() {
        let source = r#"
pub trait Port {
    fn write(&self, sink: &mut dyn tokio::io::AsyncWrite) -> impl futures::Stream;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn dyn_and_impl_trait_bounds_are_flagged_with_the_dyn_crate_first() {
        let source = r#"
pub trait Port {
    fn write(&self, sink: &mut dyn tokio::io::AsyncWrite) -> impl futures::Stream;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `tokio`");
    }

    #[test]
    fn dyn_and_impl_trait_bounds_are_flagged_with_the_impl_crate_second() {
        let source = r#"
pub trait Port {
    fn write(&self, sink: &mut dyn tokio::io::AsyncWrite) -> impl futures::Stream;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[1]).message_contains("external crate `futures`");
    }

    #[test]
    fn each_occurrence_is_flagged() {
        let source = r#"
use ra_ap_vfs::VfsPath;

pub trait Port {
    fn rename(&self, from: VfsPath) -> VfsPath;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn empty_source_produces_no_diagnostics() {
        let diagnostics = run("");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn extern_crate_alias_is_flagged_once() {
        let source = r#"
extern crate ra_ap_vfs as vfs;

pub trait Port {
    fn read(&self, file: vfs::VfsPath);
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn extern_crate_alias_message_names_the_real_crate() {
        let source = r#"
extern crate ra_ap_vfs as vfs;

pub trait Port {
    fn read(&self, file: vfs::VfsPath);
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `ra_ap_vfs`");
    }

    #[test]
    fn extern_crate_self_alias_is_not_flagged() {
        let source = r#"
extern crate self as whisker;

pub trait Port {
    fn read(&self) -> whisker::Coverage;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn file_in_tests_directory_is_not_flagged() {
        let source = "pub trait Port { fn read(&self) -> anyhow::Result<()>; }";

        let diagnostics = run_at(
            Path::new("crates/whisker/tests/fixtures/src/lib.rs"),
            source,
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn function_pointer_parameter_is_flagged_once() {
        let diagnostics = run("pub trait Port { fn visit(&self, f: fn(syn::Item)); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn function_pointer_parameter_message_names_the_parameter_type() {
        let diagnostics = run("pub trait Port { fn visit(&self, f: fn(syn::Item)); }");

        assert_diagnostic(&diagnostics[0]).message_contains("names `syn::Item`");
    }

    #[test]
    fn generic_argument_is_flagged_once() {
        let source = r#"
use ra_ap_vfs::VfsPath;

pub trait Port {
    fn list(&self) -> Vec<Option<VfsPath>>;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn generic_argument_message_names_the_argument_type() {
        let source = r#"
use ra_ap_vfs::VfsPath;

pub trait Port {
    fn list(&self) -> Vec<Option<VfsPath>>;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("names `VfsPath`");
    }

    #[test]
    fn glob_import_leaves_unexplained_root_unreported() {
        let source = r#"
use super::*;

pub trait Port {
    fn read(&self) -> model::Coverage;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn glob_import_still_reports_proven_external_alias_once() {
        let source = r#"
use super::*;
extern crate anyhow as failure;

pub trait Port {
    fn read(&self) -> failure::Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn glob_import_still_reports_proven_external_alias_with_the_real_crate() {
        let source = r#"
use super::*;
extern crate anyhow as failure;

pub trait Port {
    fn read(&self) -> failure::Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `anyhow`");
    }

    #[test]
    fn import_chain_to_external_crate_is_flagged_once() {
        let source = r#"
use ra_ap_hir as hir;
use hir::Semantics;

pub trait Port {
    fn run(&self, semantics: &Semantics);
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn import_chain_to_external_crate_message_names_the_real_crate() {
        let source = r#"
use ra_ap_hir as hir;
use hir::Semantics;

pub trait Port {
    fn run(&self, semantics: &Semantics);
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `ra_ap_hir`");
    }

    #[test]
    fn import_chain_to_local_module_is_not_flagged() {
        let source = r#"
use crate::model;
use model::Coverage;

pub trait Port {
    fn read(&self) -> Coverage;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn import_cycle_is_not_flagged() {
        let source = r#"
use first as second;
use second as first;

pub trait Port {
    fn read(&self) -> first::Coverage;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn import_under_cfg_test_is_not_resolved() {
        let source = r#"
#[cfg(test)]
use anyhow::Result;

pub trait Port {
    fn read(&self) -> Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn imported_crate_name_is_flagged_once() {
        let source = r#"
use anyhow;

pub trait Port {
    fn read(&self) -> anyhow::Error;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn imported_crate_name_message_names_the_crate() {
        let source = r#"
use anyhow;

pub trait Port {
    fn read(&self) -> anyhow::Error;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `anyhow`");
    }

    #[test]
    fn inherent_method_is_not_flagged() {
        let source = r#"
pub struct Parser;

impl Parser {
    pub fn new(language: tree_sitter::Language) -> Self {
        Parser
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_cfg_test_attribute_hides_the_module() {
        let source = r#"
mod fixtures {
    #![cfg(test)]

    pub trait Port {
        fn read(&self) -> anyhow::Result<()>;
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn leading_colon_path_ignores_a_glob() {
        let source = r#"
use super::*;

pub trait Port {
    fn read(&self) -> ::anyhow::Error;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn leading_colon_path_is_flagged_once() {
        let diagnostics = run("pub trait Port { fn read(&self) -> ::anyhow::Error; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn leading_colon_path_message_names_the_path_and_crate() {
        let diagnostics = run("pub trait Port { fn read(&self) -> ::anyhow::Error; }");

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `::anyhow::Error` from the external crate `anyhow`");
    }

    #[test]
    fn local_module_root_is_not_flagged() {
        let source = r#"
mod model;

use model::Coverage;

pub trait Port {
    fn read(&self) -> model::Tree;
    fn cover(&self) -> Coverage;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn local_path_roots_are_not_flagged() {
        let source = r#"
use super::Tree;

pub trait Port {
    fn read(&self, tree: Tree) -> crate::Coverage;
    fn write(&self, value: self::Value) -> super::Outcome;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn macro_in_type_position_is_not_flagged() {
        let diagnostics = run("pub trait Port { fn read(&self) -> output!(anyhow::Error); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn macro_rules_body_is_not_flagged() {
        let source = r#"
macro_rules! port {
    () => {
        pub trait Port {
            fn read(&self) -> anyhow::Result<()>;
        }
    };
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_generated_by_macro_is_not_flagged() {
        let source = r#"
pub trait Port {
    port_methods!(fn read(&self) -> anyhow::Result<()>;);
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_under_cfg_not_test_is_flagged_once() {
        let source = r#"
pub trait Port {
    #[cfg(not(test))]
    fn read(&self) -> anyhow::Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn method_under_cfg_not_test_message_names_the_type() {
        let source = r#"
pub trait Port {
    #[cfg(not(test))]
    fn read(&self) -> anyhow::Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("names `anyhow::Result`");
    }

    #[test]
    fn method_under_cfg_test_is_not_flagged() {
        let source = r#"
pub trait Port {
    #[cfg(test)]
    fn fake(&self) -> anyhow::Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_module_does_not_inherit_imports() {
        let source = r#"
use anyhow::Result;

mod inner {
    pub trait Port {
        fn read(&self) -> Result<()>;
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_module_with_own_import_is_flagged_once() {
        let source = r#"
pub mod inner {
    use anyhow::Result;

    pub trait Port {
        fn read(&self) -> Result<()>;
    }
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn nested_module_with_own_import_message_names_the_crate() {
        let source = r#"
pub mod inner {
    use anyhow::Result;

    pub trait Port {
        fn read(&self) -> Result<()>;
    }
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `anyhow`");
    }

    #[test]
    fn non_sibling_crate_in_a_workspace_is_flagged_once() {
        let source = r#"
use stabby::IStable;

pub trait Port {
    fn read<T: IStable>(&self, value: T);
}
"#;

        let diagnostics = run_at(Path::new("crates/whisker-types/src/lib.rs"), source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn non_sibling_crate_in_a_workspace_message_names_the_crate() {
        let source = r#"
use stabby::IStable;

pub trait Port {
    fn read<T: IStable>(&self, value: T);
}
"#;

        let diagnostics = run_at(Path::new("crates/whisker-types/src/lib.rs"), source);

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `stabby`");
    }

    #[test]
    fn private_trait_is_not_flagged() {
        let diagnostics = run("trait Port { fn read(&self) -> anyhow::Result<()>; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn qualified_path_to_unimported_crate_is_flagged_once() {
        let source = r#"
pub trait Port {
    fn decorate(&self, file: &ra_ap_vfs::VfsPath) -> Coverage;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn qualified_path_to_unimported_crate_message_names_method_path_and_crate() {
        let source = r#"
pub trait Port {
    fn decorate(&self, file: &ra_ap_vfs::VfsPath) -> Coverage;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains(
            "port method `decorate` names `ra_ap_vfs::VfsPath` from the external crate `ra_ap_vfs`",
        );
    }

    #[test]
    fn restricted_trait_is_not_flagged() {
        let diagnostics = run("pub(crate) trait Port { fn read(&self) -> anyhow::Result<()>; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn self_associated_type_is_not_flagged() {
        let source = r#"
pub trait Decoration {
    type Ref<'a>;

    fn lookup<'a>(node: &DecoratedNode<'a>) -> Self::Ref<'a>;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn sibling_crate_is_not_flagged() {
        let source = r#"
use whisker_types::RuleId;

pub trait DeclaresRules {
    fn rules(&self) -> Vec<RuleId>;
    fn language(&self) -> whisker_rust::Language;
}
"#;

        let diagnostics = run_at(
            Path::new("crates/whisker-plugin/src/declares_rules.rs"),
            source,
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn sibling_of_unprefixed_crate_directory_is_not_flagged() {
        let source = "pub trait Port { fn read(&self) -> whisker_types::RuleId; }";

        let diagnostics = run_at(Path::new("whisker/src/lib.rs"), source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn toolchain_crates_are_not_flagged() {
        let source = r#"
use std::io;
use alloc::sync::Arc;

pub trait Port {
    fn read(&self, file: &std::path::Path) -> io::Result<Arc<str>>;
    fn show(&self, f: &mut core::fmt::Formatter<'_>) -> ::core::fmt::Result;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_in_cfg_test_module_is_not_flagged() {
        let source = r#"
#[cfg(test)]
mod tests {
    pub trait Port {
        fn read(&self) -> anyhow::Result<()>;
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_inside_function_is_not_flagged() {
        let source = r#"
pub fn build() {
    pub trait Port {
        fn read(&self) -> anyhow::Result<()>;
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ExternalTypeInPort>();
    }

    #[test]
    fn trait_send_glob() {
        fn assert_send<T: Send>() {}
        assert_send::<Glob>();
    }

    #[test]
    fn trait_send_root() {
        fn assert_send<T: Send>() {}
        assert_send::<Root<'_>>();
    }

    #[test]
    fn trait_send_scope() {
        fn assert_send<T: Send>() {}
        assert_send::<Scope<'_>>();
    }

    #[test]
    fn trait_send_signature() {
        fn assert_send<T: Send>() {}
        assert_send::<Signature<'_, '_>>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ExternalTypeInPort>();
    }

    #[test]
    fn trait_sync_glob() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Glob>();
    }

    #[test]
    fn trait_sync_root() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Root<'_>>();
    }

    #[test]
    fn trait_sync_scope() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Scope<'_>>();
    }

    #[test]
    fn trait_sync_signature() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Signature<'_, '_>>();
    }

    #[test]
    fn trait_under_cfg_any_test_or_unix_is_flagged_once() {
        let source = r#"
#[cfg(any(test, unix))]
pub trait Port {
    fn read(&self) -> anyhow::Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_under_cfg_any_test_or_unix_message_names_the_type() {
        let source = r#"
#[cfg(any(test, unix))]
pub trait Port {
    fn read(&self) -> anyhow::Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("names `anyhow::Result`");
    }

    #[test]
    fn trait_under_cfg_test_is_not_flagged() {
        let source = r#"
#[cfg(test)]
pub trait Port {
    fn read(&self) -> anyhow::Result<()>;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ExternalTypeInPort>();
    }

    #[test]
    fn trait_unpin_glob() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Glob>();
    }

    #[test]
    fn trait_unpin_root() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Root<'_>>();
    }

    #[test]
    fn trait_unpin_scope() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Scope<'_>>();
    }

    #[test]
    fn trait_unpin_signature() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Signature<'_, '_>>();
    }

    #[test]
    fn type_binding_name_is_not_flagged() {
        let source = r#"
use ra_ap_vfs::Item;

pub trait Port {
    fn items(&self) -> impl Iterator<Item = u8>;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn type_binding_value_is_flagged_once() {
        let source = r#"
pub trait Port {
    fn items(&self) -> impl Iterator<Item = ra_ap_vfs::FileId>;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn type_binding_value_message_names_the_value_type() {
        let source = r#"
pub trait Port {
    fn items(&self) -> impl Iterator<Item = ra_ap_vfs::FileId>;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("names `ra_ap_vfs::FileId`");
    }

    #[test]
    fn type_parameter_shadowing_an_import_is_not_flagged() {
        let source = r#"
use serde::T;

pub trait Port {
    fn write<T>(&self, value: T);
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn typed_receiver_is_flagged_once() {
        let diagnostics = run("pub trait Port { fn read(self: triomphe::Arc<Self>); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn typed_receiver_message_names_the_crate() {
        let diagnostics = run("pub trait Port { fn read(self: triomphe::Arc<Self>); }");

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `triomphe`");
    }

    #[test]
    fn underscore_import_binds_no_name() {
        let source = r#"
use anyhow::Context as _;

pub trait Port {
    fn read(&self) -> Context;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn unrelated_crate_without_workspace_is_flagged() {
        let source = "pub trait Port { fn read(&self) -> whisker_types::RuleId; }";

        let diagnostics = run_at(Path::new("src/lib.rs"), source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn use_list_entries_are_flagged_three_times() {
        let source = r#"
use ra_ap_vfs::{FileId, path::{self as paths, VfsPath}};

pub trait Port {
    fn read(&self, id: FileId, path: VfsPath) -> paths::Kind;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 3);
    }

    #[test]
    fn use_list_entries_are_flagged_with_the_list_crate() {
        let source = r#"
use ra_ap_vfs::{FileId, path::{self as paths, VfsPath}};

pub trait Port {
    fn read(&self, id: FileId, path: VfsPath) -> paths::Kind;
}
"#;

        let naming_the_crate = run(source)
            .iter()
            .filter(|diagnostic| diagnostic.message().contains("external crate `ra_ap_vfs`"))
            .count();

        assert_eq!(naming_the_crate, 3);
    }

    #[test]
    fn use_list_self_entry_is_resolved_to_one_diagnostic() {
        let source = r#"
use std::io::{self, Read};
use ra_ap_vfs::{self};

pub trait Port {
    fn read(&self, reader: &mut dyn Read) -> io::Result<ra_ap_vfs::FileId>;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn use_list_self_entry_is_resolved_to_the_external_path() {
        let source = r#"
use std::io::{self, Read};
use ra_ap_vfs::{self};

pub trait Port {
    fn read(&self, reader: &mut dyn Read) -> io::Result<ra_ap_vfs::FileId>;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("names `ra_ap_vfs::FileId`");
    }

    #[test]
    fn workspace_prefix_with_crate_directory_returns_prefix() {
        let prefix = workspace_prefix(Path::new("./crates/whisker-rust/src/lint/pass.rs"));

        assert_eq!(prefix, Some("whisker"));
    }

    #[test]
    fn workspace_prefix_without_src_returns_none() {
        let prefix = workspace_prefix(Path::new("build.rs"));

        assert_eq!(prefix, None);
    }
}
