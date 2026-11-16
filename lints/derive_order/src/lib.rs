use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, Edit, Fix, LintPass, Severity, Span};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(derive_order::RULE_ID.as_str(), "lint.derive-order");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.derive-order");

const STD_DERIVES: &[&str] = &[
    "Copy",
    "Clone",
    "Eq",
    "PartialEq",
    "Ord",
    "PartialOrd",
    "Hash",
    "Debug",
    "Default",
];

/// Enforces canonical ordering of `#[derive(...)]` attributes
///
/// Standard library derives must appear first in the prescribed order
/// (Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default),
/// followed by third-party derives sorted alphabetically.
///
/// `stiles check --fix` writes the derives in that order. Each derive keeps
/// its path as written, such as `serde::Serialize`, and the commas and line
/// breaks between them stay in place. The rule attaches no fix when the list
/// holds a comment, because the comment has no clear place in the new order.
///
/// # Examples
///
/// ```text
/// // DON'T
/// #[derive(Debug, Serialize, Clone, PartialEq)]
/// struct Point { x: i32, y: i32 }
///
/// // DO
/// #[derive(Clone, PartialEq, Debug, Serialize)]
/// struct Point { x: i32, y: i32 }
/// ```
///
/// ```
/// use derive_order::DeriveOrder;
/// use stiles_testing::{execute, parse};
/// use stiles_types::Language;
///
/// let tree = parse("#[derive(Debug, Clone)]\nstruct Point;", Language::Rust);
/// let mut passes = vec![DeriveOrder::into_lint_pass()];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct DeriveOrder;

impl DeriveOrder {
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = DeriveOrder::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

/// Returns the positional index of a standard library derive, if any
fn std_derive_index(name: &str) -> Option<usize> {
    STD_DERIVES.iter().position(|&s| s == name)
}

/// Extracts the macro name from a potentially qualified path
///
/// Given `"serde::Serialize"`, returns `"Serialize"`.
fn macro_name(full_path: &str) -> &str {
    full_path.rsplit("::").next().unwrap_or(full_path)
}

/// One derive of the list, with the offset of its first byte
///
/// The offset counts from the start of the attribute's text.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
struct Entry<'a> {
    offset: usize,
    name: &'a str,
}

/// Returns the derives of `#[derive(...)]` attribute text, as written
///
/// Returns [`None`] if the text is not a derive attribute or the derive
/// list is empty. A trailing comma adds no entry.
///
/// [`None`]: std::option::Option::None
fn derive_entries(text: &str) -> Option<Vec<Entry<'_>>> {
    let inner = text.strip_prefix("#[derive(")?.strip_suffix(")]")?;
    let base = "#[derive(".len();

    let mut entries = Vec::new();
    let mut start = 0;
    for piece in inner.split(',') {
        let name = piece.trim();
        if !name.is_empty() {
            let leading = piece.len() - piece.trim_start().len();
            entries.push(Entry {
                offset: base + start + leading,
                name,
            });
        }
        start += piece.len() + 1;
    }

    match entries.is_empty() {
        true => None,
        false => Some(entries),
    }
}

/// Computes the canonical ordering for a list of derive names
///
/// Standard derives are placed first in their prescribed order,
/// followed by third-party derives sorted case-insensitively.
fn compute_expected_order<'a>(names: &[&'a str]) -> Vec<&'a str> {
    let mut std_derives: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| std_derive_index(macro_name(n)).is_some())
        .collect();
    std_derives.sort_by_key(|n| std_derive_index(macro_name(n)).unwrap_or(usize::MAX));

    let mut third_party: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| std_derive_index(macro_name(n)).is_none())
        .collect();
    third_party.sort_by_key(|a| a.to_lowercase());

    std_derives.extend(third_party);
    std_derives
}

/// Returns the fix that writes each derive in its expected place, or
/// [`None`] when the list holds a comment
///
/// Each edit replaces one derive and leaves the commas, the spaces, and the
/// line breaks between them as they are. A comment has no clear place in
/// the new order, so a list with one gets no fix.
///
/// [`None`]: std::option::Option::None
fn reorder(node: &DecoratedNode<'_>, entries: &[Entry<'_>], expected: &[&str]) -> Option<Fix> {
    let text = node.text();
    if text.contains("//") || text.contains("/*") {
        return None;
    }

    let start = node.span().start();
    let edits: Vec<Edit> = entries
        .iter()
        .zip(expected)
        .filter(|(entry, name)| entry.name != **name)
        .map(
            |(
                &Entry {
                    offset,
                    name: written,
                },
                name,
            )| {
                let span = Span::builder()
                    .file(node.span().file_path().clone())
                    .start(start + offset)
                    .end(start + offset + written.len())
                    .build();
                Edit::builder().span(span).replacement(*name).build()
            },
        )
        .collect();

    Fix::builder()
        .message("sort the derives")
        .edits(edits)
        .build()
        .ok()
}

impl RustLintPass for DeriveOrder {
    fn check_attribute_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(entries) = derive_entries(node.text()) else {
            return Vec::new();
        };
        let names: Vec<&str> = entries.iter().map(|entry| entry.name).collect();
        let expected = compute_expected_order(&names);
        if names == expected {
            return Vec::new();
        }

        let diagnostic = Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Error)
            .message(format!(
                "derive macros are not in canonical order; expected: {}",
                expected.join(", ")
            ))
            .span(node.span())
            .build();
        let fixed = reorder(node, &entries, &expected)
            .and_then(|fix| diagnostic.clone().with_fix(fix).ok());

        vec![fixed.unwrap_or(diagnostic)]
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for DeriveOrder {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::none()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [DeriveOrder]];

#[cfg(test)]
mod tests {
    use stiles_testing::{assert_diagnostic, assert_fix, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, Severity};

    use super::*;

    fn adapt() -> Box<dyn LintPass> {
        DeriveOrder::into_lint_pass()
    }

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![adapt()]
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![adapt()];
        execute(&tree, &mut passes)
    }

    #[test]
    fn block_comment_in_the_list_has_no_fix() {
        let diagnostics = run("#[derive(Debug /* printed */, Clone)]\nstruct Foo;");

        assert_diagnostic(&diagnostics[0]).has_no_fix();
    }

    #[test]
    fn correct_full_std_order_not_flagged() {
        let diagnostics = run(
            "#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]\nstruct Foo;",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn correct_partial_std_order_not_flagged() {
        let diagnostics = run("#[derive(Clone, Debug)]\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn correct_std_then_third_party_not_flagged() {
        let diagnostics = run("#[derive(Clone, Debug, Builder, Getters)]\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    fn run_documented(source: &str) -> Vec<stiles_types::Diagnostic> {
        let tree = stiles_testing::parse(source, stiles_types::Language::Rust);
        stiles_testing::execute(&tree, &mut [DeriveOrder::into_lint_pass()])
    }

    #[test]
    fn correct_third_party_alphabetical_not_flagged() {
        let diagnostics = run("#[derive(Clone, Debug, Alpha, Beta, Gamma)]\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        pair.assert_example(run_documented);
    }
    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_documented(pair.fix());

        stiles_testing::assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_derive_not_flagged() {
        let diagnostics = run("#[derive()]\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn entry_offsets_skip_the_spaces_and_the_trailing_comma() {
        let entries = derive_entries("#[derive( Debug,\n    Clone, )]").expect("a derive list");

        let found: Vec<(usize, &str)> = entries
            .iter()
            .map(|entry| (entry.offset, entry.name))
            .collect();

        assert_eq!(found, [(10, "Debug"), (21, "Clone")]);
    }

    #[test]
    fn fix_keeps_a_multiline_layout_and_its_trailing_comma() {
        assert_fix()
            .tree(parse(
                "#[derive(\n    Debug,\n    Clone,\n    Alpha,\n)]\nstruct Foo;",
                Language::Rust,
            ))
            .passes(passes)
            .produces("#[derive(\n    Clone,\n    Debug,\n    Alpha,\n)]\nstruct Foo;");
    }

    #[test]
    fn fix_keeps_qualified_paths_as_written() {
        assert_fix()
            .tree(parse(
                "#[derive(serde::Serialize, std::fmt::Debug, Clone)]\nstruct Foo;",
                Language::Rust,
            ))
            .passes(passes)
            .produces("#[derive(Clone, std::fmt::Debug, serde::Serialize)]\nstruct Foo;");
    }

    #[test]
    fn fix_keeps_the_spacing_between_derives() {
        assert_fix()
            .tree(parse(
                "#[derive(Debug,Clone,  Default)]\nstruct Foo;",
                Language::Rust,
            ))
            .passes(passes)
            .produces("#[derive(Clone,Debug,  Default)]\nstruct Foo;");
    }

    #[test]
    fn fix_puts_std_derives_before_third_party_derives() {
        assert_fix()
            .tree(parse(
                "#[derive(Builder, Clone, Debug)]\nstruct Foo;",
                Language::Rust,
            ))
            .passes(passes)
            .produces("#[derive(Clone, Debug, Builder)]\nstruct Foo;");
    }

    #[test]
    fn fix_sorts_every_derive_list_of_a_file() {
        assert_fix()
            .tree(parse(
                "#[derive(Debug, Clone)]\nstruct A;\n#[derive(Gamma, Alpha)]\nstruct B;",
                Language::Rust,
            ))
            .passes(passes)
            .produces("#[derive(Clone, Debug)]\nstruct A;\n#[derive(Alpha, Gamma)]\nstruct B;");
    }

    #[test]
    fn fix_sorts_std_derives_in_the_prescribed_order() {
        assert_fix()
            .tree(parse(
                "#[derive(Default, Debug, Hash, PartialOrd, Ord, PartialEq, Eq, Clone, Copy)]\nstruct Foo;",
                Language::Rust,
            ))
            .passes(passes)
            .produces("#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]\nstruct Foo;");
    }

    #[test]
    fn fix_sorts_third_party_derives_without_regard_to_case() {
        assert_fix()
            .tree(parse(
                "#[derive(Clone, getters, Beta, alpha)]\nstruct Foo;",
                Language::Rust,
            ))
            .passes(passes)
            .produces("#[derive(Clone, alpha, Beta, getters)]\nstruct Foo;");
    }

    #[test]
    fn line_comment_in_the_list_has_no_fix() {
        let diagnostics =
            run("#[derive(\n    Debug,\n    Clone, // cloned\n    Copy,\n)]\nstruct Foo;");

        assert_diagnostic(&diagnostics[0]).has_no_fix();
    }

    #[test]
    fn multiline_trailing_comma_in_canonical_order_not_flagged() {
        let diagnostics = run(
            "#[derive(\n    Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, bon::Builder, CopyGetters, Getters,\n)]\nstruct Panic;",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn multiline_trailing_comma_out_of_order_message_has_no_empty_entry() {
        let diagnostics = run("#[derive(\n    Debug, Clone, Alpha,\n)]\nstruct Panic;");

        assert!(!diagnostics[0].message().contains(", ,"));
    }

    #[test]
    fn non_derive_attribute_not_flagged() {
        let diagnostics = run("#[allow(dead_code)]\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn single_derive_not_flagged() {
        let diagnostics = run("#[derive(Debug)]\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn third_party_before_std_flagged_once() {
        let diagnostics = run("#[derive(Builder, Clone, Debug)]\nstruct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn third_party_before_std_flagged_with_rule_id_severity_and_the_expected_order() {
        let diagnostics = run("#[derive(Builder, Clone, Debug)]\nstruct Foo;");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.derive-order")
            .has_severity(Severity::Error)
            .message_contains("Clone, Debug, Builder");
    }

    #[test]
    fn third_party_out_of_alpha_order_flagged_once() {
        let diagnostics = run("#[derive(Clone, Debug, Gamma, Alpha)]\nstruct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn third_party_out_of_alpha_order_flagged_with_rule_id_severity_and_the_expected_order() {
        let diagnostics = run("#[derive(Clone, Debug, Gamma, Alpha)]\nstruct Foo;");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.derive-order")
            .has_severity(Severity::Error)
            .message_contains("Alpha, Gamma");
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DeriveOrder>();
    }

    #[test]
    fn trait_send_entry() {
        fn assert_send<T: Send>() {}
        assert_send::<Entry<'_>>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DeriveOrder>();
    }

    #[test]
    fn trait_sync_entry() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Entry<'_>>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DeriveOrder>();
    }

    #[test]
    fn trait_unpin_entry() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Entry<'_>>();
    }

    #[test]
    fn wrong_std_and_third_party_flagged_once() {
        let diagnostics = run("#[derive(Debug, Clone, Beta, Alpha)]\nstruct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn wrong_std_and_third_party_flagged_with_rule_id_severity_and_the_expected_order() {
        let diagnostics = run("#[derive(Debug, Clone, Beta, Alpha)]\nstruct Foo;");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.derive-order")
            .has_severity(Severity::Error)
            .message_contains("Clone, Debug, Alpha, Beta");
    }

    #[test]
    fn wrong_std_order_flagged_once() {
        let diagnostics = run("#[derive(Debug, Clone, Default)]\nstruct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn wrong_std_order_flagged_with_rule_id_severity_and_the_expected_order() {
        let diagnostics = run("#[derive(Debug, Clone, Default)]\nstruct Foo;");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.derive-order")
            .has_severity(Severity::Error)
            .message_contains("Clone, Debug, Default");
    }

    #[test]
    fn wrong_std_order_multiple_flagged_once() {
        let diagnostics = run("#[derive(Default, Copy, Clone)]\nstruct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn wrong_std_order_multiple_flagged_with_rule_id_severity_and_the_expected_order() {
        let diagnostics = run("#[derive(Default, Copy, Clone)]\nstruct Foo;");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.derive-order")
            .has_severity(Severity::Error)
            .message_contains("Copy, Clone, Default");
    }
}
