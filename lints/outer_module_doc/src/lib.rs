use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity, Span};

const RULE_ID: RuleId = RuleId::new("lint.outer-module-doc");

/// Flags documentation on a `mod` declaration that ends in a semicolon
///
/// `mod foo;` always means the module lives in a file of its own, and that
/// file documents the module with a `//!` header. Rustc attaches a `///` on
/// the declaration to the same module. Rustdoc then prints both texts at the
/// top of the module page, one after the other. The listing in the parent
/// module shows the declaration's text as well. The two texts also drift
/// apart, because nothing makes them agree.
///
/// The rule reports `///` and `/** */` comments and `#[doc = ...]`
/// attributes. Other attributes and plain comments may sit between them and
/// the declaration, because rustc attaches the documentation across them.
/// Private declarations count too, since documentation built with private
/// items doubles them in the same way.
///
/// An inline module, `mod foo { ... }`, has no file to carry a header. A
/// `///` on it is the ordinary way to document it, so the rule leaves it
/// alone. The rule reads syntax alone. It cannot compare the two texts,
/// because whisker never sees the child's file from the parent.
pub struct OuterModuleDoc;

/// What a node above a declaration means for the declaration's
/// documentation
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Preceding {
    /// Documentation that rustc attaches to the declaration
    Doc,
    /// A plain comment or another attribute, which rustc steps over
    Transparent,
    /// Anything else, which ends the attributes of the declaration
    Boundary,
}

/// Returns whether `node` is a doc comment on the item below it
///
/// The grammar records the documentation in the `doc` field and its marker
/// in `outer` or `inner`. A `//!` or `/*! */` documents the item around it,
/// so it is not a comment on the declaration.
fn is_outer_doc_comment(node: &DecoratedNode<'_>) -> bool {
    node.child_by_field_name("doc").is_some() && node.child_by_field_name("outer").is_some()
}

/// Returns whether an `attribute_item` is `#[doc = ...]`
///
/// The attribute needs the plain path `doc` and a value. `#[doc(hidden)]`
/// and `#[doc(alias = "...")]` carry no text, so they are not documentation.
/// `#[cfg_attr(..., doc = "...")]` documents only under a condition, and the
/// rule cannot evaluate it. Tree-sitter keeps a comment wherever it sits, so
/// the path is the first child of the attribute that is not a comment.
fn is_doc_attribute(node: &DecoratedNode<'_>) -> bool {
    let Some(attribute) = node
        .named_children()
        .into_iter()
        .find(|child| child.kind() == "attribute")
    else {
        return false;
    };
    let Some(path) = attribute
        .named_children()
        .into_iter()
        .find(|child| child.kind() != "line_comment" && child.kind() != "block_comment")
    else {
        return false;
    };

    path.kind() == "identifier"
        && path.text() == "doc"
        && attribute.child_by_field_name("value").is_some()
}

/// Returns what `node` means when it sits above a declaration
///
/// A `//!` or a `#![...]` documents or configures the item around the
/// declaration, so both end the search like an item does.
fn classify(node: &DecoratedNode<'_>) -> Preceding {
    match node.kind() {
        "line_comment" | "block_comment" => {
            if is_outer_doc_comment(node) {
                return Preceding::Doc;
            }
            match node.child_by_field_name("inner") {
                Some(_) => Preceding::Boundary,
                None => Preceding::Transparent,
            }
        }
        "attribute_item" => match is_doc_attribute(node) {
            true => Preceding::Doc,
            false => Preceding::Transparent,
        },
        _ => Preceding::Boundary,
    }
}

/// Returns the documentation rustc attaches to `declaration`, nearest last
/// in source order
///
/// Tree-sitter leaves an item's attributes and comments beside it rather
/// than inside it. The walk therefore reads the siblings above the
/// declaration, and it stops at the first one that ends its attributes.
fn attached_docs<'a>(declaration: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let Some(parent) = declaration.parent() else {
        return Vec::new();
    };
    let siblings = parent.named_children();
    let Some(index) = siblings
        .iter()
        .position(|sibling| sibling.id() == declaration.id())
    else {
        return Vec::new();
    };

    let mut docs = Vec::new();
    for sibling in siblings[..index].iter().rev() {
        match classify(sibling) {
            Preceding::Doc => docs.push(*sibling),
            Preceding::Transparent => {}
            Preceding::Boundary => break,
        }
    }

    docs.reverse();
    docs
}

impl RustLintPass for OuterModuleDoc {
    fn check_mod_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        if node.child_by_field_name("body").is_some() {
            return Vec::new();
        }
        let Some(name) = node.child_by_field_name("name") else {
            return Vec::new();
        };

        let docs = attached_docs(node);
        let (Some(first), Some(last)) = (docs.first(), docs.last()) else {
            return Vec::new();
        };

        vec![Diagnostic::new(
            RULE_ID,
            Severity::Warn,
            format!(
                "documentation on `mod {};`: document the module with `//!` in its own file",
                name.text()
            ),
            covering_span(first, last),
        )]
    }
}

/// Returns the span from the start of `first` to the end of `last`
///
/// The node of a line comment ends after its newline, so the span stops
/// before that newline and ends on the comment's own line.
fn covering_span(first: &DecoratedNode<'_>, last: &DecoratedNode<'_>) -> Span {
    let start = first.span();
    let end = last.span().start() + last.text().trim_end_matches(['\n', '\r']).len();

    Span::new(start.file_path().clone(), start.start(), end)
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for OuterModuleDoc {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

whisker_plugin::export_lints![rust: [OuterModuleDoc]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(OuterModuleDoc))]
    }

    fn check(source: &str) -> Vec<Diagnostic> {
        execute(&parse(source, Language::Rust), &mut passes())
    }

    #[test]
    fn block_doc_comment_on_declaration_is_flagged() {
        let source = "/** What the manifests declare */\nmod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn block_doc_comment_on_declaration_is_flagged_over_the_comment() {
        let source = "/** What the manifests declare */\nmod manifests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 0, 33);
    }

    #[test]
    fn cfg_attr_doc_attribute_is_not_flagged() {
        let source = "#[cfg_attr(docsrs, doc = \"Manifests\")]\nmod manifests;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn declaration_in_function_body_is_flagged() {
        let source = "fn f() {\n    /// Manifests\n    mod manifests;\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn declaration_in_inline_module_is_flagged() {
        let source = "mod outer {\n    /// Manifests\n    pub mod manifests;\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn declaration_in_inline_module_is_flagged_with_the_declaration_in_the_message() {
        let source = "mod outer {\n    /// Manifests\n    pub mod manifests;\n}";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).message_contains("`mod manifests;`");
    }

    #[test]
    fn declaration_without_documentation_is_not_flagged() {
        let source = "pub mod manifests;\nmod next_action;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_attribute_on_declaration_is_flagged() {
        let source = "#[doc = \"What the manifests declare\"]\npub mod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_attribute_on_declaration_is_flagged_over_the_attribute() {
        let source = "#[doc = \"What the manifests declare\"]\npub mod manifests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 0, 37);
    }

    #[test]
    fn doc_attribute_with_inner_comment_is_flagged() {
        let source = "#[/* the summary */ doc = \"Manifests\"]\nmod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_attribute_with_macro_value_is_flagged() {
        let source = "#[doc = include_str!(\"manifests.md\")]\nmod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_above_other_attributes_is_flagged() {
        let source = "/// Tests for the manifests\n#[cfg(test)]\n#[path = \"manifests_tests.rs\"]\nmod tests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_above_other_attributes_is_flagged_with_the_declaration_in_the_message() {
        let source = "/// Tests for the manifests\n#[cfg(test)]\n#[path = \"manifests_tests.rs\"]\nmod tests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).message_contains("`mod tests;`");
    }

    #[test]
    fn doc_comment_above_plain_comment_is_flagged() {
        let source = "/// Manifests\n// keeps the parser happy\nmod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_above_plain_comment_is_flagged_over_the_doc_comment_only() {
        let source = "/// Manifests\n// keeps the parser happy\nmod manifests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 0, 13);
    }

    #[test]
    fn doc_comment_after_blank_line_is_flagged() {
        let source = "/// Manifests\n\nmod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_on_declaration_is_flagged() {
        let source = "/// What the manifests of a workspace declare\npub mod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_on_declaration_is_flagged_with_the_full_diagnostic() {
        let source = "/// What the manifests of a workspace declare\npub mod manifests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.outer-module-doc")
            .has_severity(Severity::Warn)
            .message_contains("`mod manifests;`")
            .message_contains("`//!`")
            .has_span("<test>", 0, 45);
    }

    #[test]
    fn doc_comment_on_private_declaration_is_flagged() {
        let source = "/// The next action of the operator\nmod next_action;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_on_restricted_declaration_is_flagged() {
        let source = "/// Manifests\npub(crate) mod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_over_declaration_with_inner_comment_is_flagged() {
        let source = "/// Manifests\npub /* re-exported below */ mod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_over_declaration_with_inner_comment_is_flagged_over_the_doc_comment_only() {
        let source = "/// Manifests\npub /* re-exported below */ mod manifests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 0, 13);
    }

    #[test]
    fn doc_comment_split_by_attribute_is_flagged_across_both() {
        let source = "/// Manifests\n#[cfg(test)]\n#[doc = \"More\"]\n/// Even more\nmod tests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 0, source.len() - 11);
    }

    #[test]
    fn doc_comment_split_by_attribute_is_flagged_once() {
        let source = "/// Manifests\n#[cfg(test)]\n#[doc = \"More\"]\n/// Even more\nmod tests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_trailing_previous_item_is_flagged() {
        let source = "fn f() {} /// Manifests\nmod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_comment_trailing_previous_item_is_flagged_over_the_trailing_comment() {
        let source = "fn f() {} /// Manifests\nmod manifests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 10, 23);
    }

    #[test]
    fn doc_comment_with_crlf_ends_before_line_break() {
        let source = "/// Manifests\r\nmod manifests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 0, 13);
    }

    #[test]
    fn doc_comment_with_crlf_is_flagged() {
        let source = "/// Manifests\r\nmod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_hidden_attribute_is_not_flagged() {
        let source = "#[doc(hidden)]\npub mod manifests;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_on_earlier_item_is_not_flagged() {
        let source = "/// Adds two numbers\nfn add() {}\nmod manifests;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_on_inline_module_is_not_flagged() {
        let source = "/// Tests for the manifests\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_on_inline_module_is_not_flagged_on_declaration_inside() {
        let source = "/// Manifests\nmod outer {\n    mod manifests;\n}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_on_previous_declaration_is_flagged_once() {
        let source = "/// Manifests\nmod manifests;\nmod next_action;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_on_previous_declaration_is_flagged_with_its_name() {
        let source = "/// Manifests\nmod manifests;\nmod next_action;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).message_contains("`mod manifests;`");
    }

    #[test]
    fn empty_block_comment_is_not_flagged() {
        let source = "/**/\nmod manifests;\n/***/\nmod next_action;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_doc_comment_is_flagged() {
        let source = "///\nmod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inner_attribute_above_declaration_is_not_flagged() {
        let source = "#![doc = \"The crate\"]\nmod manifests;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_comment_above_declaration_is_not_flagged() {
        let source = "//! The crate\n\nmod manifests;\n/*! More of the crate */\nmod next_action;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn multi_line_doc_comment_is_flagged_once() {
        let source = "/// What the manifests\n/// of a workspace declare\npub mod manifests;";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn multi_line_doc_comment_is_flagged_over_all_lines() {
        let source = "/// What the manifests\n/// of a workspace declare\npub mod manifests;";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 0, 49);
    }

    #[test]
    fn plain_block_comment_is_not_flagged() {
        let source = "/* What the manifests declare */\nmod manifests;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn plain_comment_is_not_flagged() {
        let source = "// What the manifests declare\npub mod manifests;\n//// Not a doc comment\nmod next_action;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn scoped_doc_attribute_is_not_flagged() {
        let source = "#[my::doc = \"Manifests\"]\nmod manifests;";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<OuterModuleDoc>();
    }

    #[test]
    fn trait_send_preceding() {
        fn assert_send<T: Send>() {}
        assert_send::<Preceding>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<OuterModuleDoc>();
    }

    #[test]
    fn trait_sync_preceding() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Preceding>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<OuterModuleDoc>();
    }

    #[test]
    fn trait_unpin_preceding() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Preceding>();
    }
}
