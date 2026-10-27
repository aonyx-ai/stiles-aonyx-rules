use whisker_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use whisker_types::{DecoratedNode, Diagnostic, LintPass, Severity};

const RULE_ID: RuleId = RuleId::new("lint.error-enum-shape");

/// The suffix every error enum's name ends in
const ERROR_SUFFIX: &str = "Error";

/// The name of the field that holds a variant's cause
const SOURCE_FIELD: &str = "source";

/// The words that open the message of a variant with a cause
const FAILED_TO: &str = "failed to";

/// Flags error enums that do not follow the typed-error shape
///
/// A typed error is one enum per fallible action. Its name ends in `Error`,
/// and each variant has named fields. A variant that wraps a cause holds it
/// in a field named `source`. Its message reads "failed to ..." and names
/// the action that failed. A variant without a cause is the diagnosis
/// itself, so its message states the condition instead.
///
/// The rule reads only enums whose derive list names `Error`, bare or as a
/// path such as `thiserror::Error`. A struct that derives `Error` is out of
/// scope, because the shape is about variants. A type that implements
/// `Error` by hand is not a derive, and the rule does not see it. A derive
/// inside `cfg_attr` is not read either.
///
/// A field is the cause when it is named `source` or carries `#[from]` or
/// `#[source]`. `thiserror` reads a field the same way. A unit variant has
/// no cause and no fields to name, so its shape is fine.
///
/// The message checks read the string literal of `#[error("...")]`, and
/// the format arguments inside it are fine. `#[error(transparent)]` takes
/// its message from the cause, so the message checks skip it. The tuple
/// variant check and the `source` name check still apply to it, because
/// `transparent` accepts a named field as well.
///
/// One id covers every finding, because each one is a part of one
/// convention on one enum. A project that cannot follow the convention for
/// an enum opts out of all of it at once.
///
/// # Examples
///
/// An enum in the shape the rule asks for:
///
/// ```ignore
/// #[derive(Debug, Error)]
/// pub enum LoadPluginError {
///     #[error("failed to open {path}")]
///     UnreadableLibrary { path: PathBuf, source: libloading::Error },
///     #[error("the plugin was built by {plugin}, this whisker by {host}")]
///     RustcMismatch { host: String, plugin: String },
/// }
/// ```
pub struct ErrorEnumShape;

impl ErrorEnumShape {
    /// Creates a boxed [`LintPass`] suitable for the whisker pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = ErrorEnumShape::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

/// Returns the last segment of a path, so `thiserror::Error` gives `Error`
fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path).trim()
}

/// Returns the attributes written on the item at `node`, in source order
///
/// An outer attribute is a sibling that precedes the item. The walk goes
/// backwards and stops at the first sibling that is neither an attribute
/// nor a comment. A doc comment between two attributes therefore does not
/// end the run, and an attribute on the item before this one does not
/// reach it.
fn outer_attributes<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    let siblings = parent.named_children();
    let Some(position) = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())
    else {
        return Vec::new();
    };

    let mut attributes = Vec::new();
    for sibling in siblings[..position].iter().rev() {
        match sibling.kind() {
            "attribute_item" => attributes.extend(sibling.named_child(0)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }

    attributes.reverse();
    attributes
}

/// Returns whether an attribute's path ends in `name`
fn is_named(attribute: &DecoratedNode<'_>, name: &str) -> bool {
    attribute
        .named_child(0)
        .is_some_and(|path| last_segment(path.text()) == name)
}

/// Returns whether an attribute is `#[from]` or `#[source]`
///
/// Either one marks the field it sits on as the variant's cause.
fn marks_a_cause(attribute: &DecoratedNode<'_>) -> bool {
    is_named(attribute, "from") || is_named(attribute, "source")
}

/// Returns whether a `#[derive(...)]` attribute names `Error`
fn derives_error(attribute: &DecoratedNode<'_>) -> bool {
    if !is_named(attribute, "derive") {
        return false;
    }
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return false;
    };

    let list = arguments.text();
    let list = list.strip_prefix('(').unwrap_or(list);
    let list = list.strip_suffix(')').unwrap_or(list);
    list.split(',')
        .any(|name| last_segment(name) == ERROR_SUFFIX)
}

/// Returns the string literal of the `#[error("...")]` among `attributes`
///
/// Returns [`None`] when no such attribute exists, or when its first
/// argument is not a string literal. `#[error(transparent)]` is one such
/// case. The rule then has no message to read and stays silent about it.
fn message_literal<'a>(attributes: &[DecoratedNode<'a>]) -> Option<DecoratedNode<'a>> {
    let attribute = attributes
        .iter()
        .find(|attribute| is_named(attribute, "error"))?;
    let arguments = attribute.child_by_field_name("arguments")?;
    let first = arguments.named_child(0)?;

    match first.kind() {
        "string_literal" => Some(first),
        "raw_string_literal" => Some(first),
        _ => None,
    }
}

/// Returns whether the text of a string literal starts with "failed to"
///
/// The scan first removes the opening quote, and for a raw string its `r`
/// and hashes too. The words must end the message or be followed by a
/// character that cannot continue a word, so "failed tomorrow" does not
/// count.
fn starts_with_failed_to(literal: &str) -> bool {
    let content = literal.strip_prefix('r').unwrap_or(literal);
    let content = content.trim_start_matches('#');
    let Some(content) = content.strip_prefix('"') else {
        return false;
    };
    let Some(rest) = content.strip_prefix(FAILED_TO) else {
        return false;
    };

    match rest.chars().next() {
        None => true,
        Some(character) => !character.is_alphanumeric() && character != '_',
    }
}

/// Returns the diagnostic for a cause in a field not named `source`
///
/// A field named `source` is the cause by its name alone. Any other name
/// makes it a cause only through `#[from]` or `#[source]`, and that field
/// is the one the rule reports.
fn misnamed_cause(field: &DecoratedNode<'_>, variant: &str) -> Option<Diagnostic> {
    let name = field.child_by_field_name("name")?;
    if name.text() == SOURCE_FIELD || !outer_attributes(field).iter().any(marks_a_cause) {
        return None;
    }

    Some(Diagnostic::new(
        RULE_ID,
        Severity::Warn,
        format!(
            "field `{}` holds the cause of `{variant}`; name it `{SOURCE_FIELD}`",
            name.text()
        ),
        name.span(),
    ))
}

/// Returns whether a variant body holds a cause
///
/// A field named `source` is the cause, and so is any field that carries
/// `#[from]` or `#[source]`. Those attributes sit beside the fields in both
/// kinds of body, so one scan reads them in either.
fn holds_a_cause(body: &DecoratedNode<'_>) -> bool {
    body.named_children()
        .iter()
        .any(|child| match child.kind() {
            "field_declaration" => child
                .child_by_field_name("name")
                .is_some_and(|name| name.text() == SOURCE_FIELD),
            "attribute_item" => child
                .named_child(0)
                .is_some_and(|attribute| marks_a_cause(&attribute)),
            _ => false,
        })
}

/// Returns the diagnostics for the fields of a variant body
///
/// A tuple variant's fields have no names. The rule reports the variant
/// once, rather than each `#[from]` field in it, because naming its fields
/// fixes both.
fn body_diagnostics(body: &DecoratedNode<'_>, name: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    match body.kind() {
        "field_declaration_list" => body
            .named_children()
            .iter()
            .filter(|field| field.kind() == "field_declaration")
            .filter_map(|field| misnamed_cause(field, name.text()))
            .collect(),
        "ordered_field_declaration_list" => {
            let advice = if holds_a_cause(body) {
                format!("give it named fields, with the cause in `{SOURCE_FIELD}`")
            } else {
                "give it named fields".to_string()
            };
            vec![Diagnostic::new(
                RULE_ID,
                Severity::Warn,
                format!(
                    "error variant `{}` is a tuple variant; {advice}",
                    name.text()
                ),
                name.span(),
            )]
        }
        _ => Vec::new(),
    }
}

/// Returns the diagnostic for a message that does not fit the variant
///
/// A variant with a cause reads "failed to ...", and one without states
/// the condition. A variant with no message literal gets no diagnostic.
fn message_diagnostic(variant: &DecoratedNode<'_>, name: &DecoratedNode<'_>) -> Option<Diagnostic> {
    let literal = message_literal(&outer_attributes(variant))?;
    let has_cause = variant
        .child_by_field_name("body")
        .is_some_and(|body| holds_a_cause(&body));
    let failed_to = starts_with_failed_to(literal.text());

    let message = match (has_cause, failed_to) {
        (true, false) => format!(
            "message of `{}` does not start with \"{FAILED_TO}\"; a variant with a cause names the action that failed",
            name.text()
        ),
        (false, true) => format!(
            "message of `{}` starts with \"{FAILED_TO}\" but the variant has no cause; state the condition instead",
            name.text()
        ),
        (true, true) | (false, false) => return None,
    };

    Some(Diagnostic::new(
        RULE_ID,
        Severity::Warn,
        message,
        literal.span(),
    ))
}

/// Returns the diagnostics for one variant of an error enum
fn variant_diagnostics(variant: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let Some(name) = variant.child_by_field_name("name") else {
        return Vec::new();
    };
    let mut diagnostics = variant
        .child_by_field_name("body")
        .map(|body| body_diagnostics(&body, &name))
        .unwrap_or_default();
    diagnostics.extend(message_diagnostic(variant, &name));
    diagnostics
}

impl RustLintPass for ErrorEnumShape {
    fn check_enum_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        if !outer_attributes(node).iter().any(derives_error) {
            return Vec::new();
        }
        let mut diagnostics = Vec::new();

        if let Some(name) = node.child_by_field_name("name")
            && !name.text().ends_with(ERROR_SUFFIX)
        {
            diagnostics.push(Diagnostic::new(
                RULE_ID,
                Severity::Warn,
                format!(
                    "error enum `{}` does not end in `{ERROR_SUFFIX}`; name it after the action that fails and its object",
                    name.text()
                ),
                name.span(),
            ));
        }

        let Some(body) = node.child_by_field_name("body") else {
            return diagnostics;
        };
        for variant in body.named_children() {
            if variant.kind() != "enum_variant" {
                continue;
            }
            diagnostics.extend(variant_diagnostics(&variant));
        }

        diagnostics
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for ErrorEnumShape {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

whisker_plugin::export_lints![rust: [ErrorEnumShape]];

#[cfg(test)]
mod tests {
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn adapt() -> Box<dyn LintPass> {
        ErrorEnumShape::into_lint_pass()
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![adapt()];
        execute(&tree, &mut passes)
    }

    fn messages(diagnostics: &[Diagnostic]) -> Vec<String> {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message().to_string())
            .collect()
    }

    #[test]
    fn attribute_on_previous_field_does_not_reach_field() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"the {path} is bad\")]\n    A { #[serde(skip)] other: u32, path: u32 },\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn attribute_on_previous_item_does_not_reach_enum() {
        let diagnostics = run("#[derive(Error)]\nstruct Unrelated;\nenum Plugin { Open(u32) }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn attribute_on_previous_variant_does_not_reach_variant() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to {path}\")]\n    A { path: u32, source: E },\n    B,\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cause_field_diagnostic_reported_once() {
        let source = "#[derive(Error)]\nenum LoadError { A { #[from] inner: E } }";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cause_field_diagnostic_spans_the_field_name() {
        let source = "#[derive(Error)]\nenum LoadError { A { #[from] inner: E } }";
        let start = source.find("inner").expect("the source holds the field");

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + 5);
    }

    #[test]
    fn conforming_enum_not_flagged() {
        let diagnostics = run(
            "#[derive(Debug, Error)]\npub enum LoadPluginError {\n    #[error(\"failed to open {path}\")]\n    UnreadableLibrary { path: PathBuf, source: libloading::Error },\n    #[error(\"the plugin was built by {plugin}, this whisker by {host}\")]\n    RustcMismatch { host: String, plugin: String },\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn derive_across_several_attributes_flagged_once() {
        let diagnostics = run("#[derive(Debug)]\n#[derive(Error)]\nenum Plugin { A }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn derive_across_several_attributes_flagged_with_the_enum_name() {
        let diagnostics = run("#[derive(Debug)]\n#[derive(Error)]\nenum Plugin { A }");

        assert_diagnostic(&diagnostics[0]).message_contains("error enum `Plugin`");
    }

    #[test]
    fn derive_behind_doc_comment_flagged_once() {
        let diagnostics = run("#[derive(Error)]\n/// Docs\n#[non_exhaustive]\nenum Plugin { A }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn derive_behind_doc_comment_flagged_with_the_enum_name() {
        let diagnostics = run("#[derive(Error)]\n/// Docs\n#[non_exhaustive]\nenum Plugin { A }");

        assert_diagnostic(&diagnostics[0]).message_contains("error enum `Plugin`");
    }

    #[test]
    fn derive_inside_cfg_attr_not_read() {
        let diagnostics =
            run("#[cfg_attr(feature = \"std\", derive(Error))]\nenum Plugin { Open(u32) }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn derive_list_over_several_lines_flagged_once() {
        let diagnostics =
            run("#[derive(\n    Debug,\n    thiserror::Error,\n)]\npub(crate) enum Plugin { A }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn derive_list_over_several_lines_flagged_with_the_enum_name() {
        let diagnostics =
            run("#[derive(\n    Debug,\n    thiserror::Error,\n)]\npub(crate) enum Plugin { A }");

        assert_diagnostic(&diagnostics[0]).message_contains("error enum `Plugin`");
    }

    #[test]
    fn derive_of_other_error_like_name_not_flagged() {
        let diagnostics = run("#[derive(Debug, ErrorKind, MyError)]\nenum Plugin { Open(u32) }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn derive_path_ending_in_error_flagged_once() {
        let diagnostics = run("#[derive(Debug, thiserror::Error)]\nenum Plugin { A }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn derive_path_ending_in_error_flagged_with_the_enum_name() {
        let diagnostics = run("#[derive(Debug, thiserror::Error)]\nenum Plugin { A }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.error-enum-shape")
            .has_severity(Severity::Warn)
            .message_contains("error enum `Plugin` does not end in `Error`");
    }

    #[test]
    fn doc_comment_between_message_and_variant_read() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to find the plugin\")]\n    /// The plugin is gone\n    Missing,\n}",
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("message of `Missing` starts with \"failed to\"");
    }

    #[test]
    fn doc_comment_between_message_and_variant_yields_one_diagnostic() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to find the plugin\")]\n    /// The plugin is gone\n    Missing,\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn enum_name_diagnostic_reported_once() {
        let diagnostics = run("#[derive(Error)]\nenum Plugin { A }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn enum_name_diagnostic_spans_the_name() {
        let diagnostics = run("#[derive(Error)]\nenum Plugin { A }");

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 22, 28);
    }

    #[test]
    fn enum_named_error_not_flagged() {
        let diagnostics = run("#[derive(Error)]\npub enum Error { A }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn enum_without_error_derive_not_flagged() {
        let diagnostics = run(
            "#[derive(Debug)]\nenum Plugin {\n    #[error(\"open failed\")]\n    Open(#[from] E),\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn example_from_issue_reports_every_shape() {
        let diagnostics = run(
            "#[derive(Debug, Error)]\npub enum PluginErr {\n    #[error(\"open failed\")]\n    Open(#[from] libloading::Error),\n    Mismatch(String, String),\n}",
        );

        assert_eq!(
            messages(&diagnostics),
            vec![
                "error enum `PluginErr` does not end in `Error`; name it after the action that fails and its object",
                "error variant `Open` is a tuple variant; give it named fields, with the cause in `source`",
                "message of `Open` does not start with \"failed to\"; a variant with a cause names the action that failed",
                "error variant `Mismatch` is a tuple variant; give it named fields",
            ]
        );
    }

    #[test]
    fn failed_to_alone_counts_as_failed_to() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to\")]\n    A { source: E },\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn failed_to_inside_word_does_not_count() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed tomorrow\")]\n    A { source: E },\n}",
        );

        assert_diagnostic(&diagnostics[0]).message_contains("does not start with \"failed to\"");
    }

    #[test]
    fn failed_to_inside_word_yields_one_diagnostic() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed tomorrow\")]\n    A { source: E },\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn failed_to_mid_message_does_not_count() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"the plugin failed to load\")]\n    A { path: u32 },\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn field_named_source_without_attribute_is_a_cause() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"the read broke\")]\n    A { source: E },\n}",
        );

        assert_diagnostic(&diagnostics[0]).message_contains("does not start with \"failed to\"");
    }

    #[test]
    fn field_named_source_without_attribute_yields_one_diagnostic() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"the read broke\")]\n    A { source: E },\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn fmt_function_message_not_read() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(fmt = render)]\n    A { source: E },\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn format_arguments_after_message_are_fine() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to open {}\", path.display())]\n    A { path: PathBuf, source: E },\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn from_field_named_source_not_flagged() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to open\")]\n    Open { #[from] source: E },\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn from_field_not_named_source_flagged_once() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to open\")]\n    Open { #[from] inner: E },\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn from_field_not_named_source_flagged_with_the_field_name() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to open\")]\n    Open { #[from] inner: E },\n}",
        );

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.error-enum-shape")
            .has_severity(Severity::Warn)
            .message_contains("field `inner` holds the cause of `Open`; name it `source`");
    }

    #[test]
    fn generic_enum_name_read_without_parameters() {
        let diagnostics = run("#[derive(Error)]\nenum Plugin<T> { A { value: T } }");

        assert_diagnostic(&diagnostics[0]).message_contains("error enum `Plugin` does not end");
    }

    #[test]
    fn generic_enum_name_yields_one_diagnostic() {
        let diagnostics = run("#[derive(Error)]\nenum Plugin<T> { A { value: T } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn message_diagnostic_reported_once() {
        let source = "#[derive(Error)]\nenum LoadError {\n    #[error(\"open failed\")]\n    A { source: E },\n}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn message_diagnostic_spans_the_literal() {
        let source = "#[derive(Error)]\nenum LoadError {\n    #[error(\"open failed\")]\n    A { source: E },\n}";
        let start = source.find("\"open").expect("the source holds the message");

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + 13);
    }

    #[test]
    fn message_with_capital_failed_to_and_cause_flagged_as_not_starting_with_failed_to() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"Failed to open\")]\n    A { source: E },\n}",
        );

        assert_diagnostic(&diagnostics[0]).message_contains("does not start with \"failed to\"");
    }

    #[test]
    fn message_with_capital_failed_to_and_cause_flagged_once() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"Failed to open\")]\n    A { source: E },\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn message_with_cause_not_failed_to_flagged_once() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"open failed for {path}\")]\n    Open { path: u32, source: E },\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn message_with_cause_not_failed_to_flagged_with_the_variant_name() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"open failed for {path}\")]\n    Open { path: u32, source: E },\n}",
        );

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.error-enum-shape")
            .has_severity(Severity::Warn)
            .message_contains("message of `Open` does not start with \"failed to\"");
    }

    #[test]
    fn message_without_cause_failed_to_flagged_as_having_no_cause() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to match {host}\")]\n    Mismatch { host: String },\n}",
        );

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.error-enum-shape")
            .has_severity(Severity::Warn)
            .message_contains(
                "message of `Mismatch` starts with \"failed to\" but the variant has no cause",
            );
    }

    #[test]
    fn message_without_cause_failed_to_flagged_once() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to match {host}\")]\n    Mismatch { host: String },\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn misnamed_cause_with_failed_to_reports_only_the_name() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to open\")]\n    Open { #[source] inner: E },\n}",
        );

        assert_eq!(
            messages(&diagnostics),
            vec!["field `inner` holds the cause of `Open`; name it `source`"]
        );
    }

    #[test]
    fn name_ending_in_error_not_flagged() {
        let diagnostics = run("#[derive(Error)]\nenum LoadError { A }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_enum_in_module_and_function_flagged_once() {
        let diagnostics = run(
            "mod inner {\n    fn f() {\n        #[derive(Error)]\n        enum Plugin { A }\n    }\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn nested_enum_in_module_and_function_flagged_with_the_enum_name() {
        let diagnostics = run(
            "mod inner {\n    fn f() {\n        #[derive(Error)]\n        enum Plugin { A }\n    }\n}",
        );

        assert_diagnostic(&diagnostics[0]).message_contains("error enum `Plugin`");
    }

    #[test]
    fn raw_string_message_read() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(r#\"failed to read \"{path}\"\"#)]\n    A { path: u32, source: E },\n    #[error(r\"failed to match\")]\n    B,\n}",
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("message of `B` starts with \"failed to\"");
    }

    #[test]
    fn raw_string_message_yields_one_diagnostic() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(r#\"failed to read \"{path}\"\"#)]\n    A { path: u32, source: E },\n    #[error(r\"failed to match\")]\n    B,\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn source_attribute_field_not_named_source_flagged_once() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to open\")]\n    Open { path: u32, #[source] cause: E },\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn source_attribute_field_not_named_source_flagged_with_the_field_name() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to open\")]\n    Open { path: u32, #[source] cause: E },\n}",
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("field `cause` holds the cause of `Open`");
    }

    #[test]
    fn struct_deriving_error_not_flagged() {
        let diagnostics =
            run("#[derive(Error)]\n#[error(\"open failed\")]\nstruct Plugin(#[from] E);");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ErrorEnumShape>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ErrorEnumShape>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ErrorEnumShape>();
    }

    #[test]
    fn transparent_named_variant_not_flagged() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(transparent)]\n    Other { #[from] source: E },\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn transparent_skips_message_checks_but_not_tuple_check() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(transparent)]\n    Other(#[from] anyhow::Error),\n}",
        );

        assert_eq!(
            messages(&diagnostics),
            vec![
                "error variant `Other` is a tuple variant; give it named fields, with the cause in `source`"
            ]
        );
    }

    #[test]
    fn transparent_with_misnamed_cause_flagged() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(transparent)]\n    Other { #[from] inner: E },\n}",
        );

        assert_eq!(
            messages(&diagnostics),
            vec!["field `inner` holds the cause of `Other`; name it `source`"]
        );
    }

    #[test]
    fn tuple_variant_diagnostic_reported_once() {
        let source = "#[derive(Error)]\nenum LoadError { Mismatch(u32) }";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn tuple_variant_diagnostic_spans_the_variant_name() {
        let source = "#[derive(Error)]\nenum LoadError { Mismatch(u32) }";
        let start = source
            .find("Mismatch")
            .expect("the source holds the variant");

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + 8);
    }

    #[test]
    fn tuple_variant_flagged_once() {
        let diagnostics = run("#[derive(Error)]\nenum LoadError { Mismatch(String, String) }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn tuple_variant_flagged_with_the_variant_name() {
        let diagnostics = run("#[derive(Error)]\nenum LoadError { Mismatch(String, String) }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.error-enum-shape")
            .has_severity(Severity::Warn)
            .message_contains("error variant `Mismatch` is a tuple variant; give it named fields");
    }

    #[test]
    fn tuple_variant_with_source_attribute_names_the_cause() {
        let diagnostics = run("#[derive(Error)]\nenum LoadError { Read(u32, #[source] E) }");

        assert_diagnostic(&diagnostics[0]).message_contains("with the cause in `source`");
    }

    #[test]
    fn tuple_variant_with_source_attribute_yields_one_diagnostic() {
        let diagnostics = run("#[derive(Error)]\nenum LoadError { Read(u32, #[source] E) }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn unit_variant_with_condition_not_flagged() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"the plugin is missing\")]\n    Missing,\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn unit_variant_with_failed_to_flagged_once() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to find the plugin\")]\n    Missing,\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn unit_variant_with_failed_to_flagged_with_the_variant_name() {
        let diagnostics = run(
            "#[derive(Error)]\nenum LoadError {\n    #[error(\"failed to find the plugin\")]\n    Missing,\n}",
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("message of `Missing` starts with \"failed to\"");
    }

    #[test]
    fn variant_without_error_attribute_not_checked_for_message() {
        let diagnostics = run("#[derive(Error)]\nenum LoadError { A { source: E }, B }");

        assert_no_diagnostics(&diagnostics);
    }
}
