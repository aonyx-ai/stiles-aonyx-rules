use whisker_types::DecoratedNode;

/// Returns the full path a reference in a function body names
///
/// The first segment is looked up in the `use` declarations of every scope
/// around the reference, innermost first: each enclosing block, then the
/// module. The walk stops at the module, because a nested module does not
/// inherit its parent's imports. A segment that no import binds stays as
/// written, so `std::fs::read` resolves to itself. A path that starts with
/// `::` names an external crate and skips the lookup.
///
/// A glob import binds no name the walk can see, so `use std::fs::*` hides
/// a bare `read_to_string`. The rule accepts that false negative.
pub(crate) fn resolve(reference: &DecoratedNode<'_>) -> Option<Vec<String>> {
    let text = reference.text().trim_start();
    let segments = path_segments(text)?;
    if text.starts_with("::") {
        return Some(segments);
    }
    let (first, rest) = segments.split_first()?;

    let mut current = reference.parent();
    while let Some(scope) = current {
        if is_import_scope(&scope) {
            let binding = bindings_in(&scope)
                .into_iter()
                .find(|(name, _)| name == first);
            if let Some((_, path)) = binding {
                return Some(path.into_iter().chain(rest.iter().cloned()).collect());
            }
        }
        if is_module_scope(&scope) {
            break;
        }
        current = scope.parent();
    }

    Some(segments)
}

/// Returns whether `use` declarations directly under the node bind names
fn is_import_scope(node: &DecoratedNode<'_>) -> bool {
    node.kind() == "block" || is_module_scope(node)
}

/// Returns whether the node is the body of a file or an inline module
///
/// An `impl` or a `trait` also holds its items in a `declaration_list`, but
/// it opens no module, so the walk passes through it.
fn is_module_scope(node: &DecoratedNode<'_>) -> bool {
    match node.kind() {
        "source_file" => true,
        "declaration_list" => node
            .parent()
            .is_some_and(|parent| parent.kind() == "mod_item"),
        _ => false,
    }
}

/// Returns every name the `use` declarations directly under a scope bind
///
/// Each binding pairs the local name with the full path it stands for.
fn bindings_in(scope: &DecoratedNode<'_>) -> Vec<(String, Vec<String>)> {
    let mut bindings = Vec::new();
    for child in scope.named_children() {
        if child.kind() != "use_declaration" {
            continue;
        }
        let Some(argument) = child.child_by_field_name("argument") else {
            continue;
        };
        collect_bindings(&argument, &[], &mut bindings);
    }
    bindings
}

/// Adds the names one use tree binds, under the prefix of its enclosing list
///
/// `self` in a list binds the list's own last segment, so
/// `use std::fs::{self, File}` binds both `fs` and `File`. An alias of `_`
/// binds nothing a call can name.
fn collect_bindings(
    node: &DecoratedNode<'_>,
    prefix: &[String],
    bindings: &mut Vec<(String, Vec<String>)>,
) {
    match node.kind() {
        "identifier" | "scoped_identifier" => {
            let Some(path) = extend(prefix, node) else {
                return;
            };
            let Some(name) = path.last() else {
                return;
            };
            bindings.push((name.clone(), path));
        }
        "self" => {
            let Some(name) = prefix.last() else {
                return;
            };
            bindings.push((name.clone(), prefix.to_vec()));
        }
        "use_as_clause" => {
            let Some(alias) = node.child_by_field_name("alias") else {
                return;
            };
            let alias = strip_raw(alias.text());
            if alias == "_" {
                return;
            }
            let Some(path) = node.child_by_field_name("path") else {
                return;
            };
            let path = match path.kind() {
                "self" => prefix.to_vec(),
                _ => match extend(prefix, &path) {
                    Some(path) => path,
                    None => return,
                },
            };
            bindings.push((alias.to_string(), path));
        }
        "scoped_use_list" => {
            let prefix = match node.child_by_field_name("path") {
                Some(path) => match extend(prefix, &path) {
                    Some(prefix) => prefix,
                    None => return,
                },
                None => prefix.to_vec(),
            };
            let Some(list) = node.child_by_field_name("list") else {
                return;
            };
            collect_bindings(&list, &prefix, bindings);
        }
        "use_list" => {
            for child in node.named_children() {
                collect_bindings(&child, prefix, bindings);
            }
        }
        _ => {}
    }
}

/// Returns the prefix followed by the segments of a path node
fn extend(prefix: &[String], path: &DecoratedNode<'_>) -> Option<Vec<String>> {
    let segments = path_segments(path.text())?;
    Some(prefix.iter().cloned().chain(segments).collect())
}

/// Splits a path into its segments, without generic arguments
///
/// `Vec::<u8>::new` becomes `Vec` and `new`. A qualified path such as
/// `<File as Read>::read` names its type through a trait, which no import
/// lookup can follow, so it yields nothing. A leading `::` leaves no empty
/// segment behind.
pub(crate) fn path_segments(text: &str) -> Option<Vec<String>> {
    let text = text.trim();
    if text.starts_with('<') {
        return None;
    }

    let mut depth = 0usize;
    let mut stripped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth > 0 || character.is_whitespace() => {}
            _ => stripped.push(character),
        }
    }

    let segments: Vec<String> = stripped
        .split("::")
        .filter(|segment| !segment.is_empty())
        .map(|segment| strip_raw(segment).to_string())
        .collect();
    if segments.is_empty() {
        return None;
    }
    Some(segments)
}

/// Returns an identifier without its `r#` raw prefix
pub(crate) fn strip_raw(identifier: &str) -> &str {
    identifier.strip_prefix("r#").unwrap_or(identifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(text: &str) -> Option<Vec<String>> {
        path_segments(text)
    }

    #[test]
    fn path_segments_with_generic_arguments_drops_them() {
        let text = "Vec::<Option<u8>>::new";

        let result = segments(text);

        assert_eq!(result, Some(vec!["Vec".to_string(), "new".to_string()]));
    }

    #[test]
    fn path_segments_with_leading_colons_drops_the_empty_segment() {
        let text = "::std::fs::read";

        let result = segments(text);

        assert_eq!(
            result,
            Some(vec![
                "std".to_string(),
                "fs".to_string(),
                "read".to_string()
            ])
        );
    }

    #[test]
    fn path_segments_with_qualified_path_returns_none() {
        let text = "<File as Read>::read";

        let result = segments(text);

        assert_eq!(result, None);
    }

    #[test]
    fn path_segments_with_raw_identifier_strips_the_prefix() {
        let text = "r#fs::r#read";

        let result = segments(text);

        assert_eq!(result, Some(vec!["fs".to_string(), "read".to_string()]));
    }

    #[test]
    fn path_segments_with_whitespace_ignores_it() {
        let text = "std :: fs\n    :: read";

        let result = segments(text);

        assert_eq!(
            result,
            Some(vec![
                "std".to_string(),
                "fs".to_string(),
                "read".to_string()
            ])
        );
    }
}
