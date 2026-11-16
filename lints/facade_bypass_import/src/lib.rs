use std::collections::BTreeSet;
use std::fmt;

use stiles_rust::decorations::{DefinitionKind, PublicPath, Reexports, Resolution};
use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(facade_bypass_import::RULE_ID.as_str(), "lint.facade-bypass-import");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.facade-bypass-import");

/// The sysroot crates, which are never a facade and never bypass one
///
/// `core` defines what `std` and `alloc` re-export, so the decoration on
/// `use core::fmt` lists `std::fmt`. That is not a facade bypass, and
/// neither is a `std` path beside a crate that re-exports `std` items.
const SYSROOT: &[&str] = &["std", "core", "alloc"];

/// Flags an import that goes around a facade the same file imports from
///
/// A facade crate re-exports its constituents so that a consumer names one
/// dependency and one import root. A file that writes
/// `use facade_core::Thing` beside `use facade::prelude::*` has two paths
/// to the same items. It also breaks when the facade moves a module.
///
/// Two imports in one file do not prove that one crate re-exports the
/// other, so the rule reads the [`Reexports`] decoration. The Rust provider
/// puts it on each path a `use` imports. It lists the shortest public path
/// to the item through each other direct dependency that exposes it. The
/// rule reports an imported path when its crate is not in that list and a
/// listed crate is one the file also imports from. The diagnostic names
/// the path through that crate. A path without the decoration is not
/// reported.
///
/// The crate of a path is the crate its first segment resolves to, which the
/// rule reads from the [`Resolution`] decoration. A first segment that
/// resolves to a module or another item names no crate. A rename and an
/// `extern crate ... as` name the crate they bind. A crate also goes by the
/// name the path spells, which is how `Cargo.toml` renames a dependency.
/// The keywords `crate`, `self`, and `super` name none. The rule reads `use`
/// declarations anywhere in the file, but not in a macro body. It never
/// treats `std`, `core`, or `alloc` as either side.
///
/// # Examples
///
/// ```text
/// // DON'T
/// use bevy::prelude::*;
/// use bevy_ecs::system::Query;
///
/// // DO
/// use bevy::prelude::*;
/// use bevy::ecs::system::Query;
/// ```
///
/// The rule reads the public paths and the resolutions the Rust provider
/// attaches as decorations. This example attaches them by hand.
///
/// ```
/// use facade_bypass_import::FacadeBypassImport;
/// use stiles_rust::RustLintPassAdapter;
/// use stiles_rust::decorations::{
///     DefinitionKind, DefinitionOrigin, PublicPath, Reexports, Resolution,
/// };
/// use stiles_testing::{decorate, execute, parse};
/// use stiles_types::{DecorationMap, Language, LintPass};
///
/// let source = "use facade::Other;\nuse facade_core::thing::Thing;";
/// let mut tree = parse(source, Language::Rust);
/// let root = tree.root_node();
/// let other = root.named_child(0).unwrap().child_by_field_name("argument").unwrap();
/// let path = root.named_child(1).unwrap().child_by_field_name("argument").unwrap();
/// let facade = other.child_by_field_name("path").unwrap();
/// let thing = path.child_by_field_name("path").unwrap();
/// let facade_core = thing.child_by_field_name("path").unwrap();
/// let crate_named = |name| {
///     Resolution::builder()
///         .kind(DefinitionKind::Crate)
///         .origin(DefinitionOrigin::OtherCrate)
///         .krate(name)
///         .build()
/// };
/// let through_facade = PublicPath::builder()
///     .krate("facade")
///     .segments(["thing", "Thing"])
///     .build();
/// let mut decorations = DecorationMap::new();
/// decorations.insert(path.id(), Reexports::new([through_facade]));
/// decorations.insert(facade.id(), crate_named("facade"));
/// decorations.insert(facade_core.id(), crate_named("facade_core"));
/// decorate(&mut tree, decorations);
/// let pass: Box<dyn LintPass> = Box::new(RustLintPassAdapter::new(FacadeBypassImport));
/// let mut passes = vec![pass];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct FacadeBypassImport;

impl RustLintPass for FacadeBypassImport {
    fn check_source_file(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let mut imports = FileImports::default();
        imports.walk(node);
        imports.report()
    }
}

/// Whether a path starts behind a leading `::`
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
enum Anchor {
    /// A leading `::`, as in `::facade::x`
    Global,
    /// No leading `::`, as in `facade::x`
    #[default]
    Bare,
}

/// The whole path to one element of a use tree, as the file spells it
///
/// A grouped import spells its prefix once, so the path to `b` in
/// `use a::{b}` carries `a` from the group.
#[derive(Clone, Debug, Default)]
struct UsePath<'a> {
    anchor: Anchor,
    segments: Vec<DecoratedNode<'a>>,
}

impl<'a> UsePath<'a> {
    /// Returns this path with the segments of `node` after it
    fn extended(&self, node: &DecoratedNode<'a>) -> Self {
        let mut path = self.clone();
        path.push(node);
        path
    }

    /// Returns this path behind a leading `::`
    fn global(&self) -> Self {
        Self {
            anchor: Anchor::Global,
            segments: self.segments.clone(),
        }
    }

    /// Appends the segments a path node spells
    ///
    /// A `scoped_identifier` without a qualifier is `::name`, and at the
    /// start of a path that `::` is spelled before the first segment.
    fn push(&mut self, node: &DecoratedNode<'a>) {
        match node.kind() {
            "identifier" | "crate" | "self" | "super" | "metavariable" => self.segments.push(*node),
            "scoped_identifier" => {
                match node.child_by_field_name("path") {
                    Some(qualifier) => self.push(&qualifier),
                    None => {
                        if self.segments.is_empty() {
                            self.anchor = Anchor::Global;
                        }
                    }
                }
                if let Some(name) = node.child_by_field_name("name") {
                    self.push(&name);
                }
            }
            _ => {}
        }
    }

    /// Returns the names of the crate the first segment resolves to
    ///
    /// The crate goes by its own name and by the name the path spells. They
    /// differ for an alias in the file and for a rename in `Cargo.toml`.
    fn root(&self) -> Option<CrateNames<'a>> {
        let first = self.segments.first()?;
        let resolution = first.decoration::<Resolution>()?;
        match resolution.kind() {
            DefinitionKind::Crate => Some(CrateNames {
                own: resolution.krate()?,
                written: unraw(first.text()),
            }),
            DefinitionKind::Module
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
            | DefinitionKind::TypeParameter
            | DefinitionKind::ConstParameter
            | DefinitionKind::SelfType
            | DefinitionKind::BuiltinType
            | DefinitionKind::BuiltinAttribute
            | DefinitionKind::ToolModule
            | DefinitionKind::DeriveHelper => None,
        }
    }
}

/// The two names a crate at the start of a path goes by
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
struct CrateNames<'a> {
    own: &'a str,
    written: &'a str,
}

impl<'a> CrateNames<'a> {
    /// Returns both names
    fn both(self) -> [&'a str; 2] {
        [self.own, self.written]
    }
}

/// Renders the path the way the file spells it
impl fmt::Display for UsePath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.anchor {
            Anchor::Global => write!(f, "::")?,
            Anchor::Bare => {}
        }
        let texts: Vec<&str> = self.segments.iter().map(DecoratedNode::text).collect();
        write!(f, "{}", texts.join("::"))
    }
}

/// A path a `use` imports, with the node the provider decorates
#[derive(Clone, Debug)]
struct ImportedPath<'a> {
    node: DecoratedNode<'a>,
    path: UsePath<'a>,
}

/// The paths a file imports and the crates it imports from
#[derive(Clone, Debug, Default)]
struct FileImports<'a> {
    imported: Vec<ImportedPath<'a>>,
    roots: BTreeSet<&'a str>,
}

impl<'a> FileImports<'a> {
    /// Collects the imports under a node
    ///
    /// A macro body is a token tree to tree-sitter, so a `use` inside one
    /// is not seen.
    fn walk(&mut self, node: &DecoratedNode<'a>) {
        if node.kind() == "use_declaration" {
            if let Some(argument) = node.child_by_field_name("argument") {
                self.walk_use_tree(&argument, &UsePath::default());
            }
            return;
        }

        for child in node.named_children() {
            self.walk(&child);
        }
    }

    /// Collects what one element of a use tree imports
    ///
    /// The provider decorates the same elements this records as imported
    /// paths: a plain or renamed path, but not a wildcard or a `self`.
    fn walk_use_tree(&mut self, node: &DecoratedNode<'a>, prefix: &UsePath<'a>) {
        match node.kind() {
            "identifier" | "scoped_identifier" => {
                let path = prefix.extended(node);
                self.import(*node, path);
            }
            "use_as_clause" => {
                let Some(target) = node.child_by_field_name("path") else {
                    return;
                };
                let path = prefix.extended(&target);
                self.import(target, path);
            }
            "use_wildcard" => {
                let path = match node.named_child(0) {
                    Some(target) => prefix.extended(&target),
                    None => prefix.clone(),
                };
                self.add_root(&path);
            }
            "scoped_use_list" => {
                let Some(list) = node.child_by_field_name("list") else {
                    return;
                };
                let path = match node.child_by_field_name("path") {
                    Some(target) => prefix.extended(&target),
                    None => prefix.global(),
                };
                self.walk_use_tree(&list, &path);
            }
            "use_list" => {
                for child in node.named_children() {
                    self.walk_use_tree(&child, prefix);
                }
            }
            "self" => self.add_root(prefix),
            _ => {}
        }
    }

    /// Records a path a `use` imports and the crate it starts at
    fn import(&mut self, node: DecoratedNode<'a>, path: UsePath<'a>) {
        self.add_root(&path);
        self.imported.push(ImportedPath { node, path });
    }

    /// Records the crate a path starts at, if it starts at one
    fn add_root(&mut self, path: &UsePath<'a>) {
        if let Some(root) = path.root() {
            self.roots.extend(root.both());
        }
    }

    /// Returns a diagnostic for each imported path that bypasses a facade
    /// the file imports from
    fn report(&self) -> Vec<Diagnostic> {
        self.imported
            .iter()
            .filter_map(|imported| bypass(imported, &self.roots))
            .collect()
    }
}

/// Returns a diagnostic when an imported path bypasses a facade in `crates`
fn bypass(imported: &ImportedPath<'_>, crates: &BTreeSet<&str>) -> Option<Diagnostic> {
    let ImportedPath { node, path } = imported;
    let root = path.root()?;
    if root.both().iter().any(|name| SYSROOT.contains(name)) {
        return None;
    }
    let reexports = node.get::<Reexports>()?;
    if root
        .both()
        .iter()
        .any(|name| reexports.through(name).is_some())
    {
        return None;
    }
    let facade = facade_path(reexports, crates)?;
    let krate = facade.krate();

    Some(
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Error)
            .message(format!(
                "`{path}` is also exported as `{facade}`, and this file already imports from \
                 `{krate}`; import it through `{krate}`"
            ))
            .span(node.span())
            .build(),
    )
}

/// Returns the first listed path through a crate the file imports from
///
/// A sysroot crate never counts, because a `std` path is not a facade.
fn facade_path<'r>(reexports: &'r Reexports, crates: &BTreeSet<&str>) -> Option<&'r PublicPath> {
    reexports
        .paths()
        .iter()
        .find(|path| !SYSROOT.contains(&path.krate()) && crates.contains(path.krate()))
}

/// Returns the name an identifier spells, without an `r#` prefix
fn unraw(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for FacadeBypassImport {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.facade-bypass-import")]
    }

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::of::<Reexports>().and::<Resolution>()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [FacadeBypassImport]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_rust::decorations::DefinitionOrigin;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use stiles_types::{DecorationMap, Language, LintPass};

    use super::*;

    /// Runs the rule with a [`Reexports`] on each path that spells a key
    ///
    /// The rule reads decorations the Rust provider makes, and these tests
    /// run no provider, so they attach them by hand. Each key names the text
    /// of an imported path, and its value lists the public paths to put on
    /// it, written as `krate::segment::Item`. Every identifier resolves to
    /// the crate it spells, as [`resolve`] describes.
    fn run(source: &str, decorations: &[(&str, &[&str])]) -> Vec<Diagnostic> {
        run_naming(source, decorations, &[])
    }

    /// Runs the rule as [`run`] does, with the identifiers in `names`
    /// resolving as listed
    fn run_naming(
        source: &str,
        decorations: &[(&str, &[&str])],
        names: &[(&str, Option<Resolution>)],
    ) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        let mut map = DecorationMap::new();
        resolve(&tree.root_node(), names, &mut map);
        for (text, paths) in decorations {
            let mut ids = Vec::new();
            collect_path_ids(&tree.root_node(), text, &mut ids);
            for id in ids {
                map.insert(id, reexports(paths));
            }
        }
        decorate(&mut tree, map);

        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(FacadeBypassImport))];
        execute(&tree, &mut passes)
    }

    /// Returns the source text under each diagnostic, in order
    fn flagged<'s>(source: &'s str, decorations: &[(&str, &[&str])]) -> Vec<&'s str> {
        run(source, decorations)
            .iter()
            .map(|diagnostic| &source[diagnostic.span().start()..diagnostic.span().end()])
            .collect()
    }

    /// Attaches a [`Resolution`] to every identifier under `node`
    ///
    /// An identifier that `names` lists resolves as listed, and [`None`]
    /// leaves it bare. Every other identifier resolves to the crate it
    /// spells, without an `r#` prefix. The keywords `crate`, `self`, and
    /// `super` are not identifiers, so they stay bare as the provider leaves
    /// them.
    fn resolve(
        node: &DecoratedNode<'_>,
        names: &[(&str, Option<Resolution>)],
        map: &mut DecorationMap,
    ) {
        if node.kind() == "identifier" {
            let text = node.text();
            let resolution = match names.iter().find(|(name, _)| *name == text) {
                Some((_, listed)) => listed.clone(),
                None => Some(crate_named(text.strip_prefix("r#").unwrap_or(text))),
            };
            if let Some(resolution) = resolution {
                map.insert(node.id(), resolution);
            }
        }
        for child in node.named_children() {
            resolve(&child, names, map);
        }
    }

    fn crate_named(name: &str) -> Resolution {
        Resolution::builder()
            .kind(DefinitionKind::Crate)
            .origin(DefinitionOrigin::OtherCrate)
            .krate(name)
            .build()
    }

    /// Returns the resolution of a name to a module of the crate `krate`
    fn module_of(krate: &str, origin: DefinitionOrigin) -> Option<Resolution> {
        Some(
            Resolution::builder()
                .kind(DefinitionKind::Module)
                .origin(origin)
                .krate(krate)
                .build(),
        )
    }

    fn collect_path_ids(node: &DecoratedNode<'_>, text: &str, ids: &mut Vec<usize>) {
        match node.kind() {
            "identifier" | "scoped_identifier" if node.text() == text => ids.push(node.id()),
            _ => {}
        }
        for child in node.named_children() {
            collect_path_ids(&child, text, ids);
        }
    }

    fn reexports(paths: &[&str]) -> Reexports {
        Reexports::new(paths.iter().map(|path| {
            let mut segments = path.split("::");
            let krate = segments.next().expect("a path names a crate");
            PublicPath::builder()
                .krate(krate)
                .segments(segments)
                .build()
        }))
    }

    const THING: (&str, &[&str]) = ("facade_core::thing::Thing", &["facade::thing::Thing"]);

    #[test]
    fn alias_named_like_the_constituent_names_the_crate_it_binds() {
        let source = "use other as facade_core;\nuse facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[(
                "facade_core::thing::Thing",
                &["other::thing::Thing", "facade::thing::Thing"],
            )],
            &[("facade_core", Some(crate_named("other")))],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn aliased_facade_counts_as_an_import() {
        let source = "use facade as f;\nuse facade_core::thing::Thing;";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn alloc_is_never_the_facade() {
        let source = "use alloc::vec::Vec;\nuse facade_core::thing::Thing;";

        let diagnostics = run(source, &[("facade_core::thing::Thing", &["alloc::Thing"])]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn alloc_path_is_not_flagged() {
        let source = "use facade::x;\nuse alloc::vec::Vec;";

        let diagnostics = run(source, &[("alloc::vec::Vec", &["facade::Vec"])]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constituent_beside_a_facade_wildcard_is_flagged() {
        let source = "use facade::prelude::*;\nuse facade_core::thing::Thing;";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn constituent_beside_the_facade_reports_the_facade_path() {
        let source = "use facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run(source, &[THING]);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.facade-bypass-import")
            .has_severity(Severity::Error)
            .message_contains(
                "`facade_core::thing::Thing` is also exported as `facade::thing::Thing`, and \
                 this file already imports from `facade`; import it through `facade`",
            )
            .has_span("<test>", 19..44);
    }

    #[test]
    fn constituent_without_a_facade_import_is_not_flagged() {
        let source = "use facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
    }

    #[test]
    fn core_is_never_the_facade() {
        let source = "use core::fmt;\nuse facade_core::thing::Thing;";

        let diagnostics = run(source, &[("facade_core::thing::Thing", &["core::Thing"])]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn core_path_beside_a_facade_is_not_flagged() {
        let source = "use facade::x;\nuse core::fmt::Debug;";

        let diagnostics = run(source, &[("core::fmt::Debug", &["facade::Debug"])]);

        assert_no_diagnostics(&diagnostics);
    }

    /// Pins that `use core::fmt` is not a bypass of `std`
    ///
    /// The provider lists the `std` and `alloc` paths to every `core`
    /// item, and a file that imports `std` as well is common.
    #[test]
    fn core_path_beside_std_is_not_flagged() {
        let source = "use std::io;\nuse core::fmt;";

        let diagnostics = run(source, &[("core::fmt", &["alloc::fmt", "std::fmt"])]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn crate_relative_path_is_not_flagged() {
        let source = "use facade::x;\nuse crate::thing::Thing;";

        let diagnostics = run(
            source,
            &[("crate::thing::Thing", &["facade::thing::Thing"])],
        );

        assert_no_diagnostics(&diagnostics);
    }

    fn run_documented(source: &str) -> Vec<stiles_types::Diagnostic> {
        let mut tree = stiles_testing::parse(source, stiles_types::Language::Rust);
        let decorations = documented_decorations(&tree);
        stiles_testing::decorate(&mut tree, decorations);
        stiles_testing::execute(
            &tree,
            &mut [Box::new(stiles_rust::RustLintPassAdapter::new(
                FacadeBypassImport,
            ))],
        )
    }

    fn documented_decorations(tree: &stiles_types::DecoratedTree) -> stiles_types::DecorationMap {
        let mut decorations = stiles_types::DecorationMap::new();
        resolve(&tree.root_node(), &[], &mut decorations);
        for import in documented_nodes(tree, "use_declaration") {
            let path = import
                .child_by_field_name("argument")
                .expect("`use` should name a path");
            if path.text().starts_with("bevy_ecs::") {
                decorations.insert(path.id(), reexports(&["bevy::ecs::system::Query"]));
            }
        }
        decorations
    }

    fn documented_nodes<'a>(
        tree: &'a stiles_types::DecoratedTree,
        kind: &str,
    ) -> Vec<stiles_types::DecoratedNode<'a>> {
        let mut pending = vec![tree.root_node()];
        let mut found = Vec::new();
        while let Some(node) = pending.pop() {
            if node.kind() == kind {
                found.push(node);
            }
            pending.extend(node.named_children());
        }
        found
    }

    #[test]
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_documented(pair.example());

        assert!(!diagnostics.is_empty());
    }

    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_documented(pair.fix());

        stiles_testing::assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn each_bypassing_path_is_flagged() {
        let source = "use facade::x;\nuse facade_core::thing::Thing;\nuse facade_cli::Args;";

        let diagnostics = run(
            source,
            &[THING, ("facade_cli::Args", &["facade::cli::Args"])],
        );

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn empty_list_is_not_flagged() {
        let source = "use facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run(source, &[("facade_core::thing::Thing", &[])]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn enum_named_like_the_constituent_is_not_flagged() {
        let source = "enum facade_core { A }\nuse facade::x;\nuse facade_core::thing::Thing;";
        let local_enum = Resolution::builder()
            .kind(DefinitionKind::Enum)
            .origin(DefinitionOrigin::SameCrate)
            .krate("sample")
            .build();

        let diagnostics = run_naming(source, &[THING], &[("facade_core", Some(local_enum))]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn extern_crate_alias_named_like_the_constituent_names_the_crate_it_binds() {
        let source =
            "extern crate other as facade_core;\nuse facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[(
                "facade_core::thing::Thing",
                &["other::thing::Thing", "facade::thing::Thing"],
            )],
            &[("facade_core", Some(crate_named("other")))],
        );

        assert_no_diagnostics(&diagnostics);
    }

    /// Pins that only a `use` counts as importing from the facade
    #[test]
    fn extern_crate_facade_is_not_an_import() {
        let source = "extern crate facade;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
    }

    #[test]
    fn facade_import_in_a_function_body_counts() {
        let source = "use facade_core::thing::Thing;\nfn f() { use facade::x; }";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn facade_import_in_a_macro_body_does_not_count() {
        let source = "macro_rules! m { () => { use facade::x; }; }\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
    }

    #[test]
    fn facade_import_in_a_nested_module_counts() {
        let source = "use facade_core::thing::Thing;\nmod tests { use facade::x; }";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn facade_list_self_counts_as_an_import() {
        let source = "use facade::{self};\nuse facade_core::thing::Thing;";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn facade_renamed_in_the_manifest_counts_as_an_import() {
        let source = "use bevy_alias::prelude::*;\nuse bevy_ecs::system::Query;";

        let diagnostics = run_naming(
            source,
            &[(
                "bevy_ecs::system::Query",
                &["bevy_alias::ecs::system::Query"],
            )],
            &[("bevy_alias", Some(crate_named("bevy")))],
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("exported as `bevy_alias::ecs::system::Query`");
    }

    /// Pins the serde case that made a syntax-only rule unjustifiable
    ///
    /// `serde_json` re-exports nothing of `serde`, so the provider lists no
    /// `serde` path on a `serde_json` import.
    #[test]
    fn family_prefix_without_a_reexport_is_not_flagged() {
        let source = "use serde::Serialize;\nuse serde_json::Value;";

        let diagnostics = run(
            source,
            &[("serde_json::Value", &[]), ("serde::Serialize", &[])],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn first_listed_imported_crate_is_named() {
        let source = "use facade::x;\nuse other::y;\nuse facade_core::thing::Thing;";

        let diagnostics = run(
            source,
            &[(
                "facade_core::thing::Thing",
                &["elsewhere::Thing", "other::Thing", "facade::thing::Thing"],
            )],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("exported as `other::Thing`");
    }

    #[test]
    fn global_group_is_flagged() {
        let source = "use ::{facade::x, facade_core::thing::Thing};";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn global_path_beside_a_module_of_its_name_is_flagged() {
        let source = "mod facade_core {}\nuse facade::x;\nuse ::facade_core::thing::Thing;";

        let diagnostics = run(
            source,
            &[("::facade_core::thing::Thing", &["facade::thing::Thing"])],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("`::facade_core::thing::Thing` is");
    }

    #[test]
    fn group_at_the_root_is_flagged() {
        let source = "use {facade::x, facade_core::thing::Thing};";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn grouped_path_reports_the_whole_path() {
        let source = "use facade::x;\nuse facade_core::{thing::Thing, Other};";

        let diagnostics = run(source, &[("thing::Thing", &["facade::thing::Thing"])]);

        assert_diagnostic(&diagnostics[0])
            .message_contains("`facade_core::thing::Thing` is also exported")
            .has_span("<test>", 33..45);
    }

    #[test]
    fn grouped_single_name_is_flagged() {
        let source = "use facade::x;\nuse facade_core::thing::{Other, Thing};";

        assert_eq!(flagged(source, &[("Thing", &["facade::Thing"])]), ["Thing"]);
    }

    #[test]
    fn import_through_a_listed_crate_is_not_flagged() {
        let source = "use other::y;\nuse facade::thing::Thing;";

        let diagnostics = run(
            source,
            &[(
                "facade::thing::Thing",
                &["facade::thing::Thing", "other::Thing"],
            )],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn listed_crate_the_file_does_not_import_is_not_flagged() {
        let source = "use other::y;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
    }

    #[test]
    fn module_from_a_glob_named_like_the_constituent_is_not_flagged() {
        let source = "use other::*;\nuse facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[THING],
            &[(
                "facade_core",
                module_of("other", DefinitionOrigin::OtherCrate),
            )],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn module_named_like_the_constituent_is_not_flagged() {
        let source = "mod facade_core;\nuse facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[THING],
            &[(
                "facade_core",
                module_of("sample", DefinitionOrigin::SameCrate),
            )],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn module_named_like_the_facade_is_not_an_import() {
        let source = "mod facade {}\nuse facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[THING],
            &[("facade", module_of("sample", DefinitionOrigin::SameCrate))],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_group_self_named_like_the_constituent_is_not_flagged() {
        let source =
            "use crate::{facade_core::{self}};\nuse facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[THING],
            &[(
                "facade_core",
                module_of("sample", DefinitionOrigin::SameCrate),
            )],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn path_through_a_crate_alias_is_flagged() {
        let source = "use facade_core as fc;\nuse facade::x;\nuse fc::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[("fc::thing::Thing", &["facade::thing::Thing"])],
            &[("fc", Some(crate_named("facade_core")))],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("`fc::thing::Thing` is also exported");
    }

    #[test]
    fn pub_use_is_flagged() {
        let source = "use facade::x;\npub use facade_core::thing::Thing;";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn raw_identifier_crates_are_compared_unraw() {
        let source = "use r#facade::x;\nuse r#facade_core::thing::Thing;";

        let diagnostics = run(
            source,
            &[("r#facade_core::thing::Thing", &["facade::thing::Thing"])],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[cfg(feature = "plugin")]
    #[test]
    fn reads_returns_the_reexports_and_the_resolution() {
        let reads = stiles_plugin::DeclaresRules::reads(&FacadeBypassImport);

        assert_eq!(
            reads,
            stiles_plugin::Reads::of::<Reexports>().and::<Resolution>()
        );
    }

    #[test]
    fn renamed_path_is_flagged() {
        let source = "use facade::x;\nuse facade_core::thing::Thing as Other;";

        assert_eq!(flagged(source, &[THING]), ["facade_core::thing::Thing"]);
    }

    #[test]
    fn self_relative_path_is_not_flagged() {
        let source = "use facade::x;\nuse self::thing::Thing;";

        let diagnostics = run(source, &[("self::thing::Thing", &["facade::thing::Thing"])]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn single_segment_crate_is_flagged() {
        let source = "use facade::x;\nuse facade_core;";

        let diagnostics = run(source, &[("facade_core", &["facade::facade_core"])]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn std_is_never_the_facade() {
        let source = "use std::fmt;\nuse facade_core::thing::Thing;";

        let diagnostics = run(source, &[("facade_core::thing::Thing", &["std::Thing"])]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn std_path_is_not_flagged() {
        let source = "use facade::x;\nuse std::fmt::Debug;";

        let diagnostics = run(source, &[("std::fmt::Debug", &["facade::Debug"])]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn super_relative_path_is_not_flagged() {
        let source = "use facade::x;\nuse super::thing::Thing;";

        let diagnostics = run(
            source,
            &[("super::thing::Thing", &["facade::thing::Thing"])],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send_holds_for_anchor() {
        fn assert_send<T: Send>() {}
        assert_send::<Anchor>();
    }

    #[test]
    fn trait_send_holds_for_crate_names() {
        fn assert_send<T: Send>() {}
        assert_send::<CrateNames<'_>>();
    }

    #[test]
    fn trait_send_holds_for_facade_bypass_import() {
        fn assert_send<T: Send>() {}
        assert_send::<FacadeBypassImport>();
    }

    #[test]
    fn trait_send_holds_for_file_imports() {
        fn assert_send<T: Send>() {}
        assert_send::<FileImports<'_>>();
    }

    #[test]
    fn trait_send_holds_for_imported_path() {
        fn assert_send<T: Send>() {}
        assert_send::<ImportedPath<'_>>();
    }

    #[test]
    fn trait_send_holds_for_use_path() {
        fn assert_send<T: Send>() {}
        assert_send::<UsePath<'_>>();
    }

    #[test]
    fn trait_sync_holds_for_anchor() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Anchor>();
    }

    #[test]
    fn trait_sync_holds_for_crate_names() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<CrateNames<'_>>();
    }

    #[test]
    fn trait_sync_holds_for_facade_bypass_import() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FacadeBypassImport>();
    }

    #[test]
    fn trait_sync_holds_for_file_imports() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FileImports<'_>>();
    }

    #[test]
    fn trait_sync_holds_for_imported_path() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ImportedPath<'_>>();
    }

    #[test]
    fn trait_sync_holds_for_use_path() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<UsePath<'_>>();
    }

    #[test]
    fn trait_unpin_holds_for_anchor() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Anchor>();
    }

    #[test]
    fn trait_unpin_holds_for_crate_names() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<CrateNames<'_>>();
    }

    #[test]
    fn trait_unpin_holds_for_facade_bypass_import() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<FacadeBypassImport>();
    }

    #[test]
    fn trait_unpin_holds_for_file_imports() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<FileImports<'_>>();
    }

    #[test]
    fn trait_unpin_holds_for_imported_path() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ImportedPath<'_>>();
    }

    #[test]
    fn trait_unpin_holds_for_use_path() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<UsePath<'_>>();
    }

    #[test]
    fn undecorated_path_is_not_flagged() {
        let source = "use facade::x;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[]));
    }

    /// Pins the shape stiles's own `use anyhow::Context as _` takes
    #[test]
    fn underscore_rename_is_flagged() {
        let source = "use facade::x;\nuse facade_core::Context as _;";

        let flagged = flagged(
            source,
            &[("facade_core::Context", &["facade::ErrorContext"])],
        );

        assert_eq!(flagged, ["facade_core::Context"]);
    }

    #[test]
    fn unresolved_first_segment_is_not_flagged() {
        let source = "use facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(source, &[THING], &[("facade_core", None)]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn use_binding_named_like_the_constituent_is_not_flagged() {
        let source = "use other::facade_core;\nuse facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[THING],
            &[(
                "facade_core",
                module_of("other", DefinitionOrigin::OtherCrate),
            )],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn use_binding_named_like_the_facade_is_not_an_import() {
        let source = "use other::facade;\nuse facade::x;\nuse facade_core::thing::Thing;";

        let diagnostics = run_naming(
            source,
            &[THING],
            &[("facade", module_of("other", DefinitionOrigin::OtherCrate))],
        );

        assert_no_diagnostics(&diagnostics);
    }
}
