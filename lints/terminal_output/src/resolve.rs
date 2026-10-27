use whisker_types::DecoratedNode;

use crate::PRINT_MACROS;

/// How many `use` declarations a resolution follows before it gives up
///
/// A chain such as `use std::process; use process::exit;` takes one step
/// per link. The cap ends a cycle, which the compiler would reject anyway.
const MAX_DEPTH: usize = 8;

/// The namespace a name is looked up in
///
/// Rust keeps macros, types and modules, and values apart, so a local
/// function named `println` does not hide the `println!` macro. The rule
/// stays coarse where that is safe: any local item hides a type or a value
/// of the same name. Too much hiding only costs a finding.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub(crate) enum Namespace {
    /// The name before a `!`
    Macro,
    /// The first segment of a path with more than one segment
    Type,
    /// A single name that is called, such as `exit(1)`
    Value,
}

/// What one name means at one point in a file
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
enum Binding {
    /// A `use` declaration binds the name to this path
    Import(Vec<String>),
    /// A local item, binding, or parameter defines the name
    Local,
    /// A glob import may bind the name, and the rule cannot see what it holds
    Unknown,
    /// Nothing in the file binds the name
    Unbound,
}

/// Returns the full path that `path` names at `node`, if the file proves it
///
/// The first segment is looked up through the `use` declarations and local
/// items of every scope around `node`, from the innermost outward. The walk
/// stops at the enclosing module, because a module does not see the names
/// its parent imports. A name that nothing binds resolves only when it is
/// `std`, or when it is a macro that names a print macro of the prelude.
/// The result is [`None`] whenever the file does not settle the question.
pub(crate) fn resolve(
    node: &DecoratedNode<'_>,
    path: &str,
    namespace: Namespace,
) -> Option<Vec<String>> {
    resolve_at(node, path, namespace, 0)
}

/// Resolves `path` at `node`, counting the `use` declarations followed so far
fn resolve_at(
    node: &DecoratedNode<'_>,
    path: &str,
    namespace: Namespace,
    depth: usize,
) -> Option<Vec<String>> {
    if depth > MAX_DEPTH {
        return None;
    }

    let (anchor, segments) = split_path(path)?;
    if anchor == Anchor::Absolute {
        return Some(segments);
    }

    let (first, rest) = segments.split_first()?;
    let first_namespace = if rest.is_empty() {
        namespace
    } else {
        Namespace::Type
    };

    match lookup(node, first, first_namespace, depth) {
        Binding::Import(mut prefix) => {
            prefix.extend(rest.iter().cloned());
            Some(prefix)
        }
        Binding::Local => None,
        Binding::Unknown => None,
        Binding::Unbound => match (first.as_str(), first_namespace) {
            ("std", Namespace::Type) => Some(segments),
            (name, Namespace::Macro) => prelude_macro(node, name),
            (_, Namespace::Type) => None,
            (_, Namespace::Value) => None,
        },
    }
}

/// Returns the path of the standard print macro that a bare `name` calls
///
/// The rule needs no other macro from the prelude, so any other name stays
/// unresolved. A `macro_rules!` anywhere in the file with the same name
/// replaces the prelude macro. Textual scoping lets such a definition reach
/// code in other modules, and the rule does not track the order.
fn prelude_macro(node: &DecoratedNode<'_>, name: &str) -> Option<Vec<String>> {
    if !PRINT_MACROS.contains(&name) {
        return None;
    }
    if defines_macro(&root(node), name) {
        return None;
    }
    Some(vec!["std".to_owned(), name.to_owned()])
}

/// Whether a path starts at the crate graph root or in the current scope
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
enum Anchor {
    /// The path starts with `::`, so its first segment is a crate name
    Absolute,
    /// The path starts with a name the current scope must resolve
    Relative,
}

/// Splits the text of a path into its segments
///
/// Whitespace and comments may sit between the segments of a path, so the
/// text loses its whitespace first. Returns [`None`] for an empty segment,
/// which only a path the rule does not understand produces.
fn split_path(path: &str) -> Option<(Anchor, Vec<String>)> {
    let compact: String = path.chars().filter(|c| !c.is_whitespace()).collect();
    let (anchor, compact) = match compact.strip_prefix("::") {
        Some(rest) => (Anchor::Absolute, rest.to_owned()),
        None => (Anchor::Relative, compact),
    };

    let segments: Vec<String> = compact.split("::").map(str::to_owned).collect();
    if segments.iter().any(|segment| !is_identifier(segment)) {
        return None;
    }
    Some((anchor, segments))
}

/// Returns whether a path segment is a plain identifier
fn is_identifier(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '#')
}

/// Returns what `name` means at `node`
fn lookup(node: &DecoratedNode<'_>, name: &str, namespace: Namespace, depth: usize) -> Binding {
    let position = node.span().start();
    let mut boundary = FunctionBoundary::Inside;
    let mut current = node.parent();

    while let Some(ancestor) = current {
        if namespace == Namespace::Value
            && boundary == FunctionBoundary::Inside
            && binds_locally(&ancestor, name)
        {
            return Binding::Local;
        }
        if ancestor.kind() == "function_item" {
            boundary = FunctionBoundary::Crossed;
        }

        let scope = scope_kind(&ancestor);
        if scope != ScopeKind::Transparent {
            let lookup = ScopeLookup {
                name,
                namespace,
                position,
                boundary,
                depth,
            };
            if let Some(binding) = lookup.in_scope(&ancestor) {
                return binding;
            }
        }
        if scope == ScopeKind::Module {
            return Binding::Unbound;
        }
        current = ancestor.parent();
    }

    Binding::Unbound
}

/// Whether the walk has left the function the name is used in
///
/// A nested `fn` sees the items of the blocks around it but not their
/// `let` bindings or parameters.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
enum FunctionBoundary {
    /// The walk is still in the function that holds the use
    Inside,
    /// The walk has passed the `fn` item that holds the use
    Crossed,
}

/// What a node contributes to name lookup
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
enum ScopeKind {
    /// A block, whose items and imports its contents see
    Block,
    /// A file or an inline module, past which the walk does not go
    Module,
    /// Anything else, which the walk passes through
    Transparent,
}

/// Returns what a node contributes to name lookup
fn scope_kind(node: &DecoratedNode<'_>) -> ScopeKind {
    match node.kind() {
        "block" => ScopeKind::Block,
        "source_file" => ScopeKind::Module,
        "declaration_list" => match node.parent() {
            Some(parent) if parent.kind() == "mod_item" => ScopeKind::Module,
            Some(_) => ScopeKind::Transparent,
            None => ScopeKind::Transparent,
        },
        _ => ScopeKind::Transparent,
    }
}

/// One lookup of a name in the scopes around a node
struct ScopeLookup<'n> {
    name: &'n str,
    namespace: Namespace,
    position: usize,
    boundary: FunctionBoundary,
    depth: usize,
}

impl ScopeLookup<'_> {
    /// Returns what the scope binds the name to, or [`None`] to keep looking
    ///
    /// A `let` before the use and a local item each hide an import. An
    /// explicit import beats a glob. A glob from `std` binds the name to the
    /// item of that name in the module it names. Any other glob may bind the
    /// name too, so the answer is unknown, except for `std` and for macros.
    /// Nobody brings a module named `std` or a macro named like a print
    /// macro in through a glob, and `use super::*;` is everywhere.
    fn in_scope(&self, scope: &DecoratedNode<'_>) -> Option<Binding> {
        let mut explicit = None;
        let mut globs = Vec::new();

        for child in scope.named_children() {
            match child.kind() {
                "let_declaration" => {
                    if self.hides_with_let(scope, &child) {
                        return Some(Binding::Local);
                    }
                }
                "use_declaration" => {
                    let Some(argument) = child.child_by_field_name("argument") else {
                        continue;
                    };
                    let mut imports = Vec::new();
                    collect_imports(&argument, &UsePrefix::default(), &mut imports);
                    for import in imports {
                        match import {
                            Import::Named { name, path } if name == self.name => {
                                explicit = Some(self.follow(&child, &path));
                            }
                            Import::Named { .. } => {}
                            Import::Glob { path } => globs.push((child, path)),
                        }
                    }
                }
                "macro_definition" => {}
                _ => {
                    if self.namespace != Namespace::Macro && defines_item(&child, self.name) {
                        return Some(Binding::Local);
                    }
                }
            }
        }

        if explicit.is_some() {
            return explicit;
        }
        if self.namespace == Namespace::Macro || self.name == "std" {
            return None;
        }
        let [(declaration, path)] = globs.as_slice() else {
            if globs.is_empty() {
                return None;
            }
            return Some(Binding::Unknown);
        };

        let Some(mut module) = resolve_at(declaration, path, Namespace::Type, self.depth + 1)
        else {
            return Some(Binding::Unknown);
        };
        if module.first().map(String::as_str) != Some("std") {
            return Some(Binding::Unknown);
        }
        module.push(self.name.to_owned());
        Some(Binding::Import(module))
    }

    /// Returns whether a `let` in `scope` hides the name before the use
    fn hides_with_let(&self, scope: &DecoratedNode<'_>, declaration: &DecoratedNode<'_>) -> bool {
        if self.namespace != Namespace::Value || self.boundary == FunctionBoundary::Crossed {
            return false;
        }
        if scope.kind() != "block" || declaration.span().start() >= self.position {
            return false;
        }
        declaration
            .child_by_field_name("pattern")
            .is_some_and(|pattern| pattern_binds(&pattern, self.name))
    }

    /// Resolves the path an import names, from the import's own position
    fn follow(&self, declaration: &DecoratedNode<'_>, path: &str) -> Binding {
        match resolve_at(declaration, path, Namespace::Type, self.depth + 1) {
            Some(resolved) => Binding::Import(resolved),
            None => Binding::Unknown,
        }
    }
}

/// One name a `use` declaration brings into scope
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
enum Import {
    /// `use a::b;` binds `b`, and `use a::b as c;` binds `c`, to `a::b`
    Named { name: String, path: String },
    /// `use a::*;` binds every public name in `a`
    Glob { path: String },
}

/// The path text that the enclosing `{..}` lists of a `use` declaration share
///
/// The prefix is empty at the top of a declaration. It ends in `::` under
/// a list that has no path of its own, as in `use ::{std::process};`.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Default)]
struct UsePrefix(String);

impl UsePrefix {
    /// Returns the text of `path` written under this prefix
    fn join(&self, path: &str) -> String {
        let UsePrefix(prefix) = self;
        if prefix.is_empty() || prefix.ends_with("::") {
            format!("{prefix}{path}")
        } else {
            format!("{prefix}::{path}")
        }
    }

    /// Returns the prefix of a `{..}` list under this one
    ///
    /// `path` is the path written in front of the list, if there is one.
    fn nest(&self, path: Option<&str>) -> UsePrefix {
        match path {
            Some(path) => UsePrefix(self.join(path)),
            None => {
                let UsePrefix(prefix) = self;
                UsePrefix(format!("{prefix}::"))
            }
        }
    }
}

/// Collects the imports under one node of a `use` declaration's argument
fn collect_imports(node: &DecoratedNode<'_>, prefix: &UsePrefix, imports: &mut Vec<Import>) {
    match node.kind() {
        "identifier" | "scoped_identifier" | "crate" | "super" => {
            let path = prefix.join(node.text());
            if let Some(name) = last_segment(&path) {
                imports.push(Import::Named { name, path });
            }
        }
        "self" => {
            let UsePrefix(path) = prefix;
            if let Some(name) = last_segment(path) {
                imports.push(Import::Named {
                    name,
                    path: path.clone(),
                });
            }
        }
        "use_as_clause" => {
            let (Some(path), Some(alias)) = (
                node.child_by_field_name("path"),
                node.child_by_field_name("alias"),
            ) else {
                return;
            };
            let path = prefix.join(path.text());
            let path = match path.strip_suffix("::self") {
                Some(module) => module.to_owned(),
                None => path,
            };
            imports.push(Import::Named {
                name: alias.text().to_owned(),
                path,
            });
        }
        "scoped_use_list" => {
            let path = node.child_by_field_name("path");
            let prefix = prefix.nest(path.as_ref().map(DecoratedNode::text));
            if let Some(list) = node.child_by_field_name("list") {
                collect_imports(&list, &prefix, imports);
            }
        }
        "use_list" => {
            for child in node.named_children() {
                collect_imports(&child, prefix, imports);
            }
        }
        "use_wildcard" => {
            let path = match node.named_child(0) {
                Some(path) => prefix.join(path.text()),
                None => {
                    let UsePrefix(path) = prefix;
                    path.trim_end_matches("::").to_owned()
                }
            };
            imports.push(Import::Glob { path });
        }
        _ => {}
    }
}

/// Returns the last segment of a path, which is the name an import binds
fn last_segment(path: &str) -> Option<String> {
    let segment = path.rsplit("::").next()?.trim();
    match segment {
        "" | "self" | "crate" | "super" | "_" => None,
        _ => Some(segment.to_owned()),
    }
}

/// Returns whether a node binds `name` as a value for the code below it
///
/// A function and a closure bind their parameters. A `match` arm and a `for`
/// loop bind their pattern. An `if let` or a `while let` binds the
/// pattern of its condition. The walk visits these only while it is still
/// inside the function that holds the use.
fn binds_locally(node: &DecoratedNode<'_>, name: &str) -> bool {
    match node.kind() {
        "function_item" | "closure_expression" => node
            .child_by_field_name("parameters")
            .is_some_and(|parameters| pattern_binds(&parameters, name)),
        "match_arm" | "for_expression" => node
            .child_by_field_name("pattern")
            .is_some_and(|pattern| pattern_binds(&pattern, name)),
        "if_expression" | "while_expression" => node
            .child_by_field_name("condition")
            .is_some_and(|condition| condition_binds(&condition, name)),
        _ => false,
    }
}

/// Returns whether a `let` inside a condition binds `name`
fn condition_binds(condition: &DecoratedNode<'_>, name: &str) -> bool {
    if condition.kind() == "let_condition" {
        return condition
            .child_by_field_name("pattern")
            .is_some_and(|pattern| pattern_binds(&pattern, name));
    }
    condition
        .named_children()
        .iter()
        .any(|child| condition_binds(child, name))
}

/// Returns whether a pattern may bind `name`
///
/// Every identifier in the pattern counts, including the path of a tuple
/// struct pattern and a type in a parameter. That reads too much as a
/// binding, which only costs a finding.
fn pattern_binds(pattern: &DecoratedNode<'_>, name: &str) -> bool {
    match pattern.kind() {
        "identifier" | "shorthand_field_identifier" => pattern.text() == name,
        _ => pattern
            .named_children()
            .iter()
            .any(|child| pattern_binds(child, name)),
    }
}

/// Returns whether a node is an item that defines `name`
fn defines_item(node: &DecoratedNode<'_>, name: &str) -> bool {
    let is_item = match node.kind() {
        "const_item"
        | "enum_item"
        | "extern_crate_declaration"
        | "function_item"
        | "mod_item"
        | "static_item"
        | "struct_item"
        | "trait_item"
        | "type_item"
        | "union_item" => true,
        _ => false,
    };
    if !is_item {
        return false;
    }
    let defined = node
        .child_by_field_name("alias")
        .or_else(|| node.child_by_field_name("name"));
    defined.is_some_and(|defined| defined.text() == name)
}

/// Returns whether a `macro_rules!` anywhere under `node` defines `name`
fn defines_macro(node: &DecoratedNode<'_>, name: &str) -> bool {
    if node.kind() == "macro_definition" {
        return node
            .child_by_field_name("name")
            .is_some_and(|defined| defined.text() == name);
    }
    node.named_children()
        .iter()
        .any(|child| defines_macro(child, name))
}

/// Returns the root of the tree that holds `node`
fn root<'a>(node: &DecoratedNode<'a>) -> DecoratedNode<'a> {
    let mut current = *node;
    while let Some(parent) = current.parent() {
        current = parent;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_with_empty_prefix_returns_path() {
        let joined = UsePrefix::default().join("std");

        assert_eq!(joined, "std");
    }

    #[test]
    fn join_with_prefix_and_path_inserts_separator() {
        let joined = UsePrefix("std".to_owned()).join("process");

        assert_eq!(joined, "std::process");
    }

    #[test]
    fn join_with_prefix_ending_in_separator_adds_none() {
        let joined = UsePrefix("::".to_owned()).join("std");

        assert_eq!(joined, "::std");
    }

    #[test]
    fn last_segment_of_self_returns_none() {
        let segment = last_segment("std::process::self");

        assert_eq!(segment, None);
    }

    #[test]
    fn last_segment_returns_name() {
        let segment = last_segment("std::process::exit");

        assert_eq!(segment.as_deref(), Some("exit"));
    }

    #[test]
    fn nest_with_path_joins_it() {
        let nested = UsePrefix("std".to_owned()).nest(Some("process"));

        assert_eq!(nested, UsePrefix("std::process".to_owned()));
    }

    #[test]
    fn nest_without_path_ends_in_separator() {
        let nested = UsePrefix::default().nest(None);

        assert_eq!(nested, UsePrefix("::".to_owned()));
    }

    #[test]
    fn split_path_with_generic_arguments_returns_none() {
        let split = split_path("Vec::<u8>::new");

        assert_eq!(split, None);
    }

    #[test]
    fn split_path_with_leading_separator_is_absolute() {
        let split = split_path(":: std :: io");

        assert_eq!(
            split,
            Some((Anchor::Absolute, vec!["std".to_owned(), "io".to_owned()]))
        );
    }

    #[test]
    fn trait_send_anchor() {
        fn assert_send<T: Send>() {}
        assert_send::<Anchor>();
    }

    #[test]
    fn trait_send_binding() {
        fn assert_send<T: Send>() {}
        assert_send::<Binding>();
    }

    #[test]
    fn trait_send_function_boundary() {
        fn assert_send<T: Send>() {}
        assert_send::<FunctionBoundary>();
    }

    #[test]
    fn trait_send_import() {
        fn assert_send<T: Send>() {}
        assert_send::<Import>();
    }

    #[test]
    fn trait_send_namespace() {
        fn assert_send<T: Send>() {}
        assert_send::<Namespace>();
    }

    #[test]
    fn trait_send_scope_kind() {
        fn assert_send<T: Send>() {}
        assert_send::<ScopeKind>();
    }

    #[test]
    fn trait_send_scope_lookup() {
        fn assert_send<T: Send>() {}
        assert_send::<ScopeLookup<'static>>();
    }

    #[test]
    fn trait_send_use_prefix() {
        fn assert_send<T: Send>() {}
        assert_send::<UsePrefix>();
    }

    #[test]
    fn trait_sync_anchor() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Anchor>();
    }

    #[test]
    fn trait_sync_binding() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Binding>();
    }

    #[test]
    fn trait_sync_function_boundary() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FunctionBoundary>();
    }

    #[test]
    fn trait_sync_import() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Import>();
    }

    #[test]
    fn trait_sync_namespace() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Namespace>();
    }

    #[test]
    fn trait_sync_scope_kind() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ScopeKind>();
    }

    #[test]
    fn trait_sync_scope_lookup() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ScopeLookup<'static>>();
    }

    #[test]
    fn trait_sync_use_prefix() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<UsePrefix>();
    }

    #[test]
    fn trait_unpin_anchor() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Anchor>();
    }

    #[test]
    fn trait_unpin_binding() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Binding>();
    }

    #[test]
    fn trait_unpin_function_boundary() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<FunctionBoundary>();
    }

    #[test]
    fn trait_unpin_import() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Import>();
    }

    #[test]
    fn trait_unpin_namespace() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Namespace>();
    }

    #[test]
    fn trait_unpin_scope_kind() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ScopeKind>();
    }

    #[test]
    fn trait_unpin_scope_lookup() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ScopeLookup<'static>>();
    }

    #[test]
    fn trait_unpin_use_prefix() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<UsePrefix>();
    }
}
