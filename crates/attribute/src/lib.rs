//! The outer attributes of an item, and what their syntax states
//!
//! Tree-sitter leaves an item's attributes beside the item rather than inside
//! it. A rule that exempts an item for an attribute it carries has to walk
//! back over the siblings to find them. [`outer`] does that walk, and the
//! other functions read what the attributes it returns state.

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

/// What a `bon` `#[builder]` on a function leaves of the function
///
/// `bon` generates a builder from a function under `#[builder]`. Without a
/// `start_fn` name it hides the function, and the builder takes its name.
/// With a name, it keeps the function callable under its own name beside
/// the builder.
///
/// # Examples
///
/// ```
/// use attribute::Builder;
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let tree = parse("#[builder]\nfn new(a: u8, b: u8) {}", Language::Rust);
/// let function = tree.root_node().named_child(1).unwrap();
///
/// assert_eq!(attribute::builder(&function), Builder::Hidden);
/// ```
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub enum Builder {
    /// The function carries no `#[builder]`
    Absent,

    /// A `#[builder]` replaces the function with a builder
    Hidden,

    /// A `#[builder]` names a `start_fn`, so the function stays callable
    Kept,
}

/// Returns what a `#[builder]` on `function` leaves of the function
///
/// An attribute counts when the last segment of its path is `builder`, so
/// `#[builder]` and `#[bon::builder]` count alike. `bon` reads the start
/// function's name from `start_fn = name`, or from `name = ...` inside
/// `start_fn(...)`. `start_fn(vis = "...")` without a name leaves the
/// function hidden. Only the attribute's own arguments count, so a
/// `start_fn` inside another argument does not.
///
/// # Examples
///
/// ```
/// use attribute::Builder;
/// use stiles_testing::parse;
/// use stiles_types::Language;
///
/// let tree = parse("#[builder(start_fn = builder)]\nfn new() {}", Language::Rust);
/// let function = tree.root_node().named_child(1).unwrap();
///
/// assert_eq!(attribute::builder(&function), Builder::Kept);
/// ```
pub fn builder(function: &DecoratedNode<'_>) -> Builder {
    let builders: Vec<_> = outer(function)
        .into_iter()
        .filter(|attribute| name(attribute) == Some("builder"))
        .collect();

    if builders.iter().any(names_a_start_fn) {
        return Builder::Kept;
    }
    match builders.is_empty() {
        true => Builder::Absent,
        false => Builder::Hidden,
    }
}

/// Returns whether a `#[builder]` attribute names its start function
fn names_a_start_fn(attribute: &DecoratedNode<'_>) -> bool {
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return false;
    };
    children(&arguments).windows(2).any(|pair| {
        if pair[0].kind() != "identifier" || pair[0].text() != "start_fn" {
            return false;
        }
        match pair[1].kind() {
            "=" => true,
            "token_tree" => children(&pair[1])
                .windows(2)
                .any(|inner| inner[0].text() == "name" && inner[1].kind() == "="),
            _ => false,
        }
    })
}

/// Returns every child of `node` in order, the unnamed ones included
///
/// A token tree holds its `=` signs and commas as unnamed children.
fn children<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    (0..node.child_count() as u32)
        .filter_map(|index| node.child(index))
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

    /// Returns what a `#[builder]` leaves of the last function in `source`
    fn builder_of(source: &str) -> Builder {
        let tree = parse(source, Language::Rust);

        builder(&last_item(&tree))
    }

    #[test]
    fn builder_naming_a_start_fn_in_a_list_is_kept() {
        let left = builder_of("#[builder(start_fn(name = make, vis = \"\"))]\nfn new() {}");

        assert_eq!(left, Builder::Kept);
    }

    #[test]
    fn builder_naming_a_start_fn_is_kept() {
        let left = builder_of("#[builder(finish_fn = done, start_fn = make)]\nfn new() {}");

        assert_eq!(left, Builder::Kept);
    }

    #[test]
    fn builder_on_the_function_before_is_absent() {
        let left = builder_of("#[builder]\nfn a() {}\nfn b() {}");

        assert_eq!(left, Builder::Absent);
    }

    #[test]
    fn builder_past_a_doc_comment_is_hidden() {
        let left = builder_of("#[builder]\n/// Docs\nfn new() {}");

        assert_eq!(left, Builder::Hidden);
    }

    #[test]
    fn builder_with_a_start_fn_inside_another_argument_is_hidden() {
        let left = builder_of("#[builder(on(String, start_fn = make))]\nfn new() {}");

        assert_eq!(left, Builder::Hidden);
    }

    #[test]
    fn builder_with_a_start_fn_visibility_only_is_hidden() {
        let left = builder_of("#[builder(start_fn(vis = \"pub(crate)\"))]\nfn new() {}");

        assert_eq!(left, Builder::Hidden);
    }

    #[test]
    fn builder_without_arguments_is_hidden() {
        let left = builder_of("#[bon::builder]\nfn new() {}");

        assert_eq!(left, Builder::Hidden);
    }

    #[test]
    fn builder_without_the_attribute_is_absent() {
        let left = builder_of("#[inline]\nfn new() {}");

        assert_eq!(left, Builder::Absent);
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
    fn trait_send_builder() {
        fn assert_send<T: Send>() {}
        assert_send::<Builder>();
    }

    #[test]
    fn trait_sync_builder() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Builder>();
    }

    #[test]
    fn trait_unpin_builder() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Builder>();
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
