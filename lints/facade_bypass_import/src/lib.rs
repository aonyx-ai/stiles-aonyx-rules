use std::collections::BTreeSet;
use std::fmt;

use whisker_rust::decorations::{PublicPath, Reexports};
use whisker_rust::{RuleId, RustLintPass};
use whisker_types::{DecoratedNode, Diagnostic, Severity};

const RULE_ID: RuleId = RuleId::new("lint.facade-bypass-import");

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
/// The crate of a path is its first segment, so `crate::`, `self::`, and
/// `super::` paths name none. A bare first segment that the file binds
/// itself, with `mod`, `enum`, `use`, or `extern crate ... as`, names that
/// binding and not a crate. A leading `::` always names a crate. The rule
/// reads `use` declarations anywhere in the file, but not in a macro body.
/// It never treats `std`, `core`, or `alloc` as either side.
pub struct FacadeBypassImport;

impl RustLintPass for FacadeBypassImport {
    fn check_source_file(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let mut imports = FileImports::default();
        imports.walk(node);
        imports.report()
    }
}

/// Whether the first segment of a path can name something other than a
/// crate
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
enum Anchor {
    /// A leading `::`, which names a crate and nothing else
    Crate,
    /// A bare first segment, which a name the file binds can shadow
    #[default]
    Scope,
}

/// Whether a path segment is a name or a keyword
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum SegmentKind {
    /// An identifier, which can name a crate
    Name,
    /// `crate`, `self`, `super`, or a macro's `$name`, which never does
    Keyword,
}

/// One segment of a path a `use` imports
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
struct Segment<'a> {
    text: &'a str,
    kind: SegmentKind,
}

/// The whole path to one element of a use tree, as the file spells it
///
/// A grouped import spells its prefix once, so the path to `b` in
/// `use a::{b}` carries `a` from the group.
#[derive(Clone, Eq, PartialEq, Debug, Default)]
struct UsePath<'a> {
    anchor: Anchor,
    segments: Vec<Segment<'a>>,
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
            anchor: Anchor::Crate,
            segments: self.segments.clone(),
        }
    }

    /// Appends the segments a path node spells
    ///
    /// A `scoped_identifier` without a qualifier is `::name`, and at the
    /// start of a path that `::` anchors the path at a crate.
    fn push(&mut self, node: &DecoratedNode<'a>) {
        match node.kind() {
            "identifier" => self.segments.push(Segment {
                text: node.text(),
                kind: SegmentKind::Name,
            }),
            "crate" | "self" | "super" | "metavariable" => self.segments.push(Segment {
                text: node.text(),
                kind: SegmentKind::Keyword,
            }),
            "scoped_identifier" => {
                match node.child_by_field_name("path") {
                    Some(qualifier) => self.push(&qualifier),
                    None => {
                        if self.segments.is_empty() {
                            self.anchor = Anchor::Crate;
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

    /// Returns the crate the path starts at, if its first segment is a name
    fn root(&self) -> Option<CrateRoot<'a>> {
        let Segment { text, kind } = self.segments.first()?;
        match kind {
            SegmentKind::Name => Some(CrateRoot {
                name: unraw(text),
                anchor: self.anchor,
            }),
            SegmentKind::Keyword => None,
        }
    }

    /// Returns the name the path binds when a `use` imports it unrenamed
    ///
    /// A path of one segment names a crate, and importing a crate binds no
    /// name of the file's own.
    fn binding(&self) -> Option<&'a str> {
        let [_, .., last] = self.segments.as_slice() else {
            return None;
        };
        let Segment { text, kind } = *last;
        match kind {
            SegmentKind::Name => Some(text),
            SegmentKind::Keyword => None,
        }
    }
}

/// Renders the path the way the file spells it
impl fmt::Display for UsePath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.anchor {
            Anchor::Crate => write!(f, "::")?,
            Anchor::Scope => {}
        }
        let texts: Vec<&str> = self.segments.iter().map(|segment| segment.text).collect();
        write!(f, "{}", texts.join("::"))
    }
}

/// The crate a path starts at, and whether a local name can shadow it
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
struct CrateRoot<'a> {
    name: &'a str,
    anchor: Anchor,
}

/// A path a `use` imports, with the node the provider decorates
#[derive(Clone, Debug)]
struct ImportedPath<'a> {
    node: DecoratedNode<'a>,
    path: UsePath<'a>,
}

/// The paths a file imports, the crates it imports from, and the names it
/// binds itself
#[derive(Clone, Debug, Default)]
struct FileImports<'a> {
    imported: Vec<ImportedPath<'a>>,
    roots: Vec<CrateRoot<'a>>,
    local_names: BTreeSet<&'a str>,
}

impl<'a> FileImports<'a> {
    /// Collects the imports and the bound names under a node
    ///
    /// A macro body is a token tree to tree-sitter, so a `use` inside one
    /// is not seen.
    fn walk(&mut self, node: &DecoratedNode<'a>) {
        match node.kind() {
            "use_declaration" => {
                let Some(argument) = node.child_by_field_name("argument") else {
                    return;
                };
                self.walk_use_tree(&argument, &UsePath::default());
                return;
            }
            "extern_crate_declaration" => {
                self.bind_field(node, "alias");
                return;
            }
            "mod_item" | "enum_item" => self.bind_field(node, "name"),
            _ => {}
        }

        for child in node.named_children() {
            self.walk(&child);
        }
    }

    /// Collects what one element of a use tree imports and binds
    ///
    /// The provider decorates the same elements this records as imported
    /// paths: a plain or renamed path, but not a wildcard or a `self`.
    fn walk_use_tree(&mut self, node: &DecoratedNode<'a>, prefix: &UsePath<'a>) {
        match node.kind() {
            "identifier" | "scoped_identifier" => {
                let path = prefix.extended(node);
                if let Some(name) = path.binding() {
                    self.bind(name);
                }
                self.import(*node, path);
            }
            "use_as_clause" => {
                self.bind_field(node, "alias");
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
            "self" => {
                if let Some(name) = prefix.binding() {
                    self.bind(name);
                }
                self.add_root(prefix);
            }
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
            self.roots.push(root);
        }
    }

    /// Records the name in a node's field as a name the file binds
    ///
    /// An enum's name is a `type_identifier` to tree-sitter, and every other
    /// name this reads is an `identifier`.
    fn bind_field(&mut self, node: &DecoratedNode<'a>, field: &str) {
        let Some(name) = node.child_by_field_name(field) else {
            return;
        };
        match name.kind() {
            "identifier" | "type_identifier" => self.bind(name.text()),
            _ => {}
        }
    }

    /// Records a name the file binds, which a bare first segment then means
    ///
    /// `_` binds nothing.
    fn bind(&mut self, name: &'a str) {
        let name = unraw(name);
        if name == "_" {
            return;
        }
        self.local_names.insert(name);
    }

    /// Returns a diagnostic for each imported path that bypasses a facade
    /// the file imports from
    fn report(&self) -> Vec<Diagnostic> {
        let crates: BTreeSet<&str> = self
            .roots
            .iter()
            .filter(|root| self.names_a_crate(root))
            .map(|root| root.name)
            .collect();

        self.imported
            .iter()
            .filter_map(|imported| self.bypass(imported, &crates))
            .collect()
    }

    /// Returns a diagnostic when an imported path bypasses a facade in
    /// `crates`
    fn bypass(&self, imported: &ImportedPath<'a>, crates: &BTreeSet<&str>) -> Option<Diagnostic> {
        let ImportedPath { node, path } = imported;
        let root = path.root()?;
        if !self.names_a_crate(&root) || SYSROOT.contains(&root.name) {
            return None;
        }
        let reexports = node.get::<Reexports>()?;
        if reexports.through(root.name).is_some() {
            return None;
        }
        let facade = facade_path(reexports, crates)?;
        let krate = facade.krate();

        Some(Diagnostic::new(
            RULE_ID,
            Severity::Warn,
            format!(
                "`{path}` is also exported as `{facade}`, and this file already imports from \
                 `{krate}`; import it through `{krate}`"
            ),
            node.span(),
        ))
    }

    /// Returns whether a crate root names a crate
    ///
    /// A bare segment that the file binds itself names that binding.
    fn names_a_crate(&self, root: &CrateRoot<'a>) -> bool {
        let CrateRoot { name, anchor } = root;
        match anchor {
            Anchor::Crate => true,
            Anchor::Scope => !self.local_names.contains(name),
        }
    }
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
impl whisker_plugin::DeclaresRules for FacadeBypassImport {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.facade-bypass-import")]
    }
}

whisker_plugin::export_lints![rust: [FacadeBypassImport]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use whisker_types::{DecorationMap, Language, LintPass};

    use super::*;

    /// Runs the rule with a [`Reexports`] on each path that spells a key
    ///
    /// The rule reads a decoration the Rust provider makes, and these tests
    /// run no provider, so they attach it by hand. Each key names the text
    /// of an imported path, and its value lists the public paths to put on
    /// it, written as `krate::segment::Item`.
    fn run(source: &str, decorations: &[(&str, &[&str])]) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        let mut map = DecorationMap::new();
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
            PublicPath::new(krate, segments)
        }))
    }

    const THING: (&str, &[&str]) = ("facade_core::thing::Thing", &["facade::thing::Thing"]);

    #[test]
    fn alias_named_like_the_constituent_is_not_flagged() {
        let source = "use other as facade_core;\nuse facade::x;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
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
            .has_severity(Severity::Warn)
            .message_contains(
                "`facade_core::thing::Thing` is also exported as `facade::thing::Thing`, and \
                 this file already imports from `facade`; import it through `facade`",
            )
            .has_span("<test>", 19, 44);
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

        assert_no_diagnostics(&run(source, &[THING]));
    }

    #[test]
    fn extern_crate_alias_named_like_the_constituent_is_not_flagged() {
        let source =
            "extern crate other as facade_core;\nuse facade::x;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
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
            .has_span("<test>", 33, 45);
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
    fn module_named_like_the_constituent_is_not_flagged() {
        let source = "mod facade_core;\nuse facade::x;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
    }

    #[test]
    fn module_named_like_the_facade_is_not_an_import() {
        let source = "mod facade {}\nuse facade::x;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
    }

    #[test]
    fn nested_group_self_named_like_the_constituent_is_not_flagged() {
        let source =
            "use crate::{facade_core::{self}};\nuse facade::x;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
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
    fn trait_send_holds_for_crate_root() {
        fn assert_send<T: Send>() {}
        assert_send::<CrateRoot<'_>>();
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
    fn trait_send_holds_for_segment() {
        fn assert_send<T: Send>() {}
        assert_send::<Segment<'_>>();
    }

    #[test]
    fn trait_send_holds_for_segment_kind() {
        fn assert_send<T: Send>() {}
        assert_send::<SegmentKind>();
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
    fn trait_sync_holds_for_crate_root() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<CrateRoot<'_>>();
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
    fn trait_sync_holds_for_segment() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Segment<'_>>();
    }

    #[test]
    fn trait_sync_holds_for_segment_kind() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<SegmentKind>();
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
    fn trait_unpin_holds_for_crate_root() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<CrateRoot<'_>>();
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
    fn trait_unpin_holds_for_segment() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Segment<'_>>();
    }

    #[test]
    fn trait_unpin_holds_for_segment_kind() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<SegmentKind>();
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

    /// Pins the shape whisker's own `use anyhow::Context as _` takes
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
    fn use_binding_named_like_the_constituent_is_not_flagged() {
        let source = "use other::facade_core;\nuse facade::x;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
    }

    #[test]
    fn use_binding_named_like_the_facade_is_not_an_import() {
        let source = "use other::facade;\nuse facade::x;\nuse facade_core::thing::Thing;";

        assert_no_diagnostics(&run(source, &[THING]));
    }
}
