//! Whether a signature or a type sits on a boundary that fixes it
//!
//! Stiles's guidance allows a primitive at a system boundary, and two
//! rules act on that: `lint.repeated-primitive-params` and
//! `lint.bool-param`. Both ask the same question of the same signatures,
//! under one option a project sets once, so they ask it here. A copy in
//! each rule could drift. One rule would then exempt a function the other
//! reports, which reads as a bug in whichever rule spoke.
//!
//! A type whose derive maps it onto a format outside Rust is a boundary
//! too: [`derives_a_boundary`] answers that. Only `lint.bool-param` reads
//! fields, so it is the one rule that asks.

use stiles_types::DecoratedNode;

pub mod corpus;

/// The option both rules read to learn which attributes mark a boundary
///
/// The rules read it under their own ids, so a project can name different
/// attributes for each. The name is the same in both, because it means the
/// same thing.
///
/// # Examples
///
/// ```
/// use boundary::OPTION;
///
/// assert_eq!(OPTION, "boundary-attributes");
/// ```
pub const OPTION: &str = "boundary-attributes";

/// Returns whether the signature at `node` sits on a boundary that fixes it
///
/// A function in an `extern` block, or one with an `extern` ABI, must match
/// a signature that a caller outside Rust fixes. Its parameter types are not
/// a free choice, so a rule that asks for better types is asking for
/// something the author cannot give.
///
/// An attribute macro that generates a bridge for such a caller does the
/// same thing, and no rule can know every framework's attribute. A project
/// names them in [`OPTION`], and `attributes` holds what it named.
///
/// # Examples
///
/// ```ignore
/// if crosses_a_boundary(node, &self.boundary_attributes) {
///     return Vec::new();
/// }
/// ```
pub fn crosses_a_boundary(node: &DecoratedNode<'_>, attributes: &[String]) -> bool {
    let extern_abi = node.named_children().iter().any(|child| {
        child.kind() == "function_modifiers"
            && child
                .named_children()
                .iter()
                .any(|modifier| modifier.kind() == "extern_modifier")
    });
    let extern_block = node
        .parent()
        .and_then(|parent| parent.parent())
        .is_some_and(|grandparent| grandparent.kind() == "foreign_mod_item");

    extern_abi || extern_block || carries_a_boundary_attribute(node, attributes)
}

/// The derive macros that map a type onto a format outside Rust
///
/// `Parser`, `Args`, `Subcommand`, and `ValueEnum` are clap's, and map the
/// type onto a command line. `Serialize` and `Deserialize` are serde's, and
/// map it onto a data format. A derive counts by the last segment of its
/// path, so `clap::Parser` and `Parser` count alike.
///
/// # Examples
///
/// ```
/// use boundary::DERIVES;
///
/// assert!(DERIVES.contains(&"Deserialize"));
/// ```
pub const DERIVES: &[&str] = &[
    "Args",
    "Deserialize",
    "Parser",
    "Serialize",
    "Subcommand",
    "ValueEnum",
];

/// Returns whether the type at `item` derives one of [`DERIVES`]
///
/// The format on the other side fixes the shape of each field. A command
/// line flag that takes no value is a `bool` to clap, and a JSON `true` is
/// a `bool` to serde. A rule that asks for a better type there asks for
/// something the format does not hold.
///
/// Only a `#[derive(...)]` written on the item counts. A derive under
/// `#[cfg_attr(...)]` applies only when its predicate holds, so it proves
/// nothing.
///
/// # Examples
///
/// ```
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let tree = parse("#[derive(Debug, clap::Args)]\nstruct A { b: bool }", Language::Rust);
/// let item = tree.root_node().named_child(1).unwrap();
///
/// assert!(boundary::derives_a_boundary(&item));
/// ```
pub fn derives_a_boundary(item: &DecoratedNode<'_>) -> bool {
    attribute::derives(item)
        .iter()
        .any(|derive| DERIVES.contains(derive))
}

/// Returns whether an attribute on the signature is one of `attributes`
///
/// An attribute is a sibling that precedes the item. The walk goes backwards
/// and stops at the first sibling that is neither an attribute nor a comment.
/// A doc comment between two attributes therefore does not end the run, and
/// an attribute on the item before this one does not reach it.
///
/// A configured name matches the last segment of the attribute's path, so
/// `shard` covers both `#[shard]` and `#[topcoat::shard]`. They are one
/// macro, and which one a file writes depends on its imports.
fn carries_a_boundary_attribute(node: &DecoratedNode<'_>, attributes: &[String]) -> bool {
    if attributes.is_empty() {
        return false;
    }

    let Some(parent) = node.parent() else {
        return false;
    };
    let siblings = parent.named_children();
    let Some(position) = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())
    else {
        return false;
    };

    for sibling in siblings[..position].iter().rev() {
        let attribute = match sibling.kind() {
            "attribute_item" => sibling.named_child(0),
            "line_comment" => continue,
            "block_comment" => continue,
            _ => return false,
        };
        let Some(attribute) = attribute else { continue };
        let Some(path) = attribute.named_child(0) else {
            continue;
        };

        let name = path.text();
        let name = name.rsplit("::").next().unwrap_or(name).trim();
        if attributes.iter().any(|candidate| candidate == name) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use stiles_testing::parse;
    use stiles_types::{DecoratedTree, Language};

    use super::*;

    /// Returns the first signature in `source`, wherever it sits
    ///
    /// A method in an `impl` block is nested, and so is a declaration in an
    /// `extern` block. The walk has to reach both.
    fn signature<'a>(tree: &'a DecoratedTree) -> DecoratedNode<'a> {
        fn find<'a>(node: &DecoratedNode<'a>) -> Option<DecoratedNode<'a>> {
            match node.kind() {
                "function_item" => return Some(*node),
                "function_signature_item" => return Some(*node),
                _ => {}
            }

            for child in node.named_children() {
                if let Some(found) = find(&child) {
                    return Some(found);
                }
            }

            None
        }

        find(&tree.root_node()).expect("the source should hold a signature")
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    /// Reports whether the first signature in `source` sits on a boundary
    fn crosses(source: &str, attributes: &[&str]) -> bool {
        let tree = parse(source, Language::Rust);

        crosses_a_boundary(&signature(&tree), &names(attributes))
    }

    #[test]
    fn crosses_a_boundary_with_a_comment_between_attributes_is_true() {
        let crossed = crosses(
            "#[shard]\n/// Doc\n#[inline]\nfn f(a: String) {}",
            &["shard"],
        );

        assert!(crossed);
    }

    #[test]
    fn crosses_a_boundary_with_a_configured_attribute_is_true() {
        let crossed = crosses("#[shard]\nfn f(a: String) {}", &["shard"]);

        assert!(crossed);
    }

    #[test]
    fn crosses_a_boundary_with_a_method_attribute_is_true() {
        let crossed = crosses(
            "impl S {\n    #[shard]\n    fn f(a: String) {}\n}",
            &["shard"],
        );

        assert!(crossed);
    }

    #[test]
    fn crosses_a_boundary_with_a_pathed_attribute_is_true() {
        let crossed = crosses("#[topcoat::shard]\nfn f(a: String) {}", &["shard"]);

        assert!(crossed);
    }

    #[test]
    fn crosses_a_boundary_with_an_extern_abi_is_true() {
        let crossed = crosses("pub extern \"C\" fn f(a: usize) {}", &[]);

        assert!(crossed);
    }

    #[test]
    fn crosses_a_boundary_with_an_extern_block_is_true() {
        let crossed = crosses("unsafe extern \"C\" { fn f(a: usize); }", &[]);

        assert!(crossed);
    }

    #[test]
    fn crosses_a_boundary_with_another_attribute_is_false() {
        let crossed = crosses("#[inline]\nfn f(a: String) {}", &["shard"]);

        assert!(!crossed);
    }

    #[test]
    fn crosses_a_boundary_with_no_attribute_configured_is_false() {
        let crossed = crosses("#[shard]\nfn f(a: String) {}", &[]);

        assert!(!crossed);
    }

    #[test]
    fn crosses_a_boundary_with_the_attribute_on_the_item_before_is_false() {
        let source = "#[shard]\nfn a(x: String) {}\nfn b(y: String) {}";
        let tree = parse(source, Language::Rust);
        let second = tree
            .root_node()
            .named_children()
            .into_iter()
            .filter(|node| node.kind() == "function_item")
            .nth(1)
            .expect("the source should hold two functions");

        let crossed = crosses_a_boundary(&second, &names(&["shard"]));

        assert!(!crossed);
    }

    #[test]
    fn crosses_a_boundary_without_an_attribute_is_false() {
        let crossed = crosses("fn f(a: String) {}", &["shard"]);

        assert!(!crossed);
    }

    /// Reports whether the last item in `source` derives a boundary
    fn derived(source: &str) -> bool {
        let tree = parse(source, Language::Rust);
        let item = tree
            .root_node()
            .named_children()
            .into_iter()
            .rev()
            .find(|node| node.kind() != "attribute_item" && node.kind() != "line_comment")
            .expect("the source should hold an item");

        derives_a_boundary(&item)
    }

    #[test]
    fn derives_a_boundary_under_cfg_attr_is_false() {
        let derived = derived("#[cfg_attr(feature = \"cli\", derive(clap::Args))]\nstruct A;");

        assert!(!derived);
    }

    #[test]
    fn derives_a_boundary_with_a_clap_derive_is_true() {
        let derived = derived("#[derive(Debug, Args)]\nstruct A;");

        assert!(derived);
    }

    #[test]
    fn derives_a_boundary_with_a_pathed_serde_derive_is_true() {
        let derived = derived("#[derive(Clone, serde::Deserialize)]\nstruct A;");

        assert!(derived);
    }

    #[test]
    fn derives_a_boundary_with_a_value_enum_is_true() {
        let derived = derived("#[derive(Clone, ValueEnum)]\nenum Format { Text }");

        assert!(derived);
    }

    #[test]
    fn derives_a_boundary_with_other_derives_is_false() {
        let derived = derived("#[derive(Clone, Debug, bon::Builder)]\nstruct A;");

        assert!(!derived);
    }

    #[test]
    fn option_names_the_shared_option() {
        let option = OPTION;

        assert_eq!(option, "boundary-attributes");
    }
}
