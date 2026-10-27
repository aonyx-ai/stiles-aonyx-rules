use std::path::Path;

use whisker_types::DecoratedNode;

/// Returns whether `node` sits in code that only a test build compiles
///
/// That is code in a `tests` or `benches` directory, which Cargo builds only
/// for test and benchmark targets. It is also code under a test attribute
/// such as `#[test]`. Last, it is code under a `cfg` whose predicate holds
/// only under `test`. The `cfg` counts as an outer attribute on any
/// enclosing item and as an inner `#![cfg(test)]` of any enclosing scope.
///
/// `#[cfg(not(test))]` and `#[cfg(any(test, unix))]` compile outside a test
/// build, so neither counts.
pub(crate) fn is_test_code(node: &DecoratedNode<'_>) -> bool {
    if is_test_target(node.span().file()) {
        return true;
    }

    let mut current = Some(*node);
    while let Some(scope) = current {
        let marked = outer_attributes(&scope)
            .iter()
            .chain(inner_attributes(&scope).iter())
            .any(marks_test);
        if marked {
            return true;
        }
        current = scope.parent();
    }

    false
}

/// Returns the attributes written before `item`, nearest first
///
/// Tree-sitter leaves an item's attributes beside it rather than inside it.
/// The walk goes backwards over the siblings. It stops at the first one that
/// is neither an attribute nor a comment. A doc comment between two
/// attributes therefore does not end the run. Each entry is the `attribute` node
/// inside the `attribute_item`.
fn outer_attributes<'a>(item: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let Some(parent) = item.parent() else {
        return Vec::new();
    };
    let siblings = parent.named_children();
    let Some(position) = siblings
        .iter()
        .position(|sibling| sibling.id() == item.id())
    else {
        return Vec::new();
    };

    let mut attributes = Vec::new();
    for sibling in siblings[..position].iter().rev() {
        match sibling.kind() {
            "attribute_item" => attributes.extend(sibling.named_child(0)),
            "line_comment" => {}
            "block_comment" => {}
            _ => break,
        }
    }
    attributes
}

/// Returns the `#![...]` attributes written directly inside `scope`
fn inner_attributes<'a>(scope: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    scope
        .named_children()
        .into_iter()
        .filter(|child| child.kind() == "inner_attribute_item")
        .filter_map(|child| child.named_child(0))
        .collect()
}

/// Returns the last segment of an attribute's path, such as `test` for
/// `#[tokio::test]`
fn attribute_name<'a>(attribute: &DecoratedNode<'a>) -> Option<&'a str> {
    let path = attribute.named_child(0)?;
    let name = path.text();
    Some(name.rsplit("::").next().unwrap_or(name).trim())
}

/// Returns whether one attribute marks test code
///
/// A test attribute counts by the last segment of its path, so `#[test]` and
/// `#[tokio::test(flavor = "multi_thread")]` both count. A `cfg` counts when
/// its predicate holds only in a test build.
fn marks_test(attribute: &DecoratedNode<'_>) -> bool {
    match attribute_name(attribute) {
        Some("test") => true,
        Some("cfg") => {
            let is_bare_cfg = attribute
                .named_child(0)
                .is_some_and(|path| path.text() == "cfg");
            let arguments = attribute.child_by_field_name("arguments");
            match (is_bare_cfg, arguments) {
                (true, Some(arguments)) => {
                    let predicates = predicates(&arguments);
                    predicates.len() == 1 && requires_test(&predicates[0])
                }
                (true, None) => false,
                (false, _) => false,
            }
        }
        Some(_) => false,
        None => false,
    }
}

/// One `cfg` predicate: a name, and what follows it
struct Predicate<'a> {
    name: &'a str,
    form: Form<'a>,
}

/// What follows the name of a `cfg` predicate
enum Form<'a> {
    /// Nothing follows, as in `test`
    Bare,
    /// A list of predicates follows, as in `all(test, unix)`
    List(DecoratedNode<'a>),
    /// Anything else follows, such as the value in `feature = "test"`
    Valued,
}

/// Splits the inside of a token tree such as `(test, unix)` into predicates
///
/// A token tree holds its commas and `=` signs as unnamed children, so the
/// walk reads every child. A group that does not start with a name is no
/// predicate the rule can read, and yields one that requires nothing.
fn predicates<'a>(tree: &DecoratedNode<'a>) -> Vec<Predicate<'a>> {
    let mut groups: Vec<Vec<DecoratedNode<'a>>> = vec![Vec::new()];
    for index in 0..tree.child_count() as u32 {
        let Some(child) = tree.child(index) else {
            continue;
        };
        match child.kind() {
            "(" | ")" => {}
            "," => groups.push(Vec::new()),
            _ => {
                if let Some(group) = groups.last_mut() {
                    group.push(child);
                }
            }
        }
    }

    groups
        .into_iter()
        .filter(|group| !group.is_empty())
        .map(|group| {
            let name = match group[0].kind() {
                "identifier" => group[0].text(),
                _ => "",
            };
            let form = match &group[1..] {
                [] => Form::Bare,
                [list] if list.kind() == "token_tree" => Form::List(*list),
                _ => Form::Valued,
            };
            Predicate { name, form }
        })
        .collect()
}

/// Returns whether a predicate holds only when `test` holds
///
/// `all` needs one argument that requires `test`. `any` needs every
/// argument to require it, and at least one argument. `not` and every other
/// name never require it.
fn requires_test(predicate: &Predicate<'_>) -> bool {
    let Predicate { name, form } = predicate;
    match form {
        Form::Bare => *name == "test",
        Form::Valued => false,
        Form::List(list) => {
            let arguments = predicates(list);
            match *name {
                "all" => arguments.iter().any(requires_test),
                "any" => !arguments.is_empty() && arguments.iter().all(requires_test),
                _ => false,
            }
        }
    }
}

/// Returns whether any directory in a path is `tests` or `benches`
///
/// Cargo builds those directories only for test and benchmark targets, so
/// the code in them never ships.
fn is_test_target(path: &Path) -> bool {
    let Some(directory) = path.parent() else {
        return false;
    };
    directory
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .any(|name| name == "tests" || name == "benches")
}

#[cfg(test)]
mod tests {
    use whisker_testing::parse;
    use whisker_types::{DecoratedTree, Language};

    use super::*;

    /// Returns the function named `f` in `source`, wherever it sits
    fn function<'a>(tree: &'a DecoratedTree) -> DecoratedNode<'a> {
        fn find<'a>(node: &DecoratedNode<'a>) -> Option<DecoratedNode<'a>> {
            let is_f = node
                .child_by_field_name("name")
                .is_some_and(|name| name.text() == "f");
            if node.kind() == "function_item" && is_f {
                return Some(*node);
            }
            node.named_children().iter().find_map(find)
        }

        find(&tree.root_node()).expect("the source should hold a function named `f`")
    }

    fn is_test(source: &str) -> bool {
        let tree = parse(source, Language::Rust);

        is_test_code(&function(&tree))
    }

    #[test]
    fn is_test_code_with_all_including_test_returns_true() {
        assert!(is_test("#[cfg(all(unix, test))]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_all_of_not_test_returns_false() {
        assert!(!is_test("#[cfg(all(not(test), unix))]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_any_of_only_test_returns_true() {
        assert!(is_test("#[cfg(any(test))]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_any_including_another_predicate_returns_false() {
        assert!(!is_test("#[cfg(any(test, unix))]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_cfg_attr_returns_false() {
        assert!(!is_test("#[cfg_attr(test, derive(Debug))]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_cfg_not_test_returns_false() {
        assert!(!is_test("#[cfg(not(test))]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_cfg_test_on_the_function_returns_true() {
        assert!(is_test("#[cfg(test)]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_cfg_test_past_a_doc_comment_returns_true() {
        assert!(is_test("#[cfg(test)]\n/// Doc\n#[inline]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_empty_any_returns_false() {
        assert!(!is_test("#[cfg(any())]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_feature_named_test_returns_false() {
        assert!(!is_test("#[cfg(feature = \"test\")]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_inner_cfg_test_in_the_file_returns_true() {
        assert!(is_test("#![cfg(test)]\n\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_inner_cfg_test_in_a_module_returns_true() {
        assert!(is_test("mod m {\n    #![cfg(test)]\n    fn f() {}\n}"));
    }

    #[test]
    fn is_test_code_with_nested_all_returns_true() {
        assert!(is_test(
            "#[cfg(all(unix, all(test, debug_assertions)))]\nfn f() {}"
        ));
    }

    #[test]
    fn is_test_code_with_pathed_cfg_returns_false() {
        assert!(!is_test("#[other::cfg(test)]\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_plain_function_returns_false() {
        assert!(!is_test("fn f() {}"));
    }

    #[test]
    fn is_test_code_with_test_attribute_on_an_enclosing_function_returns_true() {
        assert!(is_test("#[test]\nfn outer() {\n    fn f() {}\n}"));
    }

    #[test]
    fn is_test_code_with_test_attribute_on_an_item_before_returns_false() {
        assert!(!is_test("#[cfg(test)]\nmod tests {}\nfn f() {}"));
    }

    #[test]
    fn is_test_code_with_test_module_around_it_returns_true() {
        assert!(is_test("#[cfg(test)]\nmod tests {\n    fn f() {}\n}"));
    }

    #[test]
    fn is_test_code_with_tokio_test_attribute_returns_true() {
        assert!(is_test(
            "#[tokio::test(flavor = \"multi_thread\")]\nfn f() {}"
        ));
    }

    #[test]
    fn is_test_target_with_benches_directory_returns_true() {
        assert!(is_test_target(Path::new("crates/whisker/benches/walk.rs")));
    }

    #[test]
    fn is_test_target_with_file_named_tests_returns_false() {
        assert!(!is_test_target(Path::new("crates/whisker/src/tests.rs")));
    }

    #[test]
    fn is_test_target_with_source_path_returns_false() {
        assert!(!is_test_target(Path::new("crates/whisker/src/lib.rs")));
    }

    #[test]
    fn is_test_target_with_tests_directory_returns_true() {
        assert!(is_test_target(Path::new("crates/whisker/tests/check.rs")));
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Predicate<'static>>();
        assert_send::<Form<'static>>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Predicate<'static>>();
        assert_sync::<Form<'static>>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Predicate<'static>>();
        assert_unpin::<Form<'static>>();
    }
}
