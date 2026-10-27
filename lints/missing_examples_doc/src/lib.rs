use whisker_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use whisker_types::{DecoratedNode, Diagnostic, LintPass, Severity};

const RULE_ID: RuleId = RuleId::new("lint.missing-examples-doc");

/// Ancestors that end the search for a public path to an item
///
/// An item inside a `block` is local to that block, so it never leaves the
/// function. A `trait_item` documents its own members, so the trait
/// carries the example for all of them.
const UNCHECKED_SCOPES: &[&str] = &["block", "trait_item"];

/// Flags a public item whose doc comment has no `# Examples` section
///
/// Clippy checks `# Errors`, `# Panics`, and `# Safety`, but has no
/// equivalent check for examples.
///
/// The rule reads only the item's own doc comment. It stays silent on an
/// item with no docs at all, and on one documented through `#[doc = ...]`,
/// whose text it cannot see. It also skips `#[doc(hidden)]` items, items
/// under a module that is not `pub`, and trait members.
///
/// A `pub` method on a private type is a false positive. Tree-sitter alone
/// cannot tell whether the type an `impl` block names leaves the crate.
pub struct MissingExamplesDoc;

impl MissingExamplesDoc {
    /// Creates a boxed [`LintPass`] suitable for the whisker pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = MissingExamplesDoc::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

impl RustLintPass for MissingExamplesDoc {
    fn check_const_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_item(node, ItemKind::Constant)
    }

    fn check_enum_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_item(node, ItemKind::Enum)
    }

    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_item(node, ItemKind::Function)
    }

    fn check_static_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_item(node, ItemKind::Static)
    }

    fn check_struct_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_item(node, ItemKind::Struct)
    }

    fn check_trait_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_item(node, ItemKind::Trait)
    }

    fn check_type_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_item(node, ItemKind::TypeAlias)
    }

    fn check_union_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_item(node, ItemKind::Union)
    }
}

/// The kinds of item the rule checks
///
/// The variant supplies the word the diagnostic uses, so the message names
/// the item the reader sees rather than a tree-sitter node kind.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum ItemKind {
    Constant,
    Enum,
    Function,
    Static,
    Struct,
    Trait,
    TypeAlias,
    Union,
}

impl ItemKind {
    /// Returns the word the diagnostic uses for this kind
    fn as_str(self) -> &'static str {
        match self {
            ItemKind::Constant => "constant",
            ItemKind::Enum => "enum",
            ItemKind::Function => "function",
            ItemKind::Static => "static",
            ItemKind::Struct => "struct",
            ItemKind::Trait => "trait",
            ItemKind::TypeAlias => "type alias",
            ItemKind::Union => "union",
        }
    }
}

/// Reports a documented public item that has no example
///
/// The diagnostic points at the item's name, not at its whole body, so a
/// long type does not bury the message.
fn check_item(node: &DecoratedNode<'_>, kind: ItemKind) -> Vec<Diagnostic> {
    if !is_public(node) {
        return Vec::new();
    }

    if !in_public_scope(node) {
        return Vec::new();
    }

    let Some(lines) = doc_lines(node) else {
        return Vec::new();
    };

    if has_examples_heading(&lines) {
        return Vec::new();
    }

    let Some(name) = node.child_by_field_name("name") else {
        return Vec::new();
    };

    vec![Diagnostic::new(
        RULE_ID,
        Severity::Warn,
        format!(
            "public {} `{}` has no `# Examples` section",
            kind.as_str(),
            name.text()
        ),
        name.span(),
    )]
}

/// Returns the outer doc comment lines attached to an item, in source order
///
/// Returns [`None`] when the rule must stay silent. The item has no doc
/// comment, or an attribute puts the docs out of reach.
fn doc_lines<'a>(node: &DecoratedNode<'a>) -> Option<Vec<&'a str>> {
    let parent = node.parent()?;
    let index = child_index(&parent, node)?;

    let mut lines = Vec::new();
    for i in (0..index).rev() {
        let Some(sibling) = parent.child(i) else {
            break;
        };

        if sibling.kind() == "attribute_item" {
            if silences_rule(&sibling) {
                return None;
            }
            continue;
        }

        if sibling.kind() != "line_comment" && sibling.kind() != "block_comment" {
            break;
        }

        if sibling.child_by_field_name("inner").is_some() {
            break;
        }

        let Some(doc) = sibling.child_by_field_name("doc") else {
            continue;
        };

        lines.extend(doc.text().lines().rev());
    }

    if lines.is_empty() {
        return None;
    }

    lines.reverse();
    Some(lines)
}

/// Returns the index of a node among its parent's children
///
/// The index counts anonymous children too. The backward walk from it
/// therefore sees every token between two items.
fn child_index(parent: &DecoratedNode<'_>, node: &DecoratedNode<'_>) -> Option<u32> {
    let count = u32::try_from(parent.child_count()).ok()?;
    (0..count).find(|&i| parent.child(i).is_some_and(|child| child.id() == node.id()))
}

/// Returns whether an attribute puts the item's docs out of the rule's reach
///
/// `#[doc(hidden)]` keeps the item out of the published documentation.
/// `#[doc = ...]` supplies the text from elsewhere, such as `include_str!`,
/// and the rule cannot read it.
fn silences_rule(attribute: &DecoratedNode<'_>) -> bool {
    let text: String = attribute
        .text()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    let Some(rest) = text.strip_prefix("#[doc") else {
        return false;
    };

    rest.starts_with('=') || rest.starts_with("(hidden)")
}

/// Returns whether any doc line opens an `Examples` section
///
/// A heading inside a fenced code block does not count, because there `#`
/// marks a doctest line that rustdoc hides.
fn has_examples_heading(lines: &[&str]) -> bool {
    let mut fence: Option<CodeFence> = None;

    for line in lines {
        let line = strip_decoration(line);

        if let Some(open) = fence {
            if open.closes(line) {
                fence = None;
            }
            continue;
        }

        if let Some(open) = CodeFence::opening(line) {
            fence = Some(open);
            continue;
        }

        if is_examples_heading(line) {
            return true;
        }
    }

    false
}

/// The character a fenced code block repeats to delimit itself
///
/// A fence closes only on the character it opened with, so a `~~~` line
/// inside a backtick block is part of the example rather than its end.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum FenceMarker {
    Backtick,
    Tilde,
}

impl FenceMarker {
    /// Every marker a fence can be built from
    const ALL: [FenceMarker; 2] = [FenceMarker::Backtick, FenceMarker::Tilde];

    /// Returns the character a fence of this marker repeats
    fn as_char(self) -> char {
        match self {
            FenceMarker::Backtick => '`',
            FenceMarker::Tilde => '~',
        }
    }

    /// Splits a line into the length of its leading run of this marker and
    /// the text that follows the run
    fn split_run(self, line: &str) -> (usize, &str) {
        let rest = line.trim_start_matches(self.as_char());

        (line.len() - rest.len(), rest)
    }
}

/// A fenced code block the scan has entered but not yet left
///
/// A fence closes only on a run of at least as many of its own marker, and
/// a closing run carries no language name. A doc comment can therefore open
/// with four backticks to show a three-backtick block inside, and the inner
/// lines stay code. Toggling on every fence-like line would end such a
/// block early and read its hidden doctest `#` lines as headings.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct CodeFence {
    marker: FenceMarker,
    length: usize,
}

impl CodeFence {
    /// The shortest run CommonMark accepts as a fence
    const MIN_LENGTH: usize = 3;

    /// Returns the fence a doc line opens, or [`None`] if it opens none
    fn opening(line: &str) -> Option<Self> {
        FenceMarker::ALL.into_iter().find_map(|marker| {
            let (length, _) = marker.split_run(line);

            (length >= Self::MIN_LENGTH).then_some(Self { marker, length })
        })
    }

    /// Returns whether a doc line closes this fence
    fn closes(self, line: &str) -> bool {
        let (length, rest) = self.marker.split_run(line);

        length >= self.length && rest.trim().is_empty()
    }
}

/// Removes the surrounding whitespace and the leading `*` of a block doc
/// comment
fn strip_decoration(line: &str) -> &str {
    let line = line.trim();
    match line.strip_prefix('*') {
        Some(rest) => rest.trim(),
        None => line,
    }
}

/// Returns whether a doc line is a Markdown heading that reads `Examples`
fn is_examples_heading(line: &str) -> bool {
    let rest = line.trim_start_matches('#');
    let level = line.len() - rest.len();

    if level == 0 || level > 6 {
        return false;
    }

    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return false;
    }

    rest.trim() == "Examples"
}

/// Returns whether an item carries an unrestricted `pub`
///
/// A `visibility_modifier` with a named child is a restricted form such as
/// `pub(crate)` or `pub(super)`, and no restricted form leaves the crate.
fn is_public(node: &DecoratedNode<'_>) -> bool {
    let Ok(count) = u32::try_from(node.child_count()) else {
        return false;
    };

    for i in 0..count {
        let Some(child) = node.child(i) else { continue };
        if child.kind() == "visibility_modifier" {
            return child.named_child_count() == 0;
        }
    }

    false
}

/// Returns whether an item's enclosing scopes can expose it outside the
/// crate
///
/// The walk stops at the first module that is not `pub`. A `pub use` in
/// another file can still re-export such an item. One file of source does
/// not show that, so the rule leaves the item alone.
fn in_public_scope(node: &DecoratedNode<'_>) -> bool {
    let mut current = node.parent();

    while let Some(ancestor) = current {
        if UNCHECKED_SCOPES.contains(&ancestor.kind()) {
            return false;
        }

        if ancestor.kind() == "mod_item" && !is_public(&ancestor) {
            return false;
        }

        current = ancestor.parent();
    }

    true
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for MissingExamplesDoc {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.missing-examples-doc")]
    }
}

whisker_plugin::export_lints![rust: [MissingExamplesDoc]];

#[cfg(test)]
mod tests {
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> = vec![MissingExamplesDoc::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    #[test]
    fn attribute_between_docs_and_item_is_flagged() {
        let diagnostics = run("/// Docs\n#[derive(Debug)]\npub struct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn block_doc_comment_with_examples_is_not_flagged() {
        let diagnostics = run("/**\n * Docs\n *\n * # Examples\n */\npub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constant_without_examples_is_flagged_once() {
        let diagnostics = run("/// Docs\npub const LIMIT: usize = 1;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn constant_without_examples_is_flagged_with_the_kind_and_name() {
        let diagnostics = run("/// Docs\npub const LIMIT: usize = 1;");

        assert_diagnostic(&diagnostics[0]).message_contains("public constant `LIMIT`");
    }

    #[test]
    fn crate_visible_item_is_not_flagged() {
        let diagnostics = run("/// Docs\npub(crate) struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_alias_attribute_does_not_silence_the_rule() {
        let diagnostics = run("/// Docs\n#[doc(alias = \"bar\")]\npub struct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_attribute_supplying_text_is_not_flagged() {
        let diagnostics = run("/// Docs\n#[doc = include_str!(\"readme.md\")]\npub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_hidden_item_is_not_flagged() {
        let diagnostics = run("/// Docs\n#[doc(hidden)]\npub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn enum_without_examples_is_flagged_once() {
        let diagnostics = run("/// Docs\npub enum Color {\n    Red,\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn enum_without_examples_is_flagged_with_the_kind_and_name() {
        let diagnostics = run("/// Docs\npub enum Color {\n    Red,\n}");

        assert_diagnostic(&diagnostics[0]).message_contains("public enum `Color`");
    }

    #[test]
    fn examples_heading_after_a_closed_longer_fence_is_not_flagged() {
        let source = "/// Docs\n///\n/// ````\n/// ```\n/// ````\n///\n/// # Examples\n///\n/// ```\n/// ```\npub struct Foo;";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn examples_heading_after_a_tilde_line_in_a_backtick_block_is_flagged() {
        let source = "/// Docs\n///\n/// ```\n/// ~~~\n/// # Examples\n/// ```\npub struct Foo;";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn examples_heading_after_an_info_string_fence_is_flagged() {
        let source =
            "/// Docs\n///\n/// ```rust\n/// ```text\n/// # Examples\n/// ```\npub struct Foo;";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn examples_heading_at_level_two_is_not_flagged() {
        let diagnostics =
            run("/// Docs\n///\n/// ## Examples\n///\n/// ```\n/// ```\npub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn examples_heading_inside_a_longer_fence_is_flagged() {
        let source =
            "/// Docs\n///\n/// ````\n/// ```\n/// # Examples\n/// ```\n/// ````\npub struct Foo;";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn examples_heading_inside_code_block_is_flagged() {
        let diagnostics = run("/// Docs\n///\n/// ```\n/// # Examples\n/// ```\npub struct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn examples_heading_without_space_is_flagged() {
        let diagnostics = run("/// Docs\n///\n/// #Examples\npub struct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn function_in_private_module_is_not_flagged() {
        let diagnostics = run("mod inner {\n    /// Docs\n    pub fn f() {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn function_in_public_module_without_examples_is_flagged() {
        let diagnostics = run("pub mod inner {\n    /// Docs\n    pub fn f() {}\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn function_inside_a_function_body_is_not_flagged() {
        let diagnostics = run("pub fn outer() {\n    /// Docs\n    pub fn inner() {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inherent_impl_public_method_without_examples_is_flagged_once() {
        let diagnostics = run("impl Foo {\n    /// Docs\n    pub fn f(&self) {}\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inherent_impl_public_method_without_examples_is_flagged_with_the_kind_and_name() {
        let diagnostics = run("impl Foo {\n    /// Docs\n    pub fn f(&self) {}\n}");

        assert_diagnostic(&diagnostics[0]).message_contains("public function `f`");
    }

    #[test]
    fn item_kind_words_are_distinct() {
        let kinds = [
            ItemKind::Constant,
            ItemKind::Enum,
            ItemKind::Function,
            ItemKind::Static,
            ItemKind::Struct,
            ItemKind::Trait,
            ItemKind::TypeAlias,
            ItemKind::Union,
        ];

        let mut words: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
        words.sort_unstable();
        words.dedup();

        assert_eq!(words.len(), kinds.len());
    }

    #[test]
    fn module_doc_comment_is_not_read_as_item_docs() {
        let diagnostics = run("//! Module docs\n\npub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn plain_comment_between_docs_and_item_is_not_flagged() {
        let diagnostics =
            run("/// Docs\n///\n/// # Examples\n///\n/// ```\n/// ```\n// note\npub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn private_item_is_not_flagged() {
        let diagnostics = run("/// Docs\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn singular_example_heading_is_flagged() {
        let diagnostics = run("/// Docs\n///\n/// # Example\npub struct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn static_without_examples_is_flagged_once() {
        let diagnostics = run("/// Docs\npub static NAME: &str = \"x\";");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn static_without_examples_is_flagged_with_the_kind_and_name() {
        let diagnostics = run("/// Docs\npub static NAME: &str = \"x\";");

        assert_diagnostic(&diagnostics[0]).message_contains("public static `NAME`");
    }

    #[test]
    fn struct_with_examples_is_not_flagged() {
        let diagnostics =
            run("/// Docs\n///\n/// # Examples\n///\n/// ```\n/// ```\npub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_without_docs_is_not_flagged() {
        let diagnostics = run("pub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_without_examples_is_flagged_once() {
        let source = "/// Reads a file\n///\n/// # Errors\n///\n/// Fails.\npub struct Foo;";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn struct_without_examples_is_flagged_with_rule_id_severity_message_and_span() {
        let source = "/// Reads a file\n///\n/// # Errors\n///\n/// Fails.\npub struct Foo;";

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.missing-examples-doc")
            .has_severity(Severity::Warn)
            .message_contains("public struct `Foo` has no `# Examples` section")
            .has_span("<test>", source.len() - 4, source.len() - 1);
    }

    #[test]
    fn trait_impl_method_is_not_flagged() {
        let diagnostics = run("impl Display for Foo {\n    /// Docs\n    fn fmt(&self) {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_member_is_not_flagged() {
        let diagnostics = run(
            "/// Docs\n///\n/// # Examples\n///\n/// ```\n/// ```\npub trait Read {\n    /// Docs\n    fn read(&self);\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<MissingExamplesDoc>();
    }

    #[test]
    fn trait_send_code_fence() {
        fn assert_send<T: Send>() {}
        assert_send::<CodeFence>();
    }

    #[test]
    fn trait_send_fence_marker() {
        fn assert_send<T: Send>() {}
        assert_send::<FenceMarker>();
    }

    #[test]
    fn trait_send_item_kind() {
        fn assert_send<T: Send>() {}
        assert_send::<ItemKind>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<MissingExamplesDoc>();
    }

    #[test]
    fn trait_sync_code_fence() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<CodeFence>();
    }

    #[test]
    fn trait_sync_fence_marker() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FenceMarker>();
    }

    #[test]
    fn trait_sync_item_kind() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ItemKind>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<MissingExamplesDoc>();
    }

    #[test]
    fn trait_unpin_code_fence() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<CodeFence>();
    }

    #[test]
    fn trait_unpin_fence_marker() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<FenceMarker>();
    }

    #[test]
    fn trait_unpin_item_kind() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ItemKind>();
    }

    #[test]
    fn trait_without_examples_is_flagged_once() {
        let diagnostics = run("/// Docs\npub trait Read {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_without_examples_is_flagged_with_the_kind_and_name() {
        let diagnostics = run("/// Docs\npub trait Read {}");

        assert_diagnostic(&diagnostics[0]).message_contains("public trait `Read`");
    }

    #[test]
    fn type_alias_without_examples_is_flagged_once() {
        let diagnostics = run("/// Docs\npub type Pair = (u8, u8);");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn type_alias_without_examples_is_flagged_with_the_kind_and_name() {
        let diagnostics = run("/// Docs\npub type Pair = (u8, u8);");

        assert_diagnostic(&diagnostics[0]).message_contains("public type alias `Pair`");
    }

    #[test]
    fn union_without_examples_is_flagged_once() {
        let diagnostics = run("/// Docs\npub union Bits {\n    a: u8,\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn union_without_examples_is_flagged_with_the_kind_and_name() {
        let diagnostics = run("/// Docs\npub union Bits {\n    a: u8,\n}");

        assert_diagnostic(&diagnostics[0]).message_contains("public union `Bits`");
    }
}
