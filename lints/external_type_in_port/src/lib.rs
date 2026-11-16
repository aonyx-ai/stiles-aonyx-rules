use std::path::Path;

#[cfg(feature = "plugin")]
use stiles_rust::ReadsTestCode as _;
use stiles_rust::decorations::TypePath;
use stiles_rust::decorations::{DefinitionKind, DefinitionOrigin, Resolution};
use stiles_rust::{RuleId, RustLintPass, is_test_code};
use stiles_types::{DecoratedNode, Diagnostic, RuleOptions, Severity};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(external_type_in_port::RULE_ID.as_str(), "lint.external-type-in-port");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.external-type-in-port");

/// The crates every Rust toolchain ships, which a port may name freely
const TOOLCHAIN_CRATES: [&str; 3] = ["std", "core", "alloc"];

/// The option that names the crates a port may name freely
const ALLOWED_CRATES: &str = "allowed-crates";

/// Flags an external crate's type in the signature of a public trait's method
///
/// A port is a `pub trait` that an adapter implements with a toolchain. Once
/// a method names a type from an external crate, every implementation and
/// every caller depends on that crate. The port then no longer keeps the
/// toolchain on the adapter's side, and the compiler does not object.
///
/// The rule reads the parameter types, the return type, and the bounds a
/// method puts on its generics or in its `where` clause. The `Resolution`
/// decoration of each type names the crate of its definition. It follows a
/// `use`, a glob, a rename, and a re-export, so `anyhow::Result`, an
/// imported `VfsPath`, and an alias of either all count. A type that the
/// crate of the file defines is not external. Neither is a type parameter,
/// an associated type of one or of `Self`, nor a name without the decoration.
///
/// `std`, `core`, and `alloc` are not external. Nor is a workspace sibling,
/// whose name shares its prefix before the first `-` or `_` with the
/// directory above the file's `src`. In `crates/stiles-rust/src`,
/// `stiles_types` is therefore not external. That is a naming convention,
/// and a workspace that does not follow it gets false positives. A sibling
/// that re-exports a type of another crate does not hide that crate.
///
/// Inherent methods, restricted traits such as `pub(crate)`, the trait's own
/// bounds, associated types, and macro bodies are out of scope. So is test
/// code, which the Rust provider marks with the `TestCode` decoration. That
/// is an item under a test attribute or under a `cfg` that holds only under
/// `test`. A file is test code too when the kinds of its `Target`
/// decoration are only integration tests and benches.
///
/// A project can make a crate part of its own public API on purpose, and
/// re-export it for its consumers. The `allowed-crates` option names such
/// crates, as Rust writes them in a path. The rule then treats each one like
/// `std`. A type is allowed when the crate that defines it is allowed. It is
/// allowed too when the path names an allowed crate first, as in
/// `facade::Thing`. A bare name is allowed when a `use` of the same file
/// imports its definition through an allowed crate. A glob import names no
/// definition, so a name it brings in counts by its defining crate alone.
/// The finding prints the crate as the path writes it, or else the defining
/// crate.
///
/// # Examples
///
/// ```text
/// // DON'T
/// pub trait DecorationProvider {
///     fn decorate(&self, tree: &DecoratedTree) -> anyhow::Result<Coverage>;
/// }
///
/// // DO
/// pub trait DecorationProvider {
///     fn decorate(&self, tree: &DecoratedTree) -> Result<Coverage, DecorateError>;
/// }
/// ```
///
/// The rule reads the resolution of each type, which the Rust provider
/// attaches as a decoration. This example attaches it by hand.
///
/// ```
/// use external_type_in_port::ExternalTypeInPort;
/// use stiles_rust::RustLintPassAdapter;
/// use stiles_rust::decorations::{DefinitionKind, DefinitionOrigin, Resolution, TypePath};
/// use stiles_testing::{decorate, execute, parse};
/// use stiles_types::{DecorationMap, Language, LintPass};
///
/// let source = "pub trait Provider { fn decorate(&self) -> anyhow::Result<()>; }";
/// let mut tree = parse(source, Language::Rust);
/// let port = tree.root_node().named_child(0).unwrap();
/// let method = port.child_by_field_name("body").unwrap().named_child(0).unwrap();
/// let returned = method.child_by_field_name("return_type").unwrap();
/// let path = returned.child_by_field_name("type").unwrap().id();
/// let result = TypePath::builder().krate("anyhow").modules(["error"]).name("Result").build();
/// let mut decorations = DecorationMap::new();
/// decorations.insert(
///     path,
///     Resolution::builder()
///         .kind(DefinitionKind::TypeAlias)
///         .origin(DefinitionOrigin::OtherCrate)
///         .krate("anyhow")
///         .path(result)
///         .build(),
/// );
/// decorate(&mut tree, decorations);
/// let pass: Box<dyn LintPass> = Box::new(RustLintPassAdapter::new(ExternalTypeInPort::default()));
/// let mut passes = vec![pass];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
///
/// The rule reports `anyhow::Result`. A `DecorateError` that the domain
/// declares takes its place, and the adapter converts its own errors into
/// it.
#[derive(Default)]
pub struct ExternalTypeInPort {
    allowed_crates: Vec<String>,
}

impl RustLintPass for ExternalTypeInPort {
    fn configure(&mut self, options: &RuleOptions) {
        self.allowed_crates = options.names(RULE_ID, ALLOWED_CRATES).unwrap_or_default();
    }

    fn check_source_file(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_file(node, &self.allowed_crates)
    }
}

/// Reports every external type in the ports one file declares
///
/// The walk reaches the traits of the file and of its inline modules, and no
/// trait inside a function body.
fn check_file(root: &DecoratedNode<'_>, allowed: &[String]) -> Vec<Diagnostic> {
    let mut imports = Vec::new();
    collect_imports(root, &mut imports);
    let span = root.span();
    let context = Context {
        prefix: workspace_prefix(span.file()),
        allowed,
        imports,
    };
    let mut diagnostics = Vec::new();
    check_module(root, &context, &mut diagnostics);
    diagnostics
}

/// What the ports of one file are checked against
struct Context<'s> {
    /// The crate prefix of the workspace sibling crates
    prefix: Option<&'s str>,
    /// The crates that `allowed-crates` names
    allowed: &'s [String],
    /// The definitions that the `use` declarations of the file import
    imports: Vec<Import>,
}

/// A definition that a `use` imports, and the crate the `use` names first
#[derive(Clone, Eq, PartialEq, Debug)]
struct Import {
    definition: TypePath,
    written: WrittenCrate,
}

/// A crate as the first segment of a path names it
#[derive(Clone, Eq, PartialEq, Debug)]
struct WrittenCrate {
    /// The name of the crate that the segment resolves to
    krate: String,
    /// The segment as written, without its `r#` prefix
    name: String,
}

/// Collects every definition that a `use` of the file imports by its path
///
/// A glob imports no single definition, and a `self` entry imports a module,
/// so neither adds one.
fn collect_imports(node: &DecoratedNode<'_>, imports: &mut Vec<Import>) {
    if node.kind() == "use_declaration" {
        if let Some(argument) = node.child_by_field_name("argument") {
            collect_use(&argument, None, imports);
        }
        return;
    }
    for child in node.named_children() {
        collect_imports(&child, imports);
    }
}

/// Collects the imports of one node of a `use` tree
///
/// `root` is the first segment of the enclosing `a::{...}` list, which every
/// entry in it shares.
fn collect_use<'a>(
    node: &DecoratedNode<'a>,
    root: Option<DecoratedNode<'a>>,
    imports: &mut Vec<Import>,
) {
    match node.kind() {
        "identifier" | "scoped_identifier" => {
            let definition = node
                .decoration::<Resolution>()
                .and_then(|resolution| resolution.path().cloned());
            let written = root
                .or_else(|| Some(first_segment(node)))
                .and_then(|first| written_crate(&first));
            if let (Some(definition), Some(written)) = (definition, written) {
                imports.push(Import {
                    definition,
                    written,
                });
            }
        }
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                collect_use(&path, root, imports);
            }
        }
        "scoped_use_list" => {
            let root = root.or_else(|| {
                node.child_by_field_name("path")
                    .map(|path| first_segment(&path))
            });
            if let Some(list) = node.child_by_field_name("list") {
                collect_use(&list, root, imports);
            }
        }
        "use_list" => {
            for entry in node.named_children() {
                collect_use(&entry, root, imports);
            }
        }
        _ => {}
    }
}

/// Returns the first segment of a path
///
/// A path that starts with `::` holds its first segment as the name of its
/// innermost part.
fn first_segment<'a>(path: &DecoratedNode<'a>) -> DecoratedNode<'a> {
    let mut root = *path;
    while let Some(qualifier) = root.child_by_field_name("path") {
        root = qualifier;
    }
    match root.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            root.child_by_field_name("name").unwrap_or(root)
        }
        _ => root,
    }
}

/// Returns the crate that a first segment names, when it resolves to one
fn written_crate(segment: &DecoratedNode<'_>) -> Option<WrittenCrate> {
    let resolution = segment
        .decoration::<Resolution>()
        .filter(|resolution| resolution.kind() == DefinitionKind::Crate)?;
    let text = segment.text();

    Some(WrittenCrate {
        krate: resolution.krate()?.to_owned(),
        name: text.strip_prefix("r#").unwrap_or(text).to_owned(),
    })
}

/// Checks the traits of one module and recurses into its inline modules
fn check_module(
    module: &DecoratedNode<'_>,
    context: &Context<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in shipped_items(module) {
        match item.kind() {
            "trait_item" => check_trait(&item, context, diagnostics),
            "mod_item" => {
                if let Some(body) = item.child_by_field_name("body") {
                    check_module(&body, context, diagnostics);
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
fn check_trait(item: &DecoratedNode<'_>, context: &Context<'_>, diagnostics: &mut Vec<Diagnostic>) {
    if !is_bare_pub(item) {
        return;
    }
    let Some(body) = item.child_by_field_name("body") else {
        return;
    };

    for method in shipped_items(&body) {
        match method.kind() {
            "function_signature_item" | "function_item" => {}
            _ => continue,
        }
        let Some(name) = method.child_by_field_name("name") else {
            continue;
        };

        let signature = Signature {
            method: name.text(),
            context,
        };
        signature.check(&method, diagnostics);
    }
}

/// Returns the items of a module or trait body that a normal build compiles
///
/// An attribute or a comment is no item. An item that only a test build
/// compiles is no port.
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

/// Returns whether a path projects an associated type out of a parameter
///
/// `T::Item`, `Self::Ref`, and `<T as Trait>::Output` name a type through
/// a parameter of the port, not a type of another crate. The bound that
/// gives the parameter its trait is checked on its own.
fn is_projection(path: &DecoratedNode<'_>) -> bool {
    let mut root = *path;
    while let Some(qualifier) = root.child_by_field_name("path") {
        root = qualifier;
    }

    root.kind() == "bracketed_type"
        || root
            .decoration::<Resolution>()
            .is_some_and(|resolution| names_a_parameter(resolution.kind()))
}

/// Returns whether a kind of definition stands for a parameter of the port
fn names_a_parameter(kind: DefinitionKind) -> bool {
    match kind {
        DefinitionKind::TypeParameter | DefinitionKind::SelfType => true,
        DefinitionKind::Crate
        | DefinitionKind::Module
        | DefinitionKind::Struct
        | DefinitionKind::Enum
        | DefinitionKind::Union
        | DefinitionKind::EnumVariant
        | DefinitionKind::Trait
        | DefinitionKind::TypeAlias
        | DefinitionKind::Function
        | DefinitionKind::AssociatedFunction
        | DefinitionKind::Method
        | DefinitionKind::Const
        | DefinitionKind::Static
        | DefinitionKind::Macro
        | DefinitionKind::LocalBinding
        | DefinitionKind::ConstParameter
        | DefinitionKind::BuiltinType
        | DefinitionKind::BuiltinAttribute
        | DefinitionKind::ToolModule
        | DefinitionKind::DeriveHelper => false,
    }
}

/// What one port method's findings name and which crates they skip
struct Signature<'s, 'a> {
    method: &'a str,
    context: &'s Context<'s>,
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
            "type_identifier" => self.check_name(node, diagnostics),
            "scoped_type_identifier" | "scoped_identifier" => {
                if !is_projection(node) {
                    self.check_name(node, diagnostics);
                }
                self.visit_path_arguments(node, diagnostics);
            }
            "generic_type" => {
                if let Some(base) = node.child_by_field_name("type") {
                    match base.kind() {
                        "identifier" => self.check_name(&base, diagnostics),
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
    /// The path itself was already checked whole, so the walk visits only
    /// what sits in brackets or angle brackets along the way.
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

    /// Reports a type whose definition lies in an external crate
    ///
    /// A name without a resolution stays unreported, as does one that the
    /// crate of the file or the language defines.
    fn check_name(&self, node: &DecoratedNode<'a>, diagnostics: &mut Vec<Diagnostic>) {
        let Some(resolution) = node.decoration::<Resolution>() else {
            return;
        };
        let defining = match resolution.origin() {
            DefinitionOrigin::OtherCrate => resolution.krate(),
            DefinitionOrigin::SameCrate | DefinitionOrigin::Builtin => None,
        };
        let Some(defining) = defining.filter(|krate| self.is_external(krate)) else {
            return;
        };
        let written = self.written_crate(node, resolution.path());
        if written.as_ref().is_some_and(|written| self.allows(written)) {
            return;
        }
        let krate = written
            .as_ref()
            .map(|written| written.krate.as_str())
            .filter(|krate| self.is_external(krate))
            .unwrap_or(defining);

        diagnostics.push(Diagnostic::builder().rule_id(RULE_ID).severity(Severity::Error).message(format!(
                "port method `{}` names `{}` from the external crate `{krate}`; use a type the domain owns",
                self.method,
                node.text()
            )).span(node.span()).build());
    }

    /// Returns the crate through which the file names a type
    ///
    /// A qualified path names it in place by its first segment. A `use` that
    /// imports the same definition names it otherwise.
    fn written_crate(
        &self,
        node: &DecoratedNode<'a>,
        definition: Option<&TypePath>,
    ) -> Option<WrittenCrate> {
        let in_place = node
            .child_by_field_name("path")
            .and_then(|path| written_crate(&first_segment(&path)));

        in_place.or_else(|| {
            let definition = definition?;
            self.context
                .imports
                .iter()
                .find(|import| import.definition == *definition)
                .map(|import| import.written.clone())
        })
    }

    /// Returns whether `allowed-crates` names a written crate
    fn allows(&self, written: &WrittenCrate) -> bool {
        let WrittenCrate { krate, name } = written;

        self.context
            .allowed
            .iter()
            .any(|allowed| allowed == krate || allowed == name)
    }

    /// Returns whether a crate is not a toolchain crate, not an allowed crate,
    /// and not a sibling
    ///
    /// A sibling shares the prefix before its first `-` or `_` with the
    /// directory of the crate being checked. Without that directory there is
    /// no proof of a sibling, so every other crate is external.
    fn is_external(&self, krate: &str) -> bool {
        if TOOLCHAIN_CRATES.contains(&krate)
            || self.context.allowed.iter().any(|name| name == krate)
        {
            return false;
        }

        match self.context.prefix {
            Some(prefix) => crate_prefix(krate) != prefix,
            None => true,
        }
    }
}

/// Returns the crate prefix of the directory that holds a file's `src`
///
/// `crates/stiles-rust/src/lib.rs` yields `stiles`. A path with no `src`
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
impl stiles_plugin::DeclaresRules for ExternalTypeInPort {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::of::<Resolution>().and_test_code()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [ExternalTypeInPort::default()]];

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use stiles_rust::RustLintPassAdapter;
    use stiles_rust::decorations::TargetKind;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use stiles_types::{DecoratedTree, DecorationMap, Language, LintPass, RuleOption};

    use super::*;

    /// A name as the source spells it, and the definition it resolves to
    ///
    /// The definition is a path whose first segment is its crate, where
    /// `sample` is the crate of the file. `<param>` stands for a type
    /// parameter, `<self>` for `Self`, and `<crate:name>` for the root of
    /// the crate `name`.
    type Resolved<'a> = (&'a str, &'a str);

    const ANYHOW_RESULT: Resolved<'static> = ("anyhow::Result", "anyhow::Result");

    /// Returns the resolution the provider gives a name with this definition
    fn resolution(definition: &str) -> Resolution {
        let parameter = match definition {
            "<param>" => Some(DefinitionKind::TypeParameter),
            "<self>" => Some(DefinitionKind::SelfType),
            _ => None,
        };
        if let Some(krate) = definition
            .strip_prefix("<crate:")
            .and_then(|rest| rest.strip_suffix('>'))
        {
            return Resolution::builder()
                .kind(DefinitionKind::Crate)
                .origin(DefinitionOrigin::OtherCrate)
                .krate(krate)
                .build();
        }
        if let Some(kind) = parameter {
            return Resolution::builder()
                .kind(kind)
                .origin(DefinitionOrigin::SameCrate)
                .krate("sample")
                .build();
        }
        let segments: Vec<&str> = definition.split("::").collect();
        let (krate, rest) = segments.split_first().expect("split yields a segment");
        let (name, modules) = rest.split_last().expect("a definition has a name");
        let origin = match *krate == "sample" {
            true => DefinitionOrigin::SameCrate,
            false => DefinitionOrigin::OtherCrate,
        };

        Resolution::builder()
            .kind(DefinitionKind::Struct)
            .origin(origin)
            .krate(krate)
            .path(
                TypePath::builder()
                    .krate(krate)
                    .modules(modules)
                    .name(name)
                    .build(),
            )
            .build()
    }

    fn mark(node: &DecoratedNode<'_>, map: &mut DecorationMap, resolved: &[Resolved<'_>]) {
        let names_something = [
            "identifier",
            "type_identifier",
            "scoped_identifier",
            "scoped_type_identifier",
        ]
        .contains(&node.kind());
        let found = resolved.iter().find(|(text, _)| *text == node.text());
        if let (true, Some((_, definition))) = (names_something, found) {
            map.insert(node.id(), resolution(definition));
        }
        for child in node.named_children() {
            mark(&child, map, resolved);
        }
    }

    /// Attaches the resolution of each listed name to a tree
    fn resolve(tree: &mut DecoratedTree, resolved: &[Resolved<'_>]) {
        let mut map = DecorationMap::new();
        mark(&tree.root_node(), &mut map, resolved);
        decorate(tree, map);
    }

    fn run(source: &str, resolved: &[Resolved<'_>]) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        resolve(&mut tree, resolved);
        execute_rule(&tree)
    }

    /// Runs the rule as stiles runs it, with `allowed-crates` set
    fn run_with_allowed_crates(
        source: &str,
        allowed: &[&str],
        resolved: &[Resolved<'_>],
    ) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        resolve(&mut tree, resolved);
        let options = RuleOptions::new(vec![
            RuleOption::builder()
                .rule(RULE_ID.as_str().to_owned())
                .name(ALLOWED_CRATES)
                .values(allowed.iter().map(|name| (*name).to_owned()).collect())
                .build(),
        ]);

        let mut pass = RustLintPassAdapter::new(ExternalTypeInPort::default());
        pass.configure(&options);
        let mut passes: Vec<Box<dyn LintPass>> = vec![Box::new(pass)];

        execute(&tree, &mut passes)
    }

    /// Runs the rule on `source` as if it were the file at `path`
    fn run_at(path: &Path, source: &str, resolved: &[Resolved<'_>]) -> Vec<Diagnostic> {
        execute_rule(&parse_at(path, source, resolved))
    }

    /// Parses `source` as the file at `path`
    ///
    /// `parse` names every file `<test>`, so this parses with the grammar
    /// of `stiles-rust` and a path of the test's own.
    fn parse_at(path: &Path, source: &str, resolved: &[Resolved<'_>]) -> DecoratedTree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&stiles_rust::language())
            .expect("the grammar should load");
        let tree = parser.parse(source, None).expect("the source should parse");
        let mut tree = DecoratedTree::builder()
            .tree(tree)
            .language(Language::Rust)
            .source(source.to_owned())
            .file(PathBuf::from(path))
            .build();
        resolve(&mut tree, resolved);
        tree
    }

    /// Runs the rule after `attach` adds what the provider knows of test code
    fn run_attaching(
        source: &str,
        resolved: &[Resolved<'_>],
        attach: impl FnOnce(&mut DecoratedTree),
    ) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        resolve(&mut tree, resolved);
        attach(&mut tree);
        execute_rule(&tree)
    }

    /// Runs the rule on `tree` with the options left unset
    fn execute_rule(tree: &DecoratedTree) -> Vec<Diagnostic> {
        let mut passes: Vec<Box<dyn LintPass>> = vec![Box::new(RustLintPassAdapter::new(
            ExternalTypeInPort::default(),
        ))];
        execute(tree, &mut passes)
    }

    #[test]
    fn allowed_crate_is_not_flagged() {
        let source = r#"pub trait Port {
    fn run(&self) -> clawless::CommandResult;
}"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["clawless"],
            &[("clawless::CommandResult", "clawless::CommandResult")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn allowed_crate_leaves_another_crate_flagged() {
        let source = r#"pub trait Port {
    fn run(&self) -> anyhow::Result<()>;
}"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["clawless"],
            &[("anyhow::Result", "anyhow::Result")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn allowed_crate_through_a_glob_counts_by_the_defining_crate() {
        let source = r#"
use facade::*;

pub trait Port {
    fn port(&self, x: Thing);
}
"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["facade"],
            &[
                ("facade::Thing", "facade_core::thing::Thing"),
                ("facade", "<crate:facade>"),
                ("Thing", "facade_core::thing::Thing"),
            ],
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `Thing` from the external crate `facade_core`");
    }

    #[test]
    fn allowed_crate_written_in_a_use_is_not_flagged() {
        let source = r#"
use facade::Thing;

pub trait Port {
    fn port(&self, x: Thing);
}
"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["facade"],
            &[
                ("facade::Thing", "facade_core::thing::Thing"),
                ("facade", "<crate:facade>"),
                ("Thing", "facade_core::thing::Thing"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn allowed_crate_written_in_a_use_list_is_not_flagged() {
        let source = r#"
use facade::{Other, Thing};

pub trait Port {
    fn port(&self, x: Thing);
}
"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["facade"],
            &[
                ("facade", "<crate:facade>"),
                ("Thing", "facade_core::thing::Thing"),
                ("Other", "facade_core::other::Other"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn allowed_crate_written_in_place_is_not_flagged() {
        let source = r#"
pub trait Port {
    fn port(&self, x: facade::Thing);
}
"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["facade"],
            &[
                ("facade::Thing", "facade_core::thing::Thing"),
                ("facade", "<crate:facade>"),
                ("Thing", "facade_core::thing::Thing"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn allowed_crate_written_under_a_cargo_rename_is_not_flagged() {
        let source = r#"
pub trait Port {
    fn port(&self, x: renamed::Thing);
}
"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["renamed"],
            &[
                ("renamed::Thing", "real_core::Thing"),
                ("renamed", "<crate:real>"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn allowed_defining_crate_is_not_flagged() {
        let source = r#"pub trait Port {
    fn run(&self) -> facade::Thing;
}"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["facade_core"],
            &[("facade::Thing", "facade_core::thing::Thing")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn anyhow_result_return_diagnostic_has_rule_severity_and_message() {
        let source = r#"
pub trait DecorationProvider {
    fn decorate(&self, tree: &DecoratedTree) -> anyhow::Result<Coverage>;
}
"#;

        let diagnostics = run(source, &[ANYHOW_RESULT]);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.external-type-in-port")
            .has_severity(Severity::Error)
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

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn array_length_is_not_flagged() {
        let source = r#"pub trait Port { fn read(&self) -> [u8; limits::LEN]; }"#;

        let diagnostics = run(source, &[("limits::LEN", "limits::LEN")]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn associated_type_of_a_bounded_parameter_is_flagged_once_at_the_bound() {
        let source = r#"
pub trait Port {
    fn first<I: vfs::Walk>(&self, items: I) -> I::Entry;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("vfs::Walk", "ra_ap_vfs::walk::Walk"),
                ("I", "<param>"),
                ("I::Entry", "ra_ap_vfs::walk::Walk::Entry"),
            ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn associated_type_of_trait_generic_is_not_flagged() {
        let source = r#"pub trait Port<S> { fn read(&self) -> S::Output; }"#;

        let diagnostics = run(source, &[("S", "<param>")]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bare_local_type_is_not_flagged() {
        let source = r#"pub trait Port { fn read(&self, tree: &Tree) -> Coverage; }"#;

        let diagnostics = run(
            source,
            &[("Tree", "sample::Tree"), ("Coverage", "sample::Coverage")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bound_in_type_parameter_is_flagged_once() {
        let source = r#"
pub trait Port {
    fn run<S: ra_ap_hir::Semantics>(&self, semantics: S);
}
"#;

        let diagnostics = run(
            source,
            &[
                ("ra_ap_hir::Semantics", "ra_ap_hir::semantics::Semantics"),
                ("S", "<param>"),
            ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bound_in_type_parameter_message_names_the_bound_and_crate() {
        let source = r#"
pub trait Port {
    fn run<S: ra_ap_hir::Semantics>(&self, semantics: S);
}
"#;

        let diagnostics = run(
            source,
            &[
                ("ra_ap_hir::Semantics", "ra_ap_hir::semantics::Semantics"),
                ("S", "<param>"),
            ],
        );

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

        let diagnostics = run(
            source,
            &[("Serialize", "serde::ser::Serialize"), ("T", "<param>")],
        );

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

        let diagnostics = run(
            source,
            &[("Serialize", "serde::ser::Serialize"), ("T", "<param>")],
        );

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

        let diagnostics = run(
            source,
            &[
                ("serde::Deserialize", "serde::de::Deserialize"),
                ("T", "<param>"),
                (
                    "<T as serde::Deserialize>::Output",
                    "serde::de::Deserialize::Output",
                ),
            ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bracketed_type_reports_the_trait_inside() {
        let source = r#"
pub trait Port {
    fn read<T>(&self) -> <T as serde::Deserialize>::Output;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("serde::Deserialize", "serde::de::Deserialize"),
                ("T", "<param>"),
                (
                    "<T as serde::Deserialize>::Output",
                    "serde::de::Deserialize::Output",
                ),
            ],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("names `serde::Deserialize`");
    }

    #[test]
    fn crate_prefix_with_hyphen_returns_first_word() {
        assert_eq!(crate_prefix("stiles-rust"), "stiles");
    }

    #[test]
    fn crate_prefix_with_underscore_returns_first_word() {
        assert_eq!(crate_prefix("ra_ap_vfs"), "ra");
    }

    #[test]
    fn crate_prefix_without_separator_returns_whole_name() {
        assert_eq!(crate_prefix("stiles"), "stiles");
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

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn diagnostic_span_covers_the_type_path() {
        let source = "pub trait Port { fn read(&self) -> anyhow::Result<()>; }";
        let start = source
            .find("anyhow")
            .expect("source should contain the path");

        let diagnostics = run(source, &[ANYHOW_RESULT]);

        assert_diagnostic(&diagnostics[0])
            .has_span("<test>", start..start + "anyhow::Result".len());
    }

    #[test]
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run(pair.example(), &DOCUMENTED);

        assert!(!diagnostics.is_empty());
    }

    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run(pair.fix(), &DOCUMENTED);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn dyn_and_impl_trait_bounds_are_flagged_twice() {
        let source = r#"
pub trait Port {
    fn write(&self, sink: &mut dyn tokio::io::AsyncWrite) -> impl futures::Stream;
}
"#;

        let diagnostics = run(
            source,
            &[
                (
                    "tokio::io::AsyncWrite",
                    "tokio::io::async_write::AsyncWrite",
                ),
                ("futures::Stream", "futures_core::stream::Stream"),
            ],
        );

        assert_eq!(diagnostics.len(), 2);
    }

    /// The names of the documented example and fix, and their definitions
    const DOCUMENTED: [Resolved<'static>; 5] = [
        ANYHOW_RESULT,
        ("Result", "core::result::Result"),
        ("DecoratedTree", "sample::DecoratedTree"),
        ("Coverage", "sample::Coverage"),
        ("DecorateError", "sample::DecorateError"),
    ];

    #[test]
    fn dyn_and_impl_trait_bounds_are_flagged_with_the_defining_crate_of_the_impl_second() {
        let source = r#"
pub trait Port {
    fn write(&self, sink: &mut dyn tokio::io::AsyncWrite) -> impl futures::Stream;
}
"#;

        let diagnostics = run(
            source,
            &[
                (
                    "tokio::io::AsyncWrite",
                    "tokio::io::async_write::AsyncWrite",
                ),
                ("futures::Stream", "futures_core::stream::Stream"),
            ],
        );

        assert_diagnostic(&diagnostics[1])
            .message_contains("names `futures::Stream` from the external crate `futures_core`");
    }

    #[test]
    fn dyn_and_impl_trait_bounds_are_flagged_with_the_dyn_crate_first() {
        let source = r#"
pub trait Port {
    fn write(&self, sink: &mut dyn tokio::io::AsyncWrite) -> impl futures::Stream;
}
"#;

        let diagnostics = run(
            source,
            &[
                (
                    "tokio::io::AsyncWrite",
                    "tokio::io::async_write::AsyncWrite",
                ),
                ("futures::Stream", "futures_core::stream::Stream"),
            ],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `tokio`");
    }

    #[test]
    fn each_occurrence_is_flagged() {
        let source = r#"
use ra_ap_vfs::VfsPath;

pub trait Port {
    fn rename(&self, from: VfsPath) -> VfsPath;
}
"#;

        let diagnostics = run(source, &[("VfsPath", "ra_ap_vfs::vfs_path::VfsPath")]);

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn empty_source_produces_no_diagnostics() {
        let source = r#""#;

        let diagnostics = run(source, &[]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn file_in_tests_directory_without_a_target_is_flagged() {
        let source = r#"pub trait Port { fn read(&self) -> anyhow::Result<()>; }"#;

        let diagnostics = run_at(
            Path::new("crates/stiles/tests/fixtures/src/lib.rs"),
            source,
            &[("anyhow::Result", "anyhow::Result")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn file_that_only_integration_tests_compile_is_not_flagged() {
        let source = "pub trait Port { fn read(&self) -> anyhow::Result<()>; }";
        let mut tree = parse_at(
            Path::new("crates/stiles/tests/fixtures/src/lib.rs"),
            source,
            &[ANYHOW_RESULT],
        );
        test_decorations::target(&mut tree, [TargetKind::IntegrationTest]);

        let diagnostics = execute_rule(&tree);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn finding_names_the_crate_written_in_a_use() {
        let source = r#"
use facade::Thing;

pub trait Port {
    fn port(&self, x: Thing);
}
"#;

        let diagnostics = run(
            source,
            &[
                ("facade::Thing", "facade_core::thing::Thing"),
                ("facade", "<crate:facade>"),
                ("Thing", "facade_core::thing::Thing"),
            ],
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `Thing` from the external crate `facade`");
    }

    #[test]
    fn finding_names_the_crate_written_in_place() {
        let source = r#"
pub trait Port {
    fn port(&self, x: facade::Thing);
}
"#;

        let diagnostics = run(
            source,
            &[
                ("facade::Thing", "facade_core::thing::Thing"),
                ("facade", "<crate:facade>"),
                ("Thing", "facade_core::thing::Thing"),
            ],
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `facade::Thing` from the external crate `facade`");
    }

    #[test]
    fn function_pointer_parameter_is_flagged_once() {
        let source = r#"pub trait Port { fn visit(&self, f: fn(syn::Item)); }"#;

        let diagnostics = run(source, &[("syn::Item", "syn::item::Item")]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn function_pointer_parameter_message_names_the_parameter_type() {
        let source = r#"pub trait Port { fn visit(&self, f: fn(syn::Item)); }"#;

        let diagnostics = run(source, &[("syn::Item", "syn::item::Item")]);

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

        let diagnostics = run(
            source,
            &[
                ("VfsPath", "ra_ap_vfs::vfs_path::VfsPath"),
                ("Vec", "alloc::vec::Vec"),
                ("Option", "core::option::Option"),
            ],
        );

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

        let diagnostics = run(
            source,
            &[
                ("VfsPath", "ra_ap_vfs::vfs_path::VfsPath"),
                ("Vec", "alloc::vec::Vec"),
                ("Option", "core::option::Option"),
            ],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("names `VfsPath`");
    }

    #[test]
    fn glob_import_is_flagged_once() {
        let source = r#"
use ra_ap_vfs::*;

pub trait Port {
    fn read(&self) -> VfsPath;
}
"#;

        let diagnostics = run(source, &[("VfsPath", "ra_ap_vfs::vfs_path::VfsPath")]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn glob_import_message_names_the_crate() {
        let source = r#"
use ra_ap_vfs::*;

pub trait Port {
    fn read(&self) -> VfsPath;
}
"#;

        let diagnostics = run(source, &[("VfsPath", "ra_ap_vfs::vfs_path::VfsPath")]);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `VfsPath` from the external crate `ra_ap_vfs`");
    }

    #[test]
    fn glob_import_of_a_local_module_is_not_flagged() {
        let source = r#"
use super::*;

pub trait Port {
    fn read(&self) -> model::Coverage;
}
"#;

        let diagnostics = run(source, &[("model::Coverage", "sample::model::Coverage")]);

        assert_no_diagnostics(&diagnostics);
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

        let diagnostics = run(
            source,
            &[("tree_sitter::Language", "tree_sitter::Language")],
        );

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

        let diagnostics = run_attaching(source, &[("anyhow::Result", "anyhow::Result")], |tree| {
            test_decorations::mark(tree, "mod fixtures")
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn leading_colon_path_is_flagged_once() {
        let source = r#"pub trait Port { fn read(&self) -> ::anyhow::Error; }"#;

        let diagnostics = run(source, &[("::anyhow::Error", "anyhow::Error")]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn leading_colon_path_message_names_the_path_and_crate() {
        let source = r#"pub trait Port { fn read(&self) -> ::anyhow::Error; }"#;

        let diagnostics = run(source, &[("::anyhow::Error", "anyhow::Error")]);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `::anyhow::Error` from the external crate `anyhow`");
    }

    #[test]
    fn macro_in_type_position_is_not_flagged() {
        let source = r#"pub trait Port { fn read(&self) -> output!(anyhow::Error); }"#;

        let diagnostics = run(source, &[("anyhow::Error", "anyhow::Error")]);

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

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_generated_by_macro_is_not_flagged() {
        let source = r#"
pub trait Port {
    port_methods!(fn read(&self) -> anyhow::Result<()>;);
}
"#;

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

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

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

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

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

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

        let diagnostics = run_attaching(source, &[("anyhow::Result", "anyhow::Result")], |tree| {
            test_decorations::mark(tree, "fn fake")
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn non_sibling_crate_in_a_workspace_is_flagged_once() {
        let source = r#"
use stabby::IStable;

pub trait Port {
    fn read<T: IStable>(&self, value: T);
}
"#;

        let diagnostics = run_at(
            Path::new("crates/stiles-types/src/lib.rs"),
            source,
            &[
                ("IStable", "stabby_abi::istable::IStable"),
                ("T", "<param>"),
            ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn non_sibling_crate_in_a_workspace_message_names_the_defining_crate() {
        let source = r#"
use stabby::IStable;

pub trait Port {
    fn read<T: IStable>(&self, value: T);
}
"#;

        let diagnostics = run_at(
            Path::new("crates/stiles-types/src/lib.rs"),
            source,
            &[
                ("IStable", "stabby_abi::istable::IStable"),
                ("T", "<param>"),
            ],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `stabby_abi`");
    }

    #[test]
    fn private_trait_is_not_flagged() {
        let source = r#"trait Port { fn read(&self) -> anyhow::Result<()>; }"#;

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn qualified_path_is_flagged_once() {
        let source = r#"
pub trait Port {
    fn decorate(&self, file: &ra_ap_vfs::VfsPath) -> Coverage;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("ra_ap_vfs::VfsPath", "ra_ap_vfs::vfs_path::VfsPath"),
                ("Coverage", "sample::Coverage"),
            ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn qualified_path_message_names_method_path_and_crate() {
        let source = r#"
pub trait Port {
    fn decorate(&self, file: &ra_ap_vfs::VfsPath) -> Coverage;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("ra_ap_vfs::VfsPath", "ra_ap_vfs::vfs_path::VfsPath"),
                ("Coverage", "sample::Coverage"),
            ],
        );

        assert_diagnostic(&diagnostics[0]).message_contains(
            "port method `decorate` names `ra_ap_vfs::VfsPath` from the external crate `ra_ap_vfs`",
        );
    }

    #[cfg(feature = "plugin")]
    #[test]
    fn reads_returns_the_resolution_the_target_and_the_test_code() {
        let reads = stiles_plugin::DeclaresRules::reads(&ExternalTypeInPort::default());

        assert_eq!(
            reads,
            stiles_plugin::Reads::of::<Resolution>()
                .and::<stiles_rust::decorations::Target>()
                .and::<stiles_rust::decorations::TestCode>()
        );
    }

    #[test]
    fn renamed_crate_import_is_flagged_once() {
        let source = r#"
use ra_ap_vfs as vfs;

pub trait Port {
    fn read(&self, file: &vfs::VfsPath);
}
"#;

        let diagnostics = run(source, &[("vfs::VfsPath", "ra_ap_vfs::vfs_path::VfsPath")]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn renamed_crate_import_message_names_the_alias_path_and_real_crate() {
        let source = r#"
use ra_ap_vfs as vfs;

pub trait Port {
    fn read(&self, file: &vfs::VfsPath);
}
"#;

        let diagnostics = run(source, &[("vfs::VfsPath", "ra_ap_vfs::vfs_path::VfsPath")]);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `vfs::VfsPath` from the external crate `ra_ap_vfs`");
    }

    #[test]
    fn renamed_type_import_is_flagged_once() {
        let source = r#"
use ra_ap_vfs::VfsPath as File;

pub trait Port {
    fn read(&self, file: File);
}
"#;

        let diagnostics = run(source, &[("File", "ra_ap_vfs::vfs_path::VfsPath")]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn renamed_type_import_message_names_the_alias_and_real_crate() {
        let source = r#"
use ra_ap_vfs::VfsPath as File;

pub trait Port {
    fn read(&self, file: File);
}
"#;

        let diagnostics = run(source, &[("File", "ra_ap_vfs::vfs_path::VfsPath")]);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `File` from the external crate `ra_ap_vfs`");
    }

    #[test]
    fn restricted_trait_is_not_flagged() {
        let source = r#"pub(crate) trait Port { fn read(&self) -> anyhow::Result<()>; }"#;

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn same_crate_paths_are_not_flagged() {
        let source = r#"
use super::Tree;

pub trait Port {
    fn read(&self, tree: Tree) -> crate::Coverage;
    fn write(&self, value: self::Value) -> super::Outcome;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("Tree", "sample::Tree"),
                ("crate::Coverage", "sample::Coverage"),
                ("self::Value", "sample::port::Value"),
                ("super::Outcome", "sample::Outcome"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn same_crate_reexport_of_an_external_type_is_flagged() {
        let source = r#"
use crate::errors::Failure;

pub trait Port {
    fn read(&self) -> Failure;
}
"#;

        let diagnostics = run(source, &[("Failure", "anyhow::Error")]);

        assert_diagnostic(&diagnostics[0])
            .message_contains("names `Failure` from the external crate `anyhow`");
    }

    #[test]
    fn self_associated_type_is_not_flagged() {
        let source = r#"
pub trait Decoration {
    type Ref<'a>;

    fn lookup<'a>(node: &DecoratedNode<'a>) -> Self::Ref<'a>;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("Self", "<self>"),
                ("Self::Ref", "sample::Decoration::Ref"),
                ("DecoratedNode", "sample::DecoratedNode"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn sibling_crate_is_not_flagged() {
        let source = r#"
use stiles_types::RuleId;

pub trait DeclaresRules {
    fn rules(&self) -> Vec<RuleId>;
    fn language(&self) -> stiles_rust::Language;
}
"#;

        let diagnostics = run_at(
            Path::new("crates/stiles-plugin/src/declares_rules.rs"),
            source,
            &[
                ("RuleId", "stiles_types::rule_id::RuleId"),
                ("Vec", "alloc::vec::Vec"),
                ("stiles_rust::Language", "stiles_rust::language::Language"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn sibling_of_unprefixed_crate_directory_is_not_flagged() {
        let source = r#"pub trait Port { fn read(&self) -> stiles_types::RuleId; }"#;

        let diagnostics = run_at(
            Path::new("stiles/src/lib.rs"),
            source,
            &[("stiles_types::RuleId", "stiles_types::rule_id::RuleId")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn sibling_reexport_of_an_external_type_is_flagged() {
        let source = r#"pub trait Port { fn language(&self) -> stiles_rust::Language; }"#;

        let diagnostics = run_at(
            Path::new("crates/stiles-plugin/src/lib.rs"),
            source,
            &[("stiles_rust::Language", "tree_sitter::Language")],
        );

        assert_diagnostic(&diagnostics[0]).message_contains(
            "names `stiles_rust::Language` from the external crate `tree_sitter`",
        );
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

        let diagnostics = run(
            source,
            &[
                ("std::path::Path", "std::path::Path"),
                ("io::Result", "std::io::error::Result"),
                ("Arc", "alloc::sync::Arc"),
                ("core::fmt::Formatter", "core::fmt::Formatter"),
                ("::core::fmt::Result", "core::fmt::Result"),
            ],
        );

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

        let diagnostics = run_attaching(source, &[("anyhow::Result", "anyhow::Result")], |tree| {
            test_decorations::mark(tree, "mod tests")
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_in_inline_module_is_flagged_once() {
        let source = r#"
pub mod inner {
    use anyhow::Result;

    pub trait Port {
        fn read(&self) -> Result<()>;
    }
}
"#;

        let diagnostics = run(source, &[("Result", "anyhow::Result")]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_in_inline_module_message_names_the_crate() {
        let source = r#"
pub mod inner {
    use anyhow::Result;

    pub trait Port {
        fn read(&self) -> Result<()>;
    }
}
"#;

        let diagnostics = run(source, &[("Result", "anyhow::Result")]);

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `anyhow`");
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

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ExternalTypeInPort>();
    }

    #[test]
    fn trait_send_context() {
        fn assert_send<T: Send>() {}
        assert_send::<Context<'_>>();
    }

    #[test]
    fn trait_send_import() {
        fn assert_send<T: Send>() {}
        assert_send::<Import>();
    }

    #[test]
    fn trait_send_signature() {
        fn assert_send<T: Send>() {}
        assert_send::<Signature<'_, '_>>();
    }

    #[test]
    fn trait_send_written_crate() {
        fn assert_send<T: Send>() {}
        assert_send::<WrittenCrate>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ExternalTypeInPort>();
    }

    #[test]
    fn trait_sync_context() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Context<'_>>();
    }

    #[test]
    fn trait_sync_import() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Import>();
    }

    #[test]
    fn trait_sync_signature() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Signature<'_, '_>>();
    }

    #[test]
    fn trait_sync_written_crate() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<WrittenCrate>();
    }

    #[test]
    fn trait_under_cfg_any_test_or_unix_is_flagged_once() {
        let source = r#"
#[cfg(any(test, unix))]
pub trait Port {
    fn read(&self) -> anyhow::Result<()>;
}
"#;

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

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

        let diagnostics = run(source, &[("anyhow::Result", "anyhow::Result")]);

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

        let diagnostics = run_attaching(source, &[("anyhow::Result", "anyhow::Result")], |tree| {
            test_decorations::mark(tree, "pub trait Port")
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ExternalTypeInPort>();
    }

    #[test]
    fn trait_unpin_context() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Context<'_>>();
    }

    #[test]
    fn trait_unpin_import() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Import>();
    }

    #[test]
    fn trait_unpin_signature() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Signature<'_, '_>>();
    }

    #[test]
    fn trait_unpin_written_crate() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<WrittenCrate>();
    }

    #[test]
    fn type_binding_name_is_not_flagged() {
        let source = r#"
use ra_ap_vfs::Item;

pub trait Port {
    fn items(&self) -> impl Iterator<Item = u8>;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("Item", "ra_ap_vfs::Item"),
                ("Iterator", "core::iter::Iterator"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn type_binding_value_is_flagged_once() {
        let source = r#"
pub trait Port {
    fn items(&self) -> impl Iterator<Item = ra_ap_vfs::FileId>;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("ra_ap_vfs::FileId", "ra_ap_vfs::FileId"),
                ("Iterator", "core::iter::Iterator"),
            ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn type_binding_value_message_names_the_value_type() {
        let source = r#"
pub trait Port {
    fn items(&self) -> impl Iterator<Item = ra_ap_vfs::FileId>;
}
"#;

        let diagnostics = run(
            source,
            &[
                ("ra_ap_vfs::FileId", "ra_ap_vfs::FileId"),
                ("Iterator", "core::iter::Iterator"),
            ],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("names `ra_ap_vfs::FileId`");
    }

    #[test]
    fn type_parameter_is_not_flagged() {
        let source = r#"
pub trait Port {
    fn write<T>(&self, value: T);
}
"#;

        let diagnostics = run(source, &[("T", "<param>")]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn typed_receiver_is_flagged_once() {
        let source = r#"pub trait Port { fn read(self: triomphe::Arc<Self>); }"#;

        let diagnostics = run(
            source,
            &[("triomphe::Arc", "triomphe::arc::Arc"), ("Self", "<self>")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn typed_receiver_message_names_the_crate() {
        let source = r#"pub trait Port { fn read(self: triomphe::Arc<Self>); }"#;

        let diagnostics = run(
            source,
            &[("triomphe::Arc", "triomphe::arc::Arc"), ("Self", "<self>")],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("external crate `triomphe`");
    }

    #[test]
    fn unrelated_crate_without_workspace_is_flagged() {
        let source = r#"pub trait Port { fn read(&self) -> stiles_types::RuleId; }"#;

        let diagnostics = run_at(
            Path::new("src/lib.rs"),
            source,
            &[("stiles_types::RuleId", "stiles_types::rule_id::RuleId")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn unresolved_name_is_not_flagged() {
        let source = r#"pub trait Port { fn read(&self) -> anyhow::Result<()>; }"#;

        let diagnostics = run(source, &[]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn use_of_another_definition_leaves_the_defining_crate() {
        let source = r#"
use facade::Other;

pub trait Port {
    fn port(&self, x: Thing);
}
"#;

        let diagnostics = run_with_allowed_crates(
            source,
            &["facade"],
            &[
                ("facade", "<crate:facade>"),
                ("Other", "facade_core::other::Other"),
                ("Thing", "facade_core::thing::Thing"),
            ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn workspace_prefix_with_crate_directory_returns_prefix() {
        let prefix = workspace_prefix(Path::new("./crates/stiles-rust/src/lint/pass.rs"));

        assert_eq!(prefix, Some("stiles"));
    }

    #[test]
    fn workspace_prefix_without_src_returns_none() {
        let prefix = workspace_prefix(Path::new("build.rs"));

        assert_eq!(prefix, None);
    }
}
