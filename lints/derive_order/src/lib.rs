use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, Severity};

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

/// Extracts derive names from `#[derive(...)]` attribute text
///
/// Returns [`None`] if the text is not a derive attribute or the
/// derive list is empty.
fn extract_derive_names(text: &str) -> Option<Vec<String>> {
    let text = text.trim();
    let inner = text.strip_prefix("#[derive(")?;
    let inner = inner.strip_suffix(")]")?;
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }

    let names: Vec<String> = inner.split(',').map(|s| s.trim().to_string()).collect();

    if names.is_empty() {
        return None;
    }

    Some(names)
}

/// Computes the canonical ordering for a list of derive names
///
/// Standard derives are placed first in their prescribed order,
/// followed by third-party derives sorted case-insensitively.
fn compute_expected_order(names: &[String]) -> Vec<String> {
    let mut std_derives: Vec<&String> = names
        .iter()
        .filter(|n| std_derive_index(macro_name(n)).is_some())
        .collect();
    std_derives.sort_by_key(|n| std_derive_index(macro_name(n)).unwrap_or(usize::MAX));

    let mut third_party: Vec<&String> = names
        .iter()
        .filter(|n| std_derive_index(macro_name(n)).is_none())
        .collect();
    third_party.sort_by_key(|a| a.to_lowercase());

    let mut result: Vec<String> = std_derives.into_iter().cloned().collect();
    result.extend(third_party.into_iter().cloned());
    result
}

impl RustLintPass for DeriveOrder {
    fn check_attribute_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let text = node.text();

        let Some(names) = extract_derive_names(text) else {
            return Vec::new();
        };

        if names.len() <= 1 {
            return Vec::new();
        }

        let expected = compute_expected_order(&names);

        if names == expected {
            return Vec::new();
        }

        let expected_str = expected.join(", ");
        vec![
            Diagnostic::builder()
                .rule_id(RuleId::new("lint.derive-order"))
                .severity(Severity::Warn)
                .message(format!(
                    "derive macros are not in canonical order; expected: {expected_str}"
                ))
                .span(node.span())
                .build(),
        ]
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for DeriveOrder {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.derive-order")]
    }
}

stiles_plugin::export_lints![rust: [DeriveOrder]];

#[cfg(test)]
mod tests {
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, Severity};

    use super::*;

    fn adapt() -> Box<dyn LintPass> {
        DeriveOrder::into_lint_pass()
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![adapt()];
        execute(&tree, &mut passes)
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

    #[test]
    fn correct_third_party_alphabetical_not_flagged() {
        let diagnostics = run("#[derive(Clone, Debug, Alpha, Beta, Gamma)]\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_derive_not_flagged() {
        let diagnostics = run("#[derive()]\nstruct Foo;");

        assert_no_diagnostics(&diagnostics);
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
            .has_severity(Severity::Warn)
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
            .has_severity(Severity::Warn)
            .message_contains("Alpha, Gamma");
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DeriveOrder>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DeriveOrder>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DeriveOrder>();
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
            .has_severity(Severity::Warn)
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
            .has_severity(Severity::Warn)
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
            .has_severity(Severity::Warn)
            .message_contains("Copy, Clone, Default");
    }
}
