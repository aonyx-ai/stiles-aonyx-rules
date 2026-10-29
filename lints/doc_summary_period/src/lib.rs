use doc_comment::DocLine;
use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity, Span};

const RULE_ID: RuleId = RuleId::new("lint.doc-summary-period");

/// The node kinds that can sit between the pieces of one item's docs
///
/// Comments and attributes can sit between two pieces of one item's docs.
/// A walk over siblings that meets any other kind has left the item.
const ATTACHED_KINDS: &[&str] = &[
    "attribute_item",
    "block_comment",
    "inner_attribute_item",
    "line_comment",
];

/// Flags a doc summary that ends with a period
///
/// Rustdoc shows the summary beside the item in module listings and search
/// results. The convention writes it as a short description without a
/// final period, such as `/// Returns the length of the string`.
///
/// The summary is the first paragraph of the doc comment, which is what
/// rustdoc shows. A summary that wraps over several lines therefore ends on
/// its last line, and only the last character of that line matters. A
/// period earlier in the summary is fine. A code span at the end, as in
/// ``/// Reads `Cargo.toml` ``, ends with a backtick, so it is fine too. An
/// ellipsis is not a final period.
///
/// A first line that opens a code block, a heading, a list, or a quote holds
/// no summary, so the rule skips it.
///
/// The rule reads `///`, `//!`, `/** */`, and `/*! */` comments. It cannot
/// read the text of a `#[doc = ...]` attribute. When such an attribute
/// comes before the comment, the summary can lie in that text, so the rule
/// stays silent. A summary that runs to the end of the comment can also
/// continue in later docs of the same item. The rule stays silent then too.
///
/// # Examples
///
/// ```
/// use doc_summary_period::DocSummaryPeriod;
/// use stiles_rust::RustLintPassAdapter;
/// use stiles_testing::{execute, parse};
/// use stiles_types::{Language, LintPass};
///
/// let tree = parse("/// Returns the length.\nfn len() {}", Language::Rust);
/// let pass: Box<dyn LintPass> = Box::new(RustLintPassAdapter::new(DocSummaryPeriod));
/// let mut passes = vec![pass];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct DocSummaryPeriod;

/// Which item a doc comment or a doc attribute documents
///
/// An outer one documents the item after it, and an inner one documents the
/// item around it. Only pieces with the same placement are parts of one doc.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum Placement {
    Inner,
    Outer,
}

impl RustLintPass for DocSummaryPeriod {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report(node).into_iter().collect()
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report(node).into_iter().collect()
    }
}

/// Returns a diagnostic when `node` starts the docs of an item and their
/// summary ends with a period
fn report(node: &DecoratedNode<'_>) -> Option<Diagnostic> {
    let lines = doc_comment::rust(node)?;
    let placement = placement(node)?;
    let end = summary_end(&lines)?;
    let period = trailing_period(lines[end])?;

    let siblings = node.parent()?.named_children();
    let index = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())?;
    if has_docs(siblings[..index].iter().rev(), placement) {
        return None;
    }

    let last = lines.last()?.offset();
    let later = siblings[index + 1..]
        .iter()
        .skip_while(|sibling| sibling.span().start() <= last);
    if end + 1 == lines.len() && has_docs(later, placement) {
        return None;
    }

    let file = node.span();
    Some(
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Error)
            .message("doc summary ends with a period")
            .span(
                Span::builder()
                    .file(file.file_path().clone())
                    .start(period)
                    .end(period + 1)
                    .build(),
            )
            .build(),
    )
}

/// Returns the placement of a doc comment or a doc attribute, or [`None`]
/// for any other node
///
/// The grammar records a comment's documentation in the `doc` field and its
/// marker in `inner` or `outer`.
///
/// [`None`]: std::option::Option::None
fn placement(node: &DecoratedNode<'_>) -> Option<Placement> {
    match node.kind() {
        "line_comment" | "block_comment" => {
            node.child_by_field_name("doc")?;
            match node.child_by_field_name("inner") {
                Some(_) => Some(Placement::Inner),
                None => node.child_by_field_name("outer").map(|_| Placement::Outer),
            }
        }
        "attribute_item" if carries_doc_text(node) => Some(Placement::Outer),
        "inner_attribute_item" if carries_doc_text(node) => Some(Placement::Inner),
        _ => None,
    }
}

/// Returns whether an attribute adds doc text the rule cannot read
///
/// `#[doc = ...]` adds text, and so does a `doc = ...` inside `cfg_attr`.
/// `#[doc(hidden)]` and `#[doc(alias = ...)]` add none.
fn carries_doc_text(attribute: &DecoratedNode<'_>) -> bool {
    let text: String = attribute
        .text()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let Some(body) = text.strip_prefix("#![").or_else(|| text.strip_prefix("#[")) else {
        return false;
    };

    body.starts_with("doc=") || (body.starts_with("cfg_attr(") && body.contains("doc="))
}

/// Returns whether the walk over `siblings` meets docs with `placement`
/// before it leaves the item
fn has_docs<'a, 'b: 'a>(
    siblings: impl Iterator<Item = &'a DecoratedNode<'b>>,
    placement: Placement,
) -> bool {
    siblings
        .take_while(|sibling| ATTACHED_KINDS.contains(&sibling.kind()))
        .any(|sibling| self::placement(sibling) == Some(placement))
}

/// Returns the index of the last line of the summary, or [`None`] when the
/// doc has no prose summary
///
/// The summary is the first paragraph, after any blank lines. A blank line
/// ends it, and so does a line that opens a code fence, a heading, a list,
/// or a quote. A setext underline turns the paragraph into a heading, so
/// such a doc has no summary.
///
/// [`None`]: std::option::Option::None
fn summary_end(lines: &[DocLine<'_>]) -> Option<usize> {
    let start = lines.iter().position(|line| !is_blank(line.text()))?;
    let first = lines[start].text();
    if spaces(first) >= 4 || opens_block(first) || list_number(first).is_some() {
        return None;
    }

    let mut end = start;
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        let text = line.text();
        if is_setext_underline(text) {
            return None;
        }
        if is_blank(text) || opens_block(text) || list_number(text) == Some(1) {
            break;
        }
        end = index;
    }

    Some(end)
}

/// Returns the file offset of the period that ends `line`, or [`None`] when
/// it does not end with one
///
/// Trailing whitespace does not count. An ellipsis is not a period.
///
/// [`None`]: std::option::Option::None
fn trailing_period(line: DocLine<'_>) -> Option<usize> {
    let text = line.text().trim_end();
    if !text.ends_with('.') || text.ends_with("...") {
        return None;
    }

    Some(line.offset() + text.len() - 1)
}

/// Returns whether `text` is empty or whitespace alone
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// Returns the number of spaces that open `text`
fn spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

/// Returns whether `text` opens a code fence, a heading, a bullet list
/// item, or a quote
///
/// Each of these ends a paragraph. An ordered list item is a separate check,
/// because only an item numbered 1 ends one. Four spaces of indentation make
/// the line part of the paragraph instead.
fn opens_block(text: &str) -> bool {
    if spaces(text) >= 4 {
        return false;
    }
    let text = text.trim_start();

    text.starts_with("```")
        || text.starts_with("~~~")
        || text.starts_with('>')
        || is_heading(text)
        || is_bullet(text)
}

/// Returns whether `text` is an ATX heading, one to six `#` and a space
fn is_heading(text: &str) -> bool {
    let rest = text.trim_start_matches('#');
    let level = text.len() - rest.len();

    (1..=6).contains(&level) && (rest.is_empty() || rest.starts_with([' ', '\t']))
}

/// Returns whether `text` opens a bullet list item
fn is_bullet(text: &str) -> bool {
    let mut characters = text.chars();
    let marker = characters.next();
    let after = characters.next();

    marker.is_some_and(|marker| ['-', '*', '+'].contains(&marker))
        && after.is_some_and(|after| [' ', '\t'].contains(&after))
}

/// Returns the number of the ordered list item that `text` opens, or
/// [`None`] when it opens none
///
/// An ordered list item is one to nine digits, then `.` or `)`, then a
/// space. Only an item numbered 1 can end a paragraph, so a wrapped line
/// such as `2019. Then` stays part of it.
///
/// [`None`]: std::option::Option::None
fn list_number(text: &str) -> Option<u32> {
    if spaces(text) >= 4 {
        return None;
    }
    let text = text.trim_start();
    let rest = text.trim_start_matches(|character: char| character.is_ascii_digit());
    let digits = &text[..text.len() - rest.len()];
    if !(1..=9).contains(&digits.len()) {
        return None;
    }

    let after = rest.strip_prefix(['.', ')'])?;
    if !after.starts_with([' ', '\t']) {
        return None;
    }

    digits.parse().ok()
}

/// Returns whether `text` underlines the paragraph above it as a heading
///
/// The underline is a run of `=` or a run of `-`, after at most three spaces.
fn is_setext_underline(text: &str) -> bool {
    if spaces(text) >= 4 {
        return false;
    }
    let text = text.trim();

    !text.is_empty() && (text.trim_matches('=').is_empty() || text.trim_matches('-').is_empty())
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for DocSummaryPeriod {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

stiles_plugin::export_lints![rust: [DocSummaryPeriod]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass};

    use super::*;

    fn check(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(DocSummaryPeriod))];

        execute(&tree, &mut passes)
    }

    /// Returns the file offset of the one finding in `source`
    ///
    /// Asserts that there is exactly one, that it covers one period, and that
    /// it carries this rule's id, severity, and message.
    fn flagged_period(source: &str) -> usize {
        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let span = diagnostics[0].span();
        assert_eq!(&source[span.start()..span.end()], ".");
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.doc-summary-period")
            .has_severity(Severity::Error)
            .message_contains("doc summary ends with a period");
        span.start()
    }

    #[test]
    fn block_doc_comment_on_one_line_is_flagged() {
        let source = "/** Returns the length. */\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with(" */"));
    }

    #[test]
    fn block_doc_comment_with_stars_is_flagged() {
        let source = "/**\n * Returns the length.\n *\n * More.\n */\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n *\n"));
    }

    #[test]
    fn block_inner_doc_comment_is_flagged() {
        let source = "/*! Holds the parser. */\n";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with(" */"));
    }

    #[test]
    fn body_paragraph_with_a_period_is_not_flagged() {
        let diagnostics = check("/// Returns the length\n///\n/// It counts bytes.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bullet_list_after_the_summary_ends_it() {
        let diagnostics =
            check("/// Reads two things:\n/// - the name.\n/// - the age.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bullet_list_as_the_first_line_is_not_flagged() {
        let diagnostics = check("/// - the name.\n/// - the age.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_attr_doc_before_the_comment_is_not_flagged() {
        let diagnostics =
            check("#[cfg_attr(docsrs, doc = \"Intro\")]\n/// Returns the length.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn code_fence_after_a_summary_with_a_period_is_flagged() {
        let source = "/// Returns the length.\n/// ```\n/// f();\n/// ```\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n/// ```\n/// f"));
    }

    #[test]
    fn code_fence_after_the_summary_ends_it() {
        let diagnostics =
            check("/// Returns the length\n/// ```\n/// let a = 1.\n/// ```\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn code_fence_as_the_first_line_is_not_flagged() {
        let diagnostics = check("/// ```\n/// let a = 1.\n/// ```\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn code_span_at_the_end_is_not_flagged() {
        let diagnostics = check("/// Reads `Cargo.toml`\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn code_span_followed_by_a_period_is_flagged() {
        let source = "/// Reads `Cargo.toml`.\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\nfn"));
    }

    #[test]
    fn crlf_line_ending_is_flagged_at_the_period() {
        let source = "/// Returns the length.\r\nfn f() {}\r\n";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\r\nfn"));
    }

    #[test]
    fn doc_alias_attribute_before_the_comment_is_flagged() {
        let source = "#[doc(alias = \"len\")]\n/// Returns the length.\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\nfn"));
    }

    #[test]
    fn doc_attribute_after_a_closed_summary_is_flagged() {
        let source = "/// Returns the length.\n///\n/// More\n#[doc = \"Extra\"]\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n///\n"));
    }

    #[test]
    fn doc_attribute_after_an_open_summary_is_not_flagged() {
        let diagnostics = check("/// Returns the length.\n#[doc = \"More\"]\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_attribute_alone_is_not_flagged() {
        let diagnostics = check("#[doc = \"Returns the length.\"]\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_attribute_before_the_comment_is_not_flagged() {
        let diagnostics = check("#[doc = \"Intro\"]\n/// Returns the length.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_comment_inside_an_impl_is_flagged() {
        let source = "impl S {\n    /// Returns the length.\n    fn f(&self) {}\n}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n    fn"));
    }

    #[test]
    fn doc_comment_on_a_field_after_an_attribute_is_flagged() {
        let source = "struct S {\n    #[serde(default)]\n    /// The name.\n    name: String,\n}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n    name"));
    }

    #[test]
    fn doc_comment_on_a_variant_is_flagged() {
        let source = "enum E {\n    /// The first.\n    A,\n}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n    A"));
    }

    #[test]
    fn ellipsis_is_not_flagged() {
        let diagnostics = check("/// Waits for the host to connect, ...\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn heading_as_the_first_line_is_not_flagged() {
        let diagnostics = check("/// # Examples.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn indented_code_as_the_first_line_is_not_flagged() {
        let diagnostics = check("///     let a = 1.\n/// Returns the length\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_attribute_before_an_inner_comment_is_not_flagged() {
        let diagnostics = check("#![doc = include_str!(\"../README.md\")]\n//! More.\n");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_attribute_does_not_hide_an_outer_comment() {
        let source = "#![doc = include_str!(\"../README.md\")]\n/// Returns the length.\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\nfn"));
    }

    #[test]
    fn inner_doc_comment_inside_a_module_is_flagged() {
        let source = "mod m {\n    //! Holds the parser.\n}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n}"));
    }

    #[test]
    fn inner_doc_comment_is_flagged() {
        let source = "//! Holds the parser.\n\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n\nfn"));
    }

    #[test]
    fn inner_run_does_not_continue_into_an_outer_run() {
        let source = "//! Holds the parser.\n/// Returns it\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n///"));
    }

    #[test]
    fn leading_blank_lines_are_skipped() {
        let source = "///\n/// Returns the length.\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\nfn"));
    }

    #[test]
    fn multibyte_summary_is_flagged_at_the_period() {
        let source = "/// Returns the résumé — 日本語.\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\nfn"));
    }

    #[test]
    fn non_doc_comment_is_not_flagged() {
        let diagnostics =
            check("// Returns the length.\n//// Returns it.\n/* Returns it. */\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn ordered_list_after_the_summary_ends_it() {
        let diagnostics = check("/// Runs two steps\n/// 1. Reads.\n/// 2. Writes.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn ordered_list_as_the_first_line_is_not_flagged() {
        let diagnostics = check("/// 3) Reads the file.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn period_inside_the_summary_is_not_flagged() {
        let diagnostics = check("/// Reads v1.2 files, e.g. `a.toml` and the rest\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn quote_after_the_summary_ends_it() {
        let diagnostics = check("/// Returns the length\n/// > Quoted.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn second_run_of_the_same_item_is_not_flagged() {
        let diagnostics =
            check("/// Returns the length\n///\n/// More\n// note\n/// Later.\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn separate_items_are_flagged_once_each() {
        let diagnostics = check("/// Returns one.\nfn f() {}\n/// Returns two.\nfn g() {}");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn setext_underline_is_not_flagged() {
        let diagnostics = check("/// Overview.\n/// ========\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn summary_continued_in_a_later_run_is_not_flagged() {
        let diagnostics = check("/// Returns the length.\n// note\n/// More\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn summary_without_a_period_is_not_flagged() {
        let diagnostics = check("/// Returns the length of the string\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trailing_whitespace_after_the_period_is_flagged() {
        let source = "/// Returns the length.   \nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("   \nfn"));
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DocSummaryPeriod>();
    }

    #[test]
    fn trait_send_placement() {
        fn assert_send<T: Send>() {}
        assert_send::<Placement>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DocSummaryPeriod>();
    }

    #[test]
    fn trait_sync_placement() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Placement>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DocSummaryPeriod>();
    }

    #[test]
    fn trait_unpin_placement() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Placement>();
    }

    #[test]
    fn unicode_ellipsis_is_not_flagged() {
        let diagnostics = check("/// Waits for the host to connect…\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn wrapped_line_starting_with_a_year_stays_in_the_summary() {
        let source = "/// Lists the releases since\n/// 2019. Then stops.\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\nfn"));
    }

    #[test]
    fn wrapped_summary_ending_with_a_period_is_flagged() {
        let source = "/// Returns the length of the string, counted\n/// in bytes.\n///\n/// More\nfn f() {}";

        let period = flagged_period(source);

        assert!(source[period + 1..].starts_with("\n///\n"));
    }

    #[test]
    fn wrapped_summary_with_a_period_mid_paragraph_is_not_flagged() {
        let diagnostics = check("/// Returns the length.\n/// Counts bytes\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }
}
