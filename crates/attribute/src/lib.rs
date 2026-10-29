//! The outer attributes of an item, and what their syntax states
//!
//! Tree-sitter leaves an item's attributes beside the item rather than inside
//! it. A rule that exempts an item for an attribute it carries has to walk
//! back over the siblings to find them. [`outer`] does that walk, and the
//! other functions read one attribute that it returns.

use stiles_types::DecoratedNode;

/// Returns the attributes written before `item`, nearest first
///
/// The walk goes backwards over the siblings. It stops at the first one that
/// is neither an attribute nor a comment. A doc comment between two
/// attributes therefore does not end the run, and an attribute on the item
/// before this one does not reach it. Each entry is the `attribute` node
/// inside the `attribute_item`.
///
/// # Examples
///
/// ```
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let tree = parse("#[repr(C)]\n/// Docs\nstruct S;", Language::Rust);
/// let item = tree.root_node().named_child(2).unwrap();
///
/// let attributes = attribute::outer(&item);
///
/// assert_eq!(attributes[0].text(), "repr(C)");
/// ```
pub fn outer<'a>(item: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
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

/// Returns the path of an attribute as written, such as `stabby::stabby`
///
/// # Examples
///
/// ```
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let tree = parse("#[stabby::stabby]\nstruct S;", Language::Rust);
/// let item = tree.root_node().named_child(1).unwrap();
/// let attributes = attribute::outer(&item);
///
/// assert_eq!(attribute::path(&attributes[0]), Some("stabby::stabby"));
/// ```
pub fn path<'a>(attribute: &DecoratedNode<'a>) -> Option<&'a str> {
    attribute.named_child(0).map(|path| path.text())
}

/// Returns the last segment of an attribute's path, such as `test` for
/// `#[tokio::test]`
///
/// # Examples
///
/// ```
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let tree = parse("#[tokio::test]\nfn f() {}", Language::Rust);
/// let item = tree.root_node().named_child(1).unwrap();
/// let attributes = attribute::outer(&item);
///
/// assert_eq!(attribute::name(&attributes[0]), Some("test"));
/// ```
pub fn name<'a>(attribute: &DecoratedNode<'a>) -> Option<&'a str> {
    let path = path(attribute)?;
    Some(path.rsplit("::").next().unwrap_or(path).trim())
}

/// Returns every identifier at the top level of an attribute's argument
/// list, in order
///
/// `#[repr(align(8), C)]` gives `align` and `C`. The `8` sits in a nested
/// token tree, so it does not count. An attribute without arguments gives
/// none.
///
/// # Examples
///
/// ```
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let tree = parse("#[repr(align(8), C)]\nstruct S;", Language::Rust);
/// let item = tree.root_node().named_child(1).unwrap();
/// let attributes = attribute::outer(&item);
///
/// assert_eq!(attribute::words(&attributes[0]), vec!["align", "C"]);
/// ```
pub fn words<'a>(attribute: &DecoratedNode<'a>) -> Vec<&'a str> {
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return Vec::new();
    };
    arguments
        .named_children()
        .into_iter()
        .filter(|child| child.kind() == "identifier")
        .map(|child| child.text())
        .collect()
}

#[cfg(test)]
mod tests {
    use stiles_testing::parse;
    use stiles_types::{DecoratedTree, Language};

    use super::*;

    /// Returns the last item in `source`, which every test puts its
    /// attributes on
    fn last_item(tree: &DecoratedTree) -> DecoratedNode<'_> {
        tree.root_node()
            .named_children()
            .into_iter()
            .rev()
            .find(|node| node.kind().ends_with("_item") && node.kind() != "attribute_item")
            .expect("the source should hold an item")
    }

    /// Returns the text of every outer attribute on the last item in `source`
    fn outer_texts(source: &str) -> Vec<String> {
        let tree = parse(source, Language::Rust);

        outer(&last_item(&tree))
            .iter()
            .map(|attribute| attribute.text().to_owned())
            .collect()
    }

    /// Returns the words of the first outer attribute on the last item
    fn first_words(source: &str) -> Vec<String> {
        let tree = parse(source, Language::Rust);
        let attributes = outer(&last_item(&tree));

        words(&attributes[0])
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn name_of_a_bare_attribute_is_the_path() {
        let tree = parse("#[builder]\nfn f() {}", Language::Rust);
        let attributes = outer(&last_item(&tree));

        assert_eq!(name(&attributes[0]), Some("builder"));
    }

    #[test]
    fn name_of_a_pathed_attribute_is_the_last_segment() {
        let tree = parse(
            "#[bon::builder(on(String, into))]\nfn f() {}",
            Language::Rust,
        );
        let attributes = outer(&last_item(&tree));

        assert_eq!(name(&attributes[0]), Some("builder"));
    }

    #[test]
    fn outer_across_a_doc_comment_holds_both_attributes() {
        let texts = outer_texts("#[repr(C)]\n/// Docs\n#[derive(Debug)]\nstruct S;");

        assert_eq!(texts, vec!["derive(Debug)", "repr(C)"]);
    }

    #[test]
    fn outer_of_a_method_holds_its_attribute() {
        let tree = parse(
            "impl S {\n    #[builder]\n    fn new() {}\n}",
            Language::Rust,
        );
        let body = tree.root_node().named_child(0).unwrap();
        let body = body.child_by_field_name("body").unwrap();
        let method = body.named_children().into_iter().last().unwrap();

        let attributes = outer(&method);

        assert_eq!(attributes[0].text(), "builder");
    }

    #[test]
    fn outer_stops_at_the_item_before() {
        let texts = outer_texts("#[repr(C)]\nstruct A;\nstruct B;");

        assert!(texts.is_empty(), "{texts:?}");
    }

    #[test]
    fn outer_without_attributes_is_empty() {
        let texts = outer_texts("struct S;");

        assert!(texts.is_empty(), "{texts:?}");
    }

    #[test]
    fn path_keeps_every_segment() {
        let tree = parse("#[stabby::stabby]\nstruct S;", Language::Rust);
        let attributes = outer(&last_item(&tree));

        assert_eq!(path(&attributes[0]), Some("stabby::stabby"));
    }

    #[test]
    fn words_of_a_list_hold_each_identifier() {
        let words = first_words("#[repr(C, packed)]\nstruct S;");

        assert_eq!(words, vec!["C", "packed"]);
    }

    #[test]
    fn words_skip_a_nested_token_tree() {
        let words = first_words("#[repr(align(8), C)]\nstruct S;");

        assert_eq!(words, vec!["align", "C"]);
    }

    #[test]
    fn words_skip_a_string_value() {
        let words = first_words("#[serde(rename = \"x\")]\nstruct S;");

        assert_eq!(words, vec!["rename"]);
    }

    #[test]
    fn words_without_arguments_are_empty() {
        let words = first_words("#[stabby]\nstruct S;");

        assert!(words.is_empty(), "{words:?}");
    }
}
