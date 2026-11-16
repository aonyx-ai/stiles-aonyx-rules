use std::ops::Range;

use stiles_types::{DecoratedNode, Edit, Fix, Span};

use crate::has_cfg_attribute;

/// The names that the prelude binds in each module, in any edition
///
/// An import at the top of a module shadows these for every function in
/// it. A move of such an import changes what the name means elsewhere.
const PRELUDE: &[&str] = &[
    "AsMut",
    "AsRef",
    "Box",
    "Clone",
    "Copy",
    "Default",
    "DoubleEndedIterator",
    "Drop",
    "Eq",
    "Err",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "Future",
    "Into",
    "IntoFuture",
    "IntoIterator",
    "Iterator",
    "None",
    "Ok",
    "Option",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Result",
    "Send",
    "Sized",
    "Some",
    "String",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
    "Vec",
    "drop",
];

/// The node kinds of items that bind a name in their scope
const NAMED_ITEMS: &[&str] = &[
    "const_item",
    "enum_item",
    "extern_crate_declaration",
    "function_item",
    "function_signature_item",
    "macro_definition",
    "mod_item",
    "static_item",
    "struct_item",
    "trait_item",
    "type_item",
    "union_item",
];

/// One name that an import binds, and the path it binds the name to
#[derive(Clone, Eq, PartialEq, Debug)]
struct Binding {
    /// The name the import binds, which is `_` for an import that binds none
    name: String,
    /// The path without whitespace, such as `std::fmt::Write`
    path: String,
}

/// What a module already makes of the names that a body import binds
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Standing {
    /// The module imports each of the same paths under the same names
    Imported,
    /// The module binds none of the names
    Free,
}

/// The source text of a whole file, and the offset where it starts
///
/// Tree-sitter starts the root node after the whitespace that opens the
/// file, so the text of the root can start after offset 0. Each offset
/// here counts from the start of the file, as a span does.
struct Source<'a> {
    text: &'a str,
    start: usize,
}

impl<'a> Source<'a> {
    /// Reads the source of the file that `node` lies in
    fn of(node: &DecoratedNode<'a>) -> Self {
        let mut root = *node;
        while let Some(parent) = root.parent() {
            root = parent;
        }

        Self {
            text: root.text(),
            start: root.span().start(),
        }
    }

    /// Returns the text between two file offsets
    fn slice(&self, range: Range<usize>) -> &'a str {
        let Range { start, end } = range;
        &self.text[start - self.start..end - self.start]
    }

    /// Returns the file offset where the text ends
    fn end(&self) -> usize {
        self.start + self.text.len()
    }

    /// Returns the offset where the line that holds `offset` starts
    fn line_start(&self, offset: usize) -> usize {
        self.slice(self.start..offset)
            .rfind('\n')
            .map_or(self.start, |newline| self.start + newline + 1)
    }

    /// Returns the offset of the newline that ends the line holding
    /// `offset`, or the end of the text when no newline follows
    fn line_end(&self, offset: usize) -> usize {
        let end = self.end();
        self.slice(offset..end)
            .find('\n')
            .map_or(end, |newline| offset + newline)
    }

    /// Returns the whitespace that opens the line of `offset`, or an empty
    /// text when something else comes before `offset` on that line
    fn indent(&self, offset: usize) -> &'a str {
        let before = self.slice(self.line_start(offset)..offset);
        match before.trim().is_empty() {
            true => before,
            false => "",
        }
    }
}

/// One import in a function body that the fix of the module deletes
struct Removal<'a> {
    /// The `use` declaration in the body
    node: DecoratedNode<'a>,
    /// Whether the module gains the import, or has it already
    standing: Standing,
}

/// Returns the fix that moves every import it can of the module that holds
/// `node` to the top of that module
///
/// One fix covers the whole module, so that a module with many imports
/// settles in one round. Each finding in the module carries this same fix.
/// The result is [`None`] when `node` itself cannot move.
///
/// [`None`]: std::option::Option::None
pub(crate) fn fix(node: &DecoratedNode<'_>) -> Option<Fix> {
    let module = module_of(node)?;
    let mut flagged = Vec::new();
    collect_flagged(&module, &mut flagged);
    let removals = removals(&module, &flagged);
    if !removals
        .iter()
        .any(|removal| removal.node.id() == node.id())
    {
        return None;
    }

    let source = Source::of(node);
    let deleted: Vec<DecoratedNode<'_>> = removals.iter().map(|removal| removal.node).collect();
    let mut edits: Vec<Edit> = deleted
        .iter()
        .map(|import| deletion(&source, import, &deleted))
        .collect();
    let moved: Vec<DecoratedNode<'_>> = removals
        .iter()
        .filter(|removal| removal.standing == Standing::Free)
        .map(|removal| removal.node)
        .collect();
    if !moved.is_empty() {
        edits.push(insertion(&source, &moved, &module)?);
    }

    Fix::builder()
        .message("move the imports in function bodies to the top of the module")
        .edits(edits)
        .build()
        .ok()
}

/// Adds each import under `scope` that the rule reports to `flagged`, in
/// source order
///
/// The walk does not enter a nested module, whose imports move to that
/// module instead.
fn collect_flagged<'a>(scope: &DecoratedNode<'a>, flagged: &mut Vec<DecoratedNode<'a>>) {
    for child in scope.named_children() {
        match child.kind() {
            "mod_item" => {}
            "use_declaration" => {
                if crate::is_flagged(&child) {
                    flagged.push(child);
                }
            }
            _ => collect_flagged(&child, flagged),
        }
    }
}

/// Returns the imports of `flagged` that the fix deletes, in source order
///
/// An import that the module already has is only deleted. An earlier move
/// can bring the same paths, and then the later import is only deleted too.
/// An import that cannot move is left out. So is an import when an earlier
/// move binds one of its names to another path or brings only some of them.
fn removals<'a>(module: &DecoratedNode<'_>, flagged: &[DecoratedNode<'a>]) -> Vec<Removal<'a>> {
    let mut moved: Vec<Binding> = Vec::new();
    let mut removals = Vec::new();
    for node in flagged {
        let Some((standing, bindings)) = plan(node, module) else {
            continue;
        };
        let standing = match standing {
            Standing::Imported => Standing::Imported,
            Standing::Free => {
                let clashes = bindings.iter().any(|binding| {
                    binding.name != "_"
                        && !moved.contains(binding)
                        && moved.iter().any(|other| other.name == binding.name)
                });
                let repeated = bindings
                    .iter()
                    .filter(|binding| moved.contains(binding))
                    .count();
                match (clashes, repeated) {
                    (true, _) => continue,
                    (false, 0) => {
                        moved.extend(bindings);
                        Standing::Free
                    }
                    (false, count) if count == bindings.len() => Standing::Imported,
                    (false, _) => continue,
                }
            }
        };
        removals.push(Removal {
            node: *node,
            standing,
        });
    }

    removals
}

/// Returns whether the module has the import already, and the names it
/// binds, or [`None`] when the import cannot move on its own
///
/// The import cannot move when the move could change what a name means, or
/// when it carries anything that the move would lose.
///
/// [`None`]: std::option::Option::None
fn plan(node: &DecoratedNode<'_>, module: &DecoratedNode<'_>) -> Option<(Standing, Vec<Binding>)> {
    if has_visibility(node) || is_decorated(node) {
        return None;
    }

    let mut bindings = Vec::new();
    collect_bindings(&node.child_by_field_name("argument")?, "", &mut bindings)?;
    if is_blocked(node, &bindings) {
        return None;
    }
    let standing = standing(module, &bindings)?;

    Some((standing, bindings))
}

/// Returns whether an attribute or a doc comment sits above the import
///
/// The walk reads backward over plain comments. The move would leave an
/// attribute or a doc comment behind, on the statement that follows.
fn is_decorated(node: &DecoratedNode<'_>) -> bool {
    let mut current = node.prev_named_sibling();
    while let Some(sibling) = current {
        match sibling.kind() {
            "attribute_item" => return true,
            "line_comment" | "block_comment" => {
                if sibling.child_by_field_name("doc").is_some() {
                    return true;
                }
            }
            _ => return false,
        }
        current = sibling.prev_named_sibling();
    }

    false
}

/// Returns whether the import carries a visibility, such as `pub use`
///
/// A visible import at the top of a module would export its name.
fn has_visibility(node: &DecoratedNode<'_>) -> bool {
    node.named_children()
        .iter()
        .any(|child| child.kind() == "visibility_modifier")
}

/// Adds the names that the argument of a `use` binds to `bindings`
///
/// `prefix` is the path of the enclosing `use` list, empty at the top. The
/// result is [`None`] for a glob, whose names the syntax does not show.
///
/// [`None`]: std::option::Option::None
fn collect_bindings(
    node: &DecoratedNode<'_>,
    prefix: &str,
    bindings: &mut Vec<Binding>,
) -> Option<()> {
    match node.kind() {
        "self" => {
            let name = prefix.rsplit("::").next().filter(|name| !name.is_empty())?;
            bindings.push(Binding {
                name: name.to_owned(),
                path: prefix.to_owned(),
            });
        }
        "crate" | "identifier" | "metavariable" | "scoped_identifier" | "super" => {
            let path = joined(prefix, node);
            let name = path.rsplit("::").next()?.to_owned();
            bindings.push(Binding { name, path });
        }
        "use_as_clause" => {
            let path = joined(prefix, &node.child_by_field_name("path")?);
            let name = node.child_by_field_name("alias")?.text().to_owned();
            bindings.push(Binding { name, path });
        }
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .map_or(prefix.to_owned(), |path| joined(prefix, &path));
            let list = node.child_by_field_name("list")?;
            collect_bindings(&list, &path, bindings)?;
        }
        "use_list" => {
            for child in node.named_children() {
                match child.kind() {
                    "line_comment" | "block_comment" => {}
                    _ => collect_bindings(&child, prefix, bindings)?,
                }
            }
        }
        _ => return None,
    }

    Some(())
}

/// Returns the path that `node` spells after `prefix`, with the `::`
/// between them and without whitespace
fn joined(prefix: &str, node: &DecoratedNode<'_>) -> String {
    let path = compact(node.text());
    match prefix.is_empty() {
        true => path,
        false => format!("{prefix}::{path}"),
    }
}

/// Returns `text` without its whitespace
fn compact(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// Returns the body of the module that holds the import, which is a file
/// or the braces of a `mod`
fn module_of<'a>(node: &DecoratedNode<'a>) -> Option<DecoratedNode<'a>> {
    let mut current = node.parent();
    loop {
        let scope = current?;
        match scope.kind() {
            "source_file" => return Some(scope),
            "declaration_list"
                if scope
                    .parent()
                    .is_some_and(|parent| parent.kind() == "mod_item") =>
            {
                return Some(scope);
            }
            _ => current = scope.parent(),
        }
    }
}

/// Returns whether a path of `bindings` starts with a name that a block
/// between the import and its module declares
///
/// That name would not resolve at the top of the module.
fn is_blocked(node: &DecoratedNode<'_>, bindings: &[Binding]) -> bool {
    let mut current = node.parent();
    while let Some(scope) = current {
        match scope.kind() {
            "source_file" | "declaration_list" => return false,
            "block" => {
                let names = declared_names(&scope, node.id());
                if bindings
                    .iter()
                    .any(|binding| names.contains(&first_segment(&binding.path).to_owned()))
                {
                    return true;
                }
            }
            _ => {}
        }
        current = scope.parent();
    }

    false
}

/// Returns the first segment of a path, which is empty for `::std`
fn first_segment(path: &str) -> &str {
    path.split("::").next().unwrap_or(path)
}

/// Returns the names that the items and the imports of a scope bind,
/// leaving out the import with the id `skip`
fn declared_names(scope: &DecoratedNode<'_>, skip: usize) -> Vec<String> {
    let mut names = Vec::new();
    for child in scope.named_children() {
        if child.id() == skip {
            continue;
        }
        match child.kind() {
            "use_declaration" => {
                let mut bindings = Vec::new();
                if let Some(argument) = child.child_by_field_name("argument") {
                    let _ = collect_bindings(&argument, "", &mut bindings);
                }
                names.extend(bindings.into_iter().map(|binding| binding.name));
            }
            "foreign_mod_item" => {
                if let Some(body) = child.child_by_field_name("body") {
                    names.extend(declared_names(&body, skip));
                }
            }
            kind if NAMED_ITEMS.contains(&kind) => {
                let name = child
                    .child_by_field_name("alias")
                    .or_else(|| child.child_by_field_name("name"));
                names.extend(name.map(|name| name.text().to_owned()));
            }
            _ => {}
        }
    }

    names
}

/// Returns what the module already makes of the names in `bindings`, or
/// [`None`] when it binds one of them to something else
///
/// A glob import can bind any name, and the prelude binds its own names.
/// An import under a `cfg` gate binds its name only in some builds. A
/// second import of the same path would clash in those builds.
///
/// [`None`]: std::option::Option::None
fn standing(module: &DecoratedNode<'_>, bindings: &[Binding]) -> Option<Standing> {
    let mut imports = Vec::new();
    let mut gated = Vec::new();
    for child in module.named_children() {
        if child.kind() != "use_declaration" {
            continue;
        }
        let mut found = Vec::new();
        collect_bindings(&child.child_by_field_name("argument")?, "", &mut found)?;
        match has_cfg_attribute(&child) {
            true => gated.extend(found),
            false => imports.extend(found),
        }
    }
    let items = declared_names(module, usize::MAX);

    let mut imported = 0;
    for binding in bindings {
        if imports.contains(binding) {
            imported += 1;
            continue;
        }
        if binding.name == "_" {
            continue;
        }
        let bound = PRELUDE.contains(&binding.name.as_str())
            || items.contains(&binding.name)
            || gated.iter().any(|other| other.name == binding.name);
        if bound {
            return None;
        }
    }

    match imported {
        0 => Some(Standing::Free),
        count if count == bindings.len() => Some(Standing::Imported),
        _ => None,
    }
}

/// Returns the edit that deletes the import from the function body
///
/// An import alone on its line takes the line with it. When a blank line
/// follows and the line above opens the block, the blank line goes too.
/// Lines of other imports in `deleted` do not count as the line above. An
/// import that shares its line loses only itself and the spaces after it.
fn deletion(source: &Source<'_>, node: &DecoratedNode<'_>, deleted: &[DecoratedNode<'_>]) -> Edit {
    let span = node.span();
    let line_start = source.line_start(span.start());
    let line_end = source.line_end(span.end());
    let alone = source.slice(line_start..span.start()).trim().is_empty()
        && source.slice(span.end()..line_end).trim().is_empty();

    let (start, end) = match alone {
        true => {
            let file_end = source.end();
            let end = (line_end + 1).min(file_end);
            let next_end = source.line_end(end);
            let blank_after = end < file_end && source.slice(end..next_end).trim().is_empty();
            let opens_block = opens_block(source, line_start, deleted);
            match blank_after && opens_block {
                true => (line_start, (next_end + 1).min(file_end)),
                false => (line_start, end),
            }
        }
        false => {
            let after = source.slice(span.end()..line_end);
            let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
            (span.start(), span.end() + spaces)
        }
    };

    Edit::builder()
        .span(
            Span::builder()
                .file(span.file_path().clone())
                .start(start)
                .end(end)
                .build(),
        )
        .replacement("")
        .build()
}

/// Returns whether the line above `line_start` ends with `{`, skipping
/// the lines of the imports in `deleted`
fn opens_block(source: &Source<'_>, line_start: usize, deleted: &[DecoratedNode<'_>]) -> bool {
    let mut start = line_start;
    while start > source.start {
        let above = source.line_start(start - 1);
        let is_deleted = deleted
            .iter()
            .any(|import| source.line_start(import.span().start()) == above);
        if !is_deleted {
            return source.slice(above..start).trim_end().ends_with('{');
        }
        start = above;
    }

    false
}

/// Returns the edit that inserts the imports with the imports of the module
///
/// The imports go on their own lines, in source order, after the last
/// import of the module. When the module has none, they go before the first
/// item, after the inner attributes and docs of the module.
fn insertion(
    source: &Source<'_>,
    moved: &[DecoratedNode<'_>],
    module: &DecoratedNode<'_>,
) -> Option<Edit> {
    let span = module.span();
    let children = module.named_children();
    let last_import = children
        .iter()
        .rev()
        .find(|child| child.kind() == "use_declaration");

    let (position, replacement) = match last_import {
        Some(import) => {
            let end = import.span().end();
            let indent = source.indent(import.span().start());
            let line_end = source.line_end(end);
            let rest = source.slice(end..line_end).trim();
            let position = match rest.is_empty() || rest.starts_with("//") {
                true => line_end,
                false => end,
            };
            let text: String = moved
                .iter()
                .map(|node| format!("\n{indent}{}", reindented(source, node, indent)))
                .collect();
            (position, text)
        }
        None => {
            let start = first_item_start(source, &children)?;
            let indent = source.indent(start);
            let text: String = moved
                .iter()
                .map(|node| format!("{}\n", reindented(source, node, indent)))
                .collect::<Vec<_>>()
                .join(indent);
            (start, format!("{text}\n{indent}"))
        }
    };

    Some(
        Edit::builder()
            .span(
                Span::builder()
                    .file(span.file_path().clone())
                    .start(position)
                    .end(position)
                    .build(),
            )
            .replacement(replacement)
            .build(),
    )
}

/// Returns where the first item of a module starts, with the attributes
/// and the comments directly above it
///
/// A blank line ends the comments that belong to the item. Inner
/// attributes and inner doc comments belong to the module, so they stay
/// above.
fn first_item_start(source: &Source<'_>, children: &[DecoratedNode<'_>]) -> Option<usize> {
    let index = children.iter().position(|child| match child.kind() {
        "attribute_item" | "block_comment" | "inner_attribute_item" | "line_comment" => false,
        _ => true,
    })?;

    let mut start = children[index].span().start();
    for sibling in children[..index].iter().rev() {
        let attached = match sibling.kind() {
            "attribute_item" => true,
            "line_comment" | "block_comment" => {
                sibling.child_by_field_name("inner").is_none()
                    && source
                        .slice(sibling.span().end()..start)
                        .matches('\n')
                        .count()
                        <= 1
            }
            _ => false,
        };
        if !attached {
            break;
        }
        start = sibling.span().start();
    }

    Some(start)
}

/// Returns the text of `node` with each later line moved from the indent
/// of `node` to `indent`
///
/// The first line keeps its own start, because the edit puts it after the
/// indent already.
fn reindented(source: &Source<'_>, node: &DecoratedNode<'_>, indent: &str) -> String {
    let from = source.indent(node.span().start());
    let mut lines = node.text().split('\n');
    let mut result = lines.next().unwrap_or_default().to_owned();
    for line in lines {
        result.push('\n');
        match line.strip_prefix(from) {
            Some(rest) => {
                result.push_str(indent);
                result.push_str(rest);
            }
            None => result.push_str(line),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_send_binding() {
        fn assert_send<T: Send>() {}
        assert_send::<Binding>();
    }

    #[test]
    fn trait_send_removal() {
        fn assert_send<T: Send>() {}
        assert_send::<Removal<'_>>();
    }

    #[test]
    fn trait_send_source() {
        fn assert_send<T: Send>() {}
        assert_send::<Source<'_>>();
    }

    #[test]
    fn trait_send_standing() {
        fn assert_send<T: Send>() {}
        assert_send::<Standing>();
    }

    #[test]
    fn trait_sync_binding() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Binding>();
    }

    #[test]
    fn trait_sync_removal() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Removal<'_>>();
    }

    #[test]
    fn trait_sync_source() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Source<'_>>();
    }

    #[test]
    fn trait_sync_standing() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Standing>();
    }

    #[test]
    fn trait_unpin_binding() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Binding>();
    }

    #[test]
    fn trait_unpin_removal() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Removal<'_>>();
    }

    #[test]
    fn trait_unpin_source() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Source<'_>>();
    }

    #[test]
    fn trait_unpin_standing() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Standing>();
    }
}
