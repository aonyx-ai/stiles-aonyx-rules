//! Tells whether a node sits in the root file of a binary
//!
//! A binary meets its user in its root file: it reports its errors there,
//! and it writes to the terminal there. Rules that spare that file call
//! [`is_binary_root`], so they spare the same files.

use stiles_rust::decorations::{Target, TargetKind};
use stiles_types::DecoratedNode;

/// Returns whether `node` sits in the root file of a binary
///
/// The file must hold a `Target` decoration whose kinds are all
/// [`TargetKind::Binary`], so no library compiles it. It must also declare
/// `fn main` at its top level, because the root of a binary does and its
/// other modules do not. A file without the decoration, or whose kinds
/// are empty, is no root, because an exemption needs proof.
///
/// A rule that calls this function must declare `Target` in its `reads`.
///
/// # Examples
///
/// ```
/// use stiles_rust::decorations::TargetKind;
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let mut tree = parse("fn main() {}", Language::Rust);
/// test_decorations::target(&mut tree, [TargetKind::Binary]);
///
/// assert!(program_root::is_binary_root(&tree.root_node()));
/// ```
pub fn is_binary_root(node: &DecoratedNode<'_>) -> bool {
    let mut file = *node;
    while let Some(parent) = file.parent() {
        file = parent;
    }

    let only_binaries = file.decoration::<Target>().is_some_and(|target| {
        let kinds = target.kinds();
        !kinds.is_empty() && kinds.iter().all(|kind| kind == TargetKind::Binary)
    });

    only_binaries && declares_main(&file)
}

/// Returns whether the top level of `file` declares a function named `main`
fn declares_main(file: &DecoratedNode<'_>) -> bool {
    file.named_children().iter().any(|child| {
        child.kind() == "function_item"
            && child
                .child_by_field_name("name")
                .is_some_and(|name| name.text() == "main")
    })
}

#[cfg(test)]
mod tests {
    use stiles_testing::parse;
    use stiles_types::Language;

    use super::*;

    #[test]
    fn is_binary_root_with_a_binary_module_returns_false() {
        let mut tree = parse("pub fn render() {}", Language::Rust);
        test_decorations::target(&mut tree, [TargetKind::Binary]);

        let root = is_binary_root(&tree.root_node());

        assert!(!root);
    }

    #[test]
    fn is_binary_root_with_a_file_a_library_shares_returns_false() {
        let mut tree = parse("fn main() {}", Language::Rust);
        test_decorations::target(&mut tree, [TargetKind::Binary, TargetKind::Library]);

        let root = is_binary_root(&tree.root_node());

        assert!(!root);
    }

    #[test]
    fn is_binary_root_with_a_library_returns_false() {
        let mut tree = parse("fn main() {}", Language::Rust);
        test_decorations::target(&mut tree, [TargetKind::Library]);

        let root = is_binary_root(&tree.root_node());

        assert!(!root);
    }

    #[test]
    fn is_binary_root_with_a_nested_main_returns_false() {
        let mut tree = parse("mod inner {\n    fn main() {}\n}", Language::Rust);
        test_decorations::target(&mut tree, [TargetKind::Binary]);

        let root = is_binary_root(&tree.root_node());

        assert!(!root);
    }

    #[test]
    fn is_binary_root_with_no_kinds_returns_false() {
        let mut tree = parse("fn main() {}", Language::Rust);
        test_decorations::target(&mut tree, []);

        let root = is_binary_root(&tree.root_node());

        assert!(!root);
    }

    #[test]
    fn is_binary_root_with_no_target_returns_false() {
        let tree = parse("fn main() {}", Language::Rust);

        let root = is_binary_root(&tree.root_node());

        assert!(!root);
    }

    #[test]
    fn is_binary_root_within_a_binary_root_returns_true() {
        let mut tree = parse("fn main() {\n    run();\n}", Language::Rust);
        test_decorations::target(&mut tree, [TargetKind::Binary]);
        let function = tree.root_node().named_child(0).unwrap();
        let body = function.child_by_field_name("body").unwrap();

        let root = is_binary_root(&body);

        assert!(root);
    }
}
