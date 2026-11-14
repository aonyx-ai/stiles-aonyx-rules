//! Attaches the decorations that tell test code apart, as the provider does
//!
//! `stiles_rust::is_test_code` reads [`TestCode`] on an item and [`Target`]
//! on a file. A rule test parses its source without a provider, so it
//! attaches both by hand. [`mark`] and [`enclose_file`] attach [`TestCode`],
//! and [`target`] attaches [`Target`].

use stiles_rust::decorations::{Edition, Target, TargetKind, TestCode};
use stiles_types::{DecoratedNode, DecoratedTree};

/// The node kinds that the provider decorates as items
const ITEM_KINDS: [&str; 17] = [
    "associated_type",
    "const_item",
    "enum_item",
    "extern_crate_declaration",
    "foreign_mod_item",
    "function_item",
    "function_signature_item",
    "impl_item",
    "macro_definition",
    "macro_invocation",
    "mod_item",
    "static_item",
    "struct_item",
    "trait_item",
    "type_item",
    "union_item",
    "use_declaration",
];

/// Attaches [`TestCode::Marked`] to the first item whose text opens with `opening`
///
/// The provider marks an item that carries a test attribute or a `cfg`
/// that implies `test`. An item's outer attributes are siblings of the item
/// in the tree, so `opening` is the text after them, such as `mod tests`.
/// The search runs in source order and visits an item before the items
/// inside it.
///
/// # Panics
///
/// Panics when no item opens with `opening`.
///
/// # Examples
///
/// ```
/// use stiles_rust::decorations::TestCode;
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let mut tree = parse("#[cfg(test)]\nmod tests {}", Language::Rust);
///
/// test_decorations::mark(&mut tree, "mod tests");
///
/// let module = tree.root_node().named_child(1).unwrap();
/// assert_eq!(module.decoration::<TestCode>(), Some(&TestCode::Marked));
/// ```
pub fn mark(tree: &mut DecoratedTree, opening: &str) {
    let id = find_item(&tree.root_node(), opening)
        .unwrap_or_else(|| panic!("the source should hold an item that opens with `{opening}`"));
    tree.decorations_mut().insert(id, TestCode::Marked);
}

/// Attaches [`TestCode::Enclosed`] to every item at the top level of the file
///
/// The provider does this for a file whose own module is test code. That
/// is a file that opens with `#![cfg(test)]`, or one whose parent declares
/// it under `#[cfg(test)]`.
///
/// # Examples
///
/// ```
/// use stiles_rust::decorations::TestCode;
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let mut tree = parse("#![cfg(test)]\nfn helper() {}", Language::Rust);
///
/// test_decorations::enclose_file(&mut tree);
///
/// let function = tree.root_node().named_child(1).unwrap();
/// assert_eq!(function.decoration::<TestCode>(), Some(&TestCode::Enclosed));
/// ```
pub fn enclose_file(tree: &mut DecoratedTree) {
    let items: Vec<usize> = tree
        .root_node()
        .named_children()
        .into_iter()
        .filter(|child| ITEM_KINDS.contains(&child.kind()))
        .map(|child| child.id())
        .collect();
    for id in items {
        tree.decorations_mut().insert(id, TestCode::Enclosed);
    }
}

/// Attaches a [`Target`] with `kinds` to the file
///
/// The provider attaches it to the `source_file` node, with the kinds of
/// every Cargo target that compiles the file. The crate name and the
/// edition are fixed, because no rule here reads them.
///
/// # Examples
///
/// ```
/// use stiles_rust::decorations::{Target, TargetKind};
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let mut tree = parse("fn helper() {}", Language::Rust);
///
/// test_decorations::target(&mut tree, [TargetKind::IntegrationTest]);
///
/// let target = tree.root_node().decoration::<Target>().cloned();
/// assert!(target.is_some_and(|target| target.kinds().contains(TargetKind::IntegrationTest)));
/// ```
pub fn target(tree: &mut DecoratedTree, kinds: impl IntoIterator<Item = TargetKind>) {
    let target = Target::builder()
        .krate("sample")
        .kinds(kinds.into_iter().collect())
        .edition(Edition::Edition2024)
        .build();
    let root = tree.root_node().id();
    tree.decorations_mut().insert(root, target);
}

/// Returns the id of the first item at or below `node` that opens with `opening`
fn find_item(node: &DecoratedNode<'_>, opening: &str) -> Option<usize> {
    if ITEM_KINDS.contains(&node.kind()) && node.text().starts_with(opening) {
        return Some(node.id());
    }
    node.named_children()
        .iter()
        .find_map(|child| find_item(child, opening))
}

#[cfg(test)]
mod tests {
    use stiles_testing::parse;
    use stiles_types::Language;

    use super::*;

    #[test]
    fn enclose_file_skips_an_inner_attribute() {
        let mut tree = parse("#![cfg(test)]\nfn helper() {}", Language::Rust);

        enclose_file(&mut tree);

        let attribute = tree.root_node().named_child(0).unwrap();
        assert!(attribute.decoration::<TestCode>().is_none());
    }

    #[test]
    fn mark_finds_an_item_inside_another() {
        let mut tree = parse("mod outer {\n    fn helper() {}\n}", Language::Rust);

        mark(&mut tree, "fn helper");

        let module = tree.root_node().named_child(0).unwrap();
        let body = module.child_by_field_name("body").unwrap();
        let function = body.named_child(0).unwrap();
        assert_eq!(function.decoration::<TestCode>(), Some(&TestCode::Marked));
    }

    #[test]
    fn mark_leaves_the_enclosing_item_alone() {
        let mut tree = parse("mod outer {\n    fn helper() {}\n}", Language::Rust);

        mark(&mut tree, "fn helper");

        let module = tree.root_node().named_child(0).unwrap();
        assert!(module.decoration::<TestCode>().is_none());
    }

    #[test]
    #[should_panic(expected = "an item that opens with `mod tests`")]
    fn mark_without_a_matching_item_panics() {
        let mut tree = parse("fn helper() {}", Language::Rust);

        mark(&mut tree, "mod tests");
    }

    #[test]
    fn target_keeps_earlier_decorations() {
        let mut tree = parse("fn helper() {}", Language::Rust);
        mark(&mut tree, "fn helper");

        target(&mut tree, [TargetKind::Bench]);

        let function = tree.root_node().named_child(0).unwrap();
        assert_eq!(function.decoration::<TestCode>(), Some(&TestCode::Marked));
    }
}
