use std::path::Path;

use whisker_types::DecoratedNode;

/// Returns whether a function belongs to test code
///
/// A function is test code when its file sits in a `tests` or `benches`
/// directory. It is also test code when it or an item around it carries
/// `#[test]`, or a `cfg` that holds only under `test`. A file or a module
/// body gates its contents with an inner `#![cfg(test)]`, so the walk reads
/// inner attributes too.
pub(crate) fn is_test_code(function: &DecoratedNode<'_>) -> bool {
    if is_test_target(function.span().file()) {
        return true;
    }

    let mut current = Some(*function);
    while let Some(node) = current {
        if has_outer_test_attribute(&node) || has_inner_test_attribute(&node) {
            return true;
        }
        current = node.parent();
    }
    false
}

/// Returns whether any component of a path is `tests` or `benches`
///
/// Cargo builds those directories only for test and benchmark targets.
fn is_test_target(path: &Path) -> bool {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .any(|name| name == "tests" || name == "benches")
}

/// Returns whether a test attribute precedes the node
///
/// Tree-sitter makes each attribute a sibling of the item it decorates. The
/// search reads backward from the node over attributes and comments, and it
/// stops at the first sibling that is neither.
fn has_outer_test_attribute(node: &DecoratedNode<'_>) -> bool {
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
                if is_test_attribute(sibling) {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
    }
    false
}

/// Returns whether a test attribute opens the node's own children
///
/// The search stops at the first child that is neither an inner attribute
/// nor a comment.
fn has_inner_test_attribute(node: &DecoratedNode<'_>) -> bool {
    for child in node.named_children() {
        match child.kind() {
            "inner_attribute_item" => {
                if is_test_attribute(&child) {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
    }
    false
}

/// Returns whether an attribute item marks test code
///
/// `#[test]` and any attribute whose path ends in `test`, such as
/// `#[tokio::test]`, mark a test. A `cfg` marks test code only when its
/// predicate cannot hold outside a test build.
fn is_test_attribute(item: &DecoratedNode<'_>) -> bool {
    let Some(attribute) = item.named_child(0) else {
        return false;
    };
    let Some(path) = attribute.named_child(0) else {
        return false;
    };

    let name = match path.text().rsplit("::").next() {
        Some(name) => name.trim(),
        None => return false,
    };
    if name == "test" {
        return true;
    }
    if path.text() != "cfg" {
        return false;
    }

    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return false;
    };
    let Some(arguments) = arguments
        .text()
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
    else {
        return false;
    };
    cfg_requires_test(arguments)
}

/// Returns whether a `cfg` predicate holds only in a test build
///
/// `test` does, and so do `all(..)` with one such operand and `any(..)` whose
/// operands all are. `not(..)` never does: `#[cfg(not(test))]` is the code
/// that ships. A predicate the parser does not understand earns no
/// exemption.
fn cfg_requires_test(predicate: &str) -> bool {
    match parse_predicate(predicate) {
        Some((requires_test, rest)) => requires_test && rest.trim().is_empty(),
        None => false,
    }
}

/// Parses one `cfg` predicate from the front of the input
///
/// Returns whether the predicate holds only under `test`, and the input
/// after it.
fn parse_predicate(input: &str) -> Option<(bool, &str)> {
    let input = input.trim_start();
    let end = input
        .find(|character: char| !(character.is_alphanumeric() || character == '_'))
        .unwrap_or(input.len());
    let (word, rest) = input.split_at(end);
    if word.is_empty() {
        return None;
    }
    let rest = rest.trim_start();

    if let Some(rest) = rest.strip_prefix('=') {
        let rest = skip_string_literal(rest.trim_start())?;
        return Some((false, rest));
    }

    let Some(mut rest) = rest.strip_prefix('(') else {
        return Some((word == "test", rest));
    };

    let mut operands = Vec::new();
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix(')') {
            rest = after;
            break;
        }
        let (operand, after) = parse_predicate(rest)?;
        operands.push(operand);
        rest = after.trim_start();
        let Some(after) = rest.strip_prefix(',') else {
            rest = rest.strip_prefix(')')?;
            break;
        };
        rest = after;
    }

    let requires_test = match word {
        "all" => operands.iter().any(|operand| *operand),
        "any" => !operands.is_empty() && operands.iter().all(|operand| *operand),
        _ => false,
    };
    Some((requires_test, rest))
}

/// Returns the input after the string literal at its front
fn skip_string_literal(input: &str) -> Option<&str> {
    let mut characters = input.strip_prefix('"')?.char_indices();
    while let Some((index, character)) = characters.next() {
        match character {
            '\\' => {
                characters.next();
            }
            '"' => return Some(&input[index + 2..]),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg_requires_test_with_all_holding_test_returns_true() {
        assert!(cfg_requires_test("all(test, unix)"));
    }

    #[test]
    fn cfg_requires_test_with_any_holding_only_tests_returns_true() {
        assert!(cfg_requires_test("any(test, all(test, miri))"));
    }

    #[test]
    fn cfg_requires_test_with_any_holding_other_returns_false() {
        assert!(!cfg_requires_test("any(test, unix)"));
    }

    #[test]
    fn cfg_requires_test_with_empty_any_returns_false() {
        assert!(!cfg_requires_test("any()"));
    }

    #[test]
    fn cfg_requires_test_with_escaped_quote_in_string_returns_true() {
        assert!(cfg_requires_test("all(feature = \"a\\\"b\", test)"));
    }

    #[test]
    fn cfg_requires_test_with_feature_named_test_returns_false() {
        assert!(!cfg_requires_test("feature = \"test\""));
    }

    #[test]
    fn cfg_requires_test_with_not_test_returns_false() {
        assert!(!cfg_requires_test("not(test)"));
    }

    #[test]
    fn cfg_requires_test_with_test_returns_true() {
        assert!(cfg_requires_test(" test "));
    }

    #[test]
    fn cfg_requires_test_with_trailing_garbage_returns_false() {
        assert!(!cfg_requires_test("test test"));
    }

    #[test]
    fn cfg_requires_test_with_unclosed_list_returns_false() {
        assert!(!cfg_requires_test("all(test"));
    }

    #[test]
    fn cfg_requires_test_with_unterminated_string_returns_false() {
        assert!(!cfg_requires_test("feature = \"test"));
    }

    #[test]
    fn is_test_target_with_benchmark_path_returns_true() {
        assert!(is_test_target(Path::new("crates/whisker/benches/walk.rs")));
    }

    #[test]
    fn is_test_target_with_integration_test_path_returns_true() {
        assert!(is_test_target(Path::new("crates/whisker/tests/check.rs")));
    }

    #[test]
    fn is_test_target_with_source_path_returns_false() {
        assert!(!is_test_target(Path::new(
            "crates/whisker/src/tests_util.rs"
        )));
    }
}
