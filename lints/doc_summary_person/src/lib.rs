use std::path::Path;

use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity, Span};

const RULE_ID: RuleId = RuleId::new("lint.doc-summary-person");

/// Verbs that end in `-ly`, in alphabetical order
///
/// "Apply edits" opens with a bare verb. Yet it has the shape of
/// "Recursively collects": a first word in `-ly` and a second word in `-s`.
/// A word on this list is never read as an adverb, so the summary is
/// reported.
const LY_VERBS: &[&str] = &[
    "apply", "comply", "fly", "imply", "multiply", "rally", "rely", "reply", "supply", "tally",
];

/// Flags a function doc summary that does not open with a third-person verb
///
/// The convention is a third-person singular summary line: "Returns the
/// length", not "Return the length" and not "Length of the string". A
/// third-person singular verb in English always ends in `-s`. So a first
/// word that does not end in `s` proves the summary breaks the convention.
/// The rule needs no verb list. Trailing punctuation such as `,`, `:`, or
/// `;` is not part of the word.
///
/// An adverb in `-ly` followed by a word in `-s` passes, as in "Recursively
/// collects". A summary that opens with a code span or a link is reported,
/// because neither is a verb. A doc comment that opens with a code fence has
/// no prose summary, and the rule leaves it alone. A plural noun such as
/// "Bytes of the file" ends in `-s` and passes: that is an accepted false
/// negative.
///
/// The rule reads functions and methods only, including trait methods with
/// no body. A struct or a constant is documented with a noun phrase. Test
/// code is exempt. That covers a function with a test attribute such as
/// `#[test]` or `#[tokio::test]`, and anything under a `#[cfg(test)]` item.
/// It also covers anything in a `tests` or `benches` directory.
///
/// The summary is the first line of text in the item's first outer doc
/// comment. When a `#[doc = ...]` attribute comes before that comment, the
/// summary is text the rule cannot read, so it stays silent.
pub struct DocSummaryPerson;

impl RustLintPass for DocSummaryPerson {
    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report(node)
    }

    fn check_function_signature_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report(node)
    }
}

/// Reports the summary of a function's doc comment if its first word is not
/// a third-person verb
fn report(node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let span = node.span();
    if is_test_target(span.file()) || is_test_code(node) {
        return Vec::new();
    }

    let Some(comment) = first_doc_comment(node) else {
        return Vec::new();
    };
    let Some(lines) = doc_comment::rust(&comment) else {
        return Vec::new();
    };
    let Some(summary) = lines.iter().find(|line| !line.text().trim().is_empty()) else {
        return Vec::new();
    };

    let text = summary.text().trim_start();
    let Some(token) = opening_token(text) else {
        return Vec::new();
    };

    let start = summary.offset() + summary.text().len() - text.len();
    vec![Diagnostic::new(
        RULE_ID,
        Severity::Warn,
        format!(
            "doc summary opens with \"{token}\"; open it with a third-person verb, such as \"Returns\" or \"Creates\""
        ),
        Span::new(span.file_path().clone(), start, start + token.len()),
    )]
}

/// Returns the token that `summary` opens with if it shows that the summary
/// does not open with a third-person verb
///
/// A code span, a link, or an autolink is never a verb, so it is returned
/// whole. Otherwise words split at whitespace, and trailing ASCII
/// punctuation is cut from each. A word that is nothing but punctuation
/// stays whole, so the diagnostic still names it. A code fence is no
/// summary at all, and returns [`None`].
///
/// [`None`]: std::option::Option::None
fn opening_token(summary: &str) -> Option<&str> {
    if is_fence(summary) {
        return None;
    }
    if summary.starts_with(['`', '[', '<']) {
        let end = markup_end(summary).or_else(|| summary.find(char::is_whitespace));
        return Some(&summary[..end.unwrap_or(summary.len())]);
    }

    let mut words = summary.split_whitespace().map(bare_word);
    let first = words.next()?;
    if is_third_person(first) {
        return None;
    }
    if is_adverb(first) && words.next().is_some_and(is_third_person) {
        return None;
    }

    Some(first)
}

/// Returns whether a line opens a code fence
///
/// A tilde fence opens with `~~~`, and a backtick fence with three
/// backticks. CommonMark forbids a backtick in the info string of a backtick
/// fence. So "```a``` b" is a code span and not a fence.
fn is_fence(line: &str) -> bool {
    if line.starts_with("~~~") {
        return true;
    }
    let Some(info) = line.strip_prefix("```") else {
        return false;
    };

    !info.trim_start_matches('`').contains('`')
}

/// Returns the byte offset just past the code span, link, or autolink that
/// opens `summary`
///
/// Returns [`None`] when the markup is never closed on the line.
///
/// [`None`]: std::option::Option::None
fn markup_end(summary: &str) -> Option<usize> {
    match summary.as_bytes().first()? {
        b'`' => code_span_end(summary, 0),
        b'[' => link_end(summary),
        b'<' => summary.find('>').map(|end| end + 1),
        _ => None,
    }
}

/// Returns the byte offset just past the code span that opens at `start`
///
/// A code span closes at the next run of exactly as many backticks as
/// opened it.
fn code_span_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let width = backtick_run(bytes, start);
    let mut position = start + width;
    while position < bytes.len() {
        if bytes[position] != b'`' {
            position += 1;
            continue;
        }
        let closing = backtick_run(bytes, position);
        if closing == width {
            return Some(position + closing);
        }
        position += closing;
    }

    None
}

/// Returns how many backticks follow one another from `start`
fn backtick_run(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .take_while(|&&byte| byte == b'`')
        .count()
}

/// Returns the byte offset just past the link that opens `text`
///
/// The link text ends at the bracket that balances the opening one, and a
/// code span inside it may hold brackets of its own. A destination in
/// parentheses or a reference label in brackets, written right after and
/// closed, is part of the link.
fn link_end(text: &str) -> Option<usize> {
    let text_end = balanced_end(text, 0)?;

    Some(balanced_end(text, text_end).unwrap_or(text_end))
}

/// Returns the byte offset just past the delimiter that balances the one at
/// `start`, skipping code spans
///
/// The byte at `start` must be `[` or `(`. Returns [`None`] for any other
/// byte, and when nothing balances it.
///
/// [`None`]: std::option::Option::None
fn balanced_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let open = *bytes.get(start)?;
    let close = if open == b'[' {
        b']'
    } else if open == b'(' {
        b')'
    } else {
        return None;
    };

    let mut depth = 0_usize;
    let mut position = start;
    while position < bytes.len() {
        let byte = bytes[position];
        if byte == b'`' {
            position = match code_span_end(text, position) {
                Some(end) => end,
                None => position + backtick_run(bytes, position),
            };
            continue;
        }
        if byte == open {
            depth += 1;
        } else if byte == close {
            depth -= 1;
            if depth == 0 {
                return Some(position + 1);
            }
        }
        position += 1;
    }

    None
}

/// Returns a word without its trailing ASCII punctuation
///
/// A word that is nothing but punctuation, such as `...`, is returned
/// whole.
fn bare_word(word: &str) -> &str {
    match word.trim_end_matches(|c: char| c.is_ascii_punctuation()) {
        "" => word,
        bare => bare,
    }
}

/// Returns whether a word has the third-person singular ending
fn is_third_person(word: &str) -> bool {
    word.ends_with('s')
}

/// Returns whether a word reads as an adverb in `-ly`
///
/// "Ly" alone is not an adverb, and neither is a verb on [`LY_VERBS`].
fn is_adverb(word: &str) -> bool {
    let lowercase = word.to_lowercase();

    lowercase.len() > "ly".len()
        && lowercase.ends_with("ly")
        && LY_VERBS.binary_search(&lowercase.as_str()).is_err()
}

/// Returns the first outer doc comment attached to an item
///
/// The walk goes back over the attributes and comments above the item. It
/// stops at anything else, and at an inner doc comment, which documents the
/// enclosing item. Returns [`None`] when the item has no doc comment, or
/// when a `#[doc = ...]` attribute supplies text before the first one.
///
/// [`None`]: std::option::Option::None
fn first_doc_comment<'a>(node: &DecoratedNode<'a>) -> Option<DecoratedNode<'a>> {
    let parent = node.parent()?;
    let index = child_index(&parent, node)?;

    let mut first = None;
    for i in (0..index).rev() {
        let Some(sibling) = parent.child(i) else {
            break;
        };

        match sibling.kind() {
            "attribute_item" => {
                if supplies_doc_text(&sibling) {
                    first = None;
                }
            }
            "line_comment" | "block_comment" => {
                if sibling.child_by_field_name("inner").is_some() {
                    break;
                }
                if sibling.child_by_field_name("doc").is_some() {
                    first = Some(sibling);
                }
            }
            _ => break,
        }
    }

    first
}

/// Returns the index of a node among its parent's children
///
/// The index counts anonymous children too, so the backward walk from it
/// sees every token between two items.
fn child_index(parent: &DecoratedNode<'_>, node: &DecoratedNode<'_>) -> Option<u32> {
    let count = u32::try_from(parent.child_count()).ok()?;
    (0..count).find(|&i| parent.child(i).is_some_and(|child| child.id() == node.id()))
}

/// Returns whether an attribute adds doc text the rule cannot read
///
/// `#[doc = ...]` supplies text from elsewhere, such as `include_str!`, and
/// `#[cfg_attr(..., doc = ...)]` does the same under a condition.
/// `#[doc(hidden)]` and `#[doc(alias = ...)]` add no text.
fn supplies_doc_text(attribute: &DecoratedNode<'_>) -> bool {
    let text: String = attribute
        .text()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    text.starts_with("#[doc=") || (text.starts_with("#[cfg_attr(") && text.contains("doc="))
}

/// Returns whether any component of a path is `tests` or `benches`
///
/// Cargo builds those directories only for test and benchmark targets, so
/// the code there is test code. `lint.missing-trait-tests` reads the path
/// the same way.
fn is_test_target(path: &Path) -> bool {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .any(|name| name == "tests" || name == "benches")
}

/// Returns whether the node sits in code that only a test build compiles
///
/// The walk visits the node and every ancestor. An item writes its gate in
/// front of itself, and a module or a file writes it inside, so both
/// spellings count at every level. A test attribute on the function itself
/// counts, and so does one on an enclosing function, for a helper declared
/// inside a test.
fn is_test_code(node: &DecoratedNode<'_>) -> bool {
    let mut current = Some(*node);
    while let Some(item) = current {
        let outer = outer_attributes(&item);
        if outer.iter().any(is_test_attribute) || outer.iter().any(is_test_cfg) {
            return true;
        }
        if inner_attributes(&item).iter().any(is_test_cfg) {
            return true;
        }
        current = item.parent();
    }
    false
}

/// Returns the `attribute` nodes written in front of the node
///
/// Tree-sitter makes each attribute a sibling of the item it decorates. The
/// walk reads backward from the node over attributes and comments. It stops
/// at the first sibling that is neither, so an attribute on the item before
/// this one never reaches it.
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
    attributes
}

/// Returns the `attribute` nodes of the `#![..]` items that open the node
///
/// A file and a module body hold their inner attributes as leading
/// children. The walk stops at the first child that is neither an inner
/// attribute nor a comment.
fn inner_attributes<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let mut attributes = Vec::new();
    for child in node.named_children() {
        match child.kind() {
            "inner_attribute_item" => attributes.extend(child.named_child(0)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    attributes
}

/// Returns whether the attribute marks a test, such as `#[tokio::test]`
///
/// Only the last segment of the path counts, because `#[tokio::test]` and
/// `#[test]` after a `use tokio::test` name one macro.
fn is_test_attribute(attribute: &DecoratedNode<'_>) -> bool {
    let Some(path) = attribute.named_child(0) else {
        return false;
    };
    let name = match path.kind() {
        "scoped_identifier" => path.child_by_field_name("name"),
        _ => Some(path),
    };
    name.is_some_and(|name| name.text() == "test")
}

/// Returns whether the attribute is a `cfg` that holds only in tests
///
/// `cfg_attr` does not count, because it applies another attribute rather
/// than removing the item.
fn is_test_cfg(attribute: &DecoratedNode<'_>) -> bool {
    let Some(path) = attribute.named_child(0) else {
        return false;
    };
    if path.kind() != "identifier" || path.text() != "cfg" {
        return false;
    }
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return false;
    };

    let predicates = predicates(&arguments);
    match predicates.as_slice() {
        [predicate] => implies_test(predicate),
        _ => false,
    }
}

/// Splits a `cfg` token tree into its comma-separated predicates
///
/// Each predicate is the list of tokens between two commas, with the
/// tree's own parentheses left out.
fn predicates<'a>(tree: &DecoratedNode<'a>) -> Vec<Vec<DecoratedNode<'a>>> {
    let mut predicates = Vec::new();
    let mut current = Vec::new();
    let Ok(count) = u32::try_from(tree.child_count()) else {
        return predicates;
    };
    for position in 0..count {
        let Some(token) = tree.child(position) else {
            continue;
        };
        match token.kind() {
            "(" | ")" => {}
            "," => predicates.push(std::mem::take(&mut current)),
            _ => current.push(token),
        }
    }
    if !current.is_empty() {
        predicates.push(current);
    }
    predicates
}

/// Returns whether a `cfg` predicate can hold only when `test` holds
///
/// `all(..)` needs one operand that implies `test`, and `any(..)` needs every
/// operand to. `not(..)` never does, and neither does a key-value predicate,
/// so `cfg(feature = "test")` and `cfg(not(test))` are not test code.
fn implies_test(predicate: &[DecoratedNode<'_>]) -> bool {
    match predicate {
        [word] => word.kind() == "identifier" && word.text() == "test",
        [operator, operands] if operands.kind() == "token_tree" => {
            let operands = predicates(operands);
            match operator.text() {
                "all" => operands.iter().any(|operand| implies_test(operand)),
                "any" => {
                    !operands.is_empty() && operands.iter().all(|operand| implies_test(operand))
                }
                _ => false,
            }
        }
        _ => false,
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for DocSummaryPerson {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

whisker_plugin::export_lints![rust: [DocSummaryPerson]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn check(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(DocSummaryPerson))];

        execute(&tree, &mut passes)
    }

    /// Returns the word each diagnostic names, in order
    fn flagged_words(source: &str) -> Vec<String> {
        check(source)
            .iter()
            .map(|diagnostic| {
                let span = diagnostic.span();
                source[span.start()..span.end()].to_owned()
            })
            .collect()
    }

    #[test]
    fn imperative_is_flagged() {
        let source = "/// Return the length of the string\nfn len() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.doc-summary-person")
            .has_severity(Severity::Warn)
            .message_contains(
                "doc summary opens with \"Return\"; open it with a third-person verb, such as \"Returns\" or \"Creates\"",
            )
            .has_span("<test>", 4, 10);
    }

    #[test]
    fn third_person_is_not_flagged() {
        let diagnostics = check("/// Returns the length of the string\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn irregular_third_person_is_not_flagged() {
        let diagnostics = check(
            "/// Does the work\nfn a() {}\n/// Has a value\nfn b() {}\n/// Is empty\nfn c() {}\n/// Tries again\nfn d() {}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn verb_that_noun_phrases_hid_is_flagged() {
        let source = "/// Load factor of the table\nfn a() {}\n/// Start offset\nfn b() {}\n/// Check the input\nfn c() {}";

        assert_eq!(flagged_words(source), ["Load", "Start", "Check"]);
    }

    #[test]
    fn unknown_verb_is_flagged() {
        assert_eq!(
            flagged_words("/// Frobnicate the widget\nfn frob() {}"),
            ["Frobnicate"]
        );
    }

    #[test]
    fn noun_opening_is_flagged() {
        assert_eq!(
            flagged_words("/// Source text the rule reads\nfn source() {}"),
            ["Source"]
        );
    }

    #[test]
    fn article_opening_is_flagged() {
        assert_eq!(
            flagged_words("/// The length of the string\nfn len() {}"),
            ["The"]
        );
    }

    #[test]
    fn modal_verb_is_flagged() {
        assert_eq!(
            flagged_words("/// Can be called twice\nfn twice() {}"),
            ["Can"]
        );
    }

    #[test]
    fn plural_noun_is_not_flagged() {
        let diagnostics = check("/// Bytes of the file\nfn bytes() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn lowercase_imperative_is_flagged() {
        assert_eq!(
            flagged_words("/// return the length\nfn len() {}"),
            ["return"]
        );
    }

    #[test]
    fn lowercase_third_person_is_not_flagged() {
        let diagnostics = check("/// returns the length\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn capital_s_ending_is_flagged() {
        assert_eq!(
            flagged_words("/// GPS position of the device\nfn position() {}"),
            ["GPS"]
        );
    }

    #[test]
    fn imperative_alone_is_flagged() {
        assert_eq!(flagged_words("/// Clear\nfn clear() {}"), ["Clear"]);
    }

    #[test]
    fn trailing_comma_is_cut_from_the_word() {
        let source = "/// Return, or panic\nfn a() {}\n/// Returns, or panics\nfn b() {}";

        assert_eq!(flagged_words(source), ["Return"]);
    }

    #[test]
    fn trailing_colon_is_cut_from_the_word() {
        let source = "/// Note: the length\nfn a() {}\n/// Panics: always\nfn b() {}";

        assert_eq!(flagged_words(source), ["Note"]);
    }

    #[test]
    fn trailing_semicolon_is_cut_from_the_word() {
        let source = "/// Stop; then wait\nfn a() {}\n/// Stops; then waits\nfn b() {}";

        assert_eq!(flagged_words(source), ["Stop"]);
    }

    #[test]
    fn trailing_period_is_cut_from_the_word() {
        let source = "/// Clear.\nfn a() {}\n/// Clears.\nfn b() {}";

        assert_eq!(flagged_words(source), ["Clear"]);
    }

    #[test]
    fn word_of_punctuation_only_is_flagged_whole() {
        assert_eq!(flagged_words("/// ... and more\nfn a() {}"), ["..."]);
    }

    #[test]
    fn hyphenated_word_is_flagged() {
        assert_eq!(
            flagged_words("/// Set-up for the test harness\nfn setup() {}"),
            ["Set-up"]
        );
    }

    #[test]
    fn hyphenated_word_in_s_is_not_flagged() {
        let diagnostics = check("/// Re-reads the file\nfn reread() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn multibyte_word_is_flagged() {
        let source = "/// Ünïcödé — 日本語 summary\nfn len() {}";

        assert_eq!(flagged_words(source), ["Ünïcödé"]);
    }

    #[test]
    fn adverb_before_third_person_is_not_flagged() {
        let diagnostics = check("/// Recursively collects the items\nfn collect() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn adverb_before_punctuated_third_person_is_not_flagged() {
        let diagnostics = check("/// Only returns, never panics\nfn get() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn adverb_before_imperative_is_flagged() {
        assert_eq!(
            flagged_words("/// Recursively collect the items\nfn collect() {}"),
            ["Recursively"]
        );
    }

    #[test]
    fn adverb_alone_is_flagged() {
        assert_eq!(
            flagged_words("/// Recursively\nfn collect() {}"),
            ["Recursively"]
        );
    }

    #[test]
    fn adverb_with_trailing_comma_is_not_flagged() {
        let diagnostics = check("/// Finally, returns the rest\nfn rest() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn verb_in_ly_before_plural_is_flagged() {
        let source = "/// Apply edits to the tree\nfn a() {}\n/// Supply defaults\nfn b() {}\n/// MULTIPLY values\nfn c() {}";

        assert_eq!(flagged_words(source), ["Apply", "Supply", "MULTIPLY"]);
    }

    #[test]
    fn ly_alone_is_not_an_adverb() {
        assert_eq!(flagged_words("/// Ly returns\nfn a() {}"), ["Ly"]);
    }

    #[test]
    fn ly_verb_list_is_sorted_lowercase_and_unique() {
        assert!(LY_VERBS.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(LY_VERBS.iter().all(|verb| *verb == verb.to_lowercase()));
        assert!(LY_VERBS.iter().all(|verb| verb.ends_with("ly")));
    }

    #[test]
    fn code_span_opening_is_flagged() {
        let diagnostics = check("/// `len` of the string\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .message_contains("opens with \"`len`\"")
            .has_span("<test>", 4, 9);
    }

    #[test]
    fn code_span_ending_in_s_is_flagged() {
        assert_eq!(
            flagged_words("/// `Returns` the length\nfn len() {}"),
            ["`Returns`"]
        );
    }

    #[test]
    fn code_span_with_spaces_is_flagged_whole() {
        assert_eq!(
            flagged_words("/// `impl Trait for T` blanket\nfn a() {}"),
            ["`impl Trait for T`"]
        );
    }

    #[test]
    fn double_backtick_code_span_is_flagged_whole() {
        assert_eq!(
            flagged_words("/// ``a ` b`` pair\nfn a() {}"),
            ["``a ` b``"]
        );
    }

    #[test]
    fn unclosed_code_span_is_flagged_to_the_first_space() {
        assert_eq!(flagged_words("/// `len of the string\nfn a() {}"), ["`len"]);
    }

    #[test]
    fn intra_doc_link_opening_is_flagged() {
        assert_eq!(
            flagged_words("/// [`Vec`] with the items\nfn a() {}"),
            ["[`Vec`]"]
        );
    }

    #[test]
    fn inline_link_opening_is_flagged_with_its_destination() {
        assert_eq!(
            flagged_words("/// [Docs](https://a.example/(x)) for it\nfn a() {}"),
            ["[Docs](https://a.example/(x))"]
        );
    }

    #[test]
    fn reference_link_opening_is_flagged_with_its_label() {
        assert_eq!(
            flagged_words("/// [Foo bar][foo] of the bar\nfn a() {}"),
            ["[Foo bar][foo]"]
        );
    }

    #[test]
    fn link_with_brackets_in_a_code_span_is_flagged_whole() {
        assert_eq!(
            flagged_words("/// [`Vec<[u8]>`] of bytes\nfn a() {}"),
            ["[`Vec<[u8]>`]"]
        );
    }

    #[test]
    fn link_with_nested_brackets_is_flagged_whole() {
        assert_eq!(flagged_words("/// [a [b] c] d\nfn a() {}"), ["[a [b] c]"]);
    }

    #[test]
    fn link_with_unclosed_backticks_is_flagged_whole() {
        assert_eq!(flagged_words("/// [a ` b] c\nfn a() {}"), ["[a ` b]"]);
    }

    #[test]
    fn link_with_unclosed_destination_is_flagged_to_its_text() {
        assert_eq!(
            flagged_words("/// [Docs](https://a of it\nfn a() {}"),
            ["[Docs]"]
        );
    }

    #[test]
    fn unclosed_link_is_flagged_to_the_first_space() {
        assert_eq!(flagged_words("/// [Foo of the bar\nfn a() {}"), ["[Foo"]);
    }

    #[test]
    fn autolink_opening_is_flagged() {
        assert_eq!(
            flagged_words("/// <https://example.com> mirror\nfn mirror() {}"),
            ["<https://example.com>"]
        );
    }

    #[test]
    fn code_span_alone_is_flagged() {
        assert_eq!(flagged_words("/// `len`\nfn len() {}"), ["`len`"]);
    }

    #[test]
    fn inline_triple_backtick_code_span_is_flagged() {
        assert_eq!(
            flagged_words("/// ```len``` of it\nfn len() {}"),
            ["```len```"]
        );
    }

    #[test]
    fn fence_with_info_string_is_not_flagged() {
        let diagnostics = check("/// ```rust\n/// let x = 1;\n/// ```\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn backtick_fence_opening_is_not_flagged() {
        let diagnostics = check("/// ```\n/// Return\n/// ```\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn tilde_fence_opening_is_not_flagged() {
        let diagnostics = check("/// ~~~\n/// Return\n/// ~~~\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn code_span_later_in_the_summary_is_flagged() {
        assert_eq!(
            flagged_words("/// Return `len` of the string\nfn len() {}"),
            ["Return"]
        );
    }

    #[test]
    fn associated_function_in_impl_is_flagged() {
        let diagnostics =
            check("impl Foo {\n    /// Create a new foo\n    pub fn new() -> Self {}\n}");

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0]).message_contains("opens with \"Create\"");
    }

    #[test]
    fn trait_method_with_body_is_flagged() {
        let diagnostics = check(
            "trait Len {\n    /// Return the length\n    fn len(&self) -> usize {\n        0\n    }\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_method_without_body_is_flagged() {
        let source = "trait Len {\n    /// Return the length\n    fn len(&self) -> usize;\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
        let start = source.find("Return").unwrap();
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + "Return".len());
    }

    #[test]
    fn extern_function_declaration_is_flagged() {
        let diagnostics =
            check("extern \"C\" {\n    /// Return the length\n    fn len() -> usize;\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn block_doc_comment_is_flagged() {
        let source = "/**\n * Return the length\n */\nfn len() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
        let start = source.find("Return").unwrap();
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + "Return".len());
    }

    #[test]
    fn indented_summary_span_starts_at_the_word() {
        let source = "///    Return the length\nfn len() {}";

        let diagnostics = check(source);

        let start = source.find("Return").unwrap();
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + "Return".len());
    }

    #[test]
    fn constant_is_not_flagged() {
        let diagnostics = check("/// Return code for a clean exit\nconst OK: i32 = 0;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_is_not_flagged() {
        let diagnostics = check("/// Set of names\npub struct Names;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn enum_variant_is_not_flagged() {
        let diagnostics = check("enum Mode {\n    /// Print the plan\n    Print,\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn test_function_is_not_flagged() {
        let diagnostics = check("/// Check the parser\n#[test]\nfn parses() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn test_attribute_above_the_doc_comment_is_not_flagged() {
        let diagnostics = check("#[test]\n/// Check the parser\nfn parses() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn scoped_test_attribute_is_not_flagged() {
        let diagnostics = check(
            "/// Check the client\n#[tokio::test(flavor = \"multi_thread\")]\nasync fn client() {}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn helper_in_cfg_test_module_is_not_flagged() {
        let diagnostics =
            check("#[cfg(test)]\nmod tests {\n    /// Build a fixture\n    fn fixture() {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn helper_in_nested_module_under_cfg_test_is_not_flagged() {
        let diagnostics = check(
            "#[cfg(test)]\nmod tests {\n    mod helpers {\n        /// Build a fixture\n        fn fixture() {}\n    }\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn helper_in_module_with_inner_cfg_test_is_not_flagged() {
        let diagnostics = check(
            "mod tests {\n    #![cfg(test)]\n    /// Build a fixture\n    fn fixture() {}\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn file_with_inner_cfg_test_is_not_flagged() {
        let diagnostics = check("#![cfg(test)]\n/// Build a fixture\nfn fixture() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_all_with_test_is_not_flagged() {
        let diagnostics = check("#[cfg(all(test, unix))]\n/// Build a fixture\nfn fixture() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn helper_inside_a_test_function_is_not_flagged() {
        let diagnostics =
            check("#[test]\nfn parses() {\n    /// Build a fixture\n    fn fixture() {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_in_impl_under_cfg_test_is_not_flagged() {
        let diagnostics = check(
            "#[cfg(test)]\nmod tests {\n    impl Foo {\n        /// Build a foo\n        fn build() {}\n    }\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_not_test_is_flagged() {
        let diagnostics = check("#[cfg(not(test))]\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_any_with_test_is_flagged() {
        let diagnostics = check("#[cfg(any(test, unix))]\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_feature_test_is_flagged() {
        let diagnostics = check("#[cfg(feature = \"test\")]\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_attr_test_is_flagged() {
        let diagnostics =
            check("#[cfg_attr(test, allow(dead_code))]\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn attribute_named_like_a_test_is_flagged() {
        let diagnostics = check("#[test_case(1)]\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_test_on_the_previous_item_is_flagged() {
        let diagnostics = check("#[cfg(test)]\nmod tests {}\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn test_target_paths_are_test_code() {
        assert!(is_test_target(Path::new("crates/core/tests/common/mod.rs")));
        assert!(is_test_target(Path::new("benches/throughput.rs")));
        assert!(!is_test_target(Path::new("crates/core/src/tests.rs")));
        assert!(!is_test_target(Path::new("crates/core/src/lib.rs")));
    }

    #[test]
    fn attribute_between_doc_and_function_is_flagged() {
        let diagnostics = check("/// Return the length\n#[must_use]\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn blank_doc_line_before_summary_is_skipped() {
        let diagnostics = check("///\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_attribute_after_doc_comment_is_flagged() {
        let diagnostics =
            check("/// Return the length\n#[doc = include_str!(\"len.md\")]\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn doc_attribute_before_doc_comment_is_not_flagged() {
        let diagnostics = check("#[doc = \"Summary\"]\n/// Return the length\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_attribute_alone_is_not_flagged() {
        let diagnostics = check("#[doc = \"Return the length\"]\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_attr_doc_before_doc_comment_is_not_flagged() {
        let diagnostics =
            check("#[cfg_attr(docsrs, doc = \"Summary\")]\n/// Return the length\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn doc_hidden_attribute_is_flagged() {
        let diagnostics = check("#[doc(hidden)]\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn first_doc_comment_holds_the_summary() {
        let diagnostics =
            check("/// Returns the length\n#[must_use]\n/// Return the rest\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn function_without_docs_is_not_flagged() {
        let diagnostics = check("fn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_doc_comment_is_not_flagged() {
        let diagnostics = check("///\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn imperative_after_the_first_line_is_not_flagged() {
        let diagnostics =
            check("/// Returns the length\n///\n/// Return early on empty input.\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_comment_above_is_not_read() {
        let diagnostics = check("//! Return the module\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_comment_ends_the_walk() {
        let diagnostics = check(
            "mod m {\n    //! Return the module\n    /// Returns the length\n    fn len() {}\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_comment_in_a_module_is_not_read() {
        let diagnostics = check("mod m {\n    //! Return the module\n    fn len() {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn plain_comment_between_doc_and_function_is_flagged() {
        let diagnostics = check("/// Return the length\n// note\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn plain_comment_is_not_flagged() {
        let diagnostics = check("// Return the length\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn previous_item_docs_are_not_read() {
        let diagnostics = check("/// Return the length\nstruct Foo;\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DocSummaryPerson>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DocSummaryPerson>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DocSummaryPerson>();
    }
}
