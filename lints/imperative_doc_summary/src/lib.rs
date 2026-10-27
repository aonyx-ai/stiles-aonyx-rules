use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity, Span};

const RULE_ID: RuleId = RuleId::new("lint.imperative-doc-summary");

/// The imperatives this rule knows, in alphabetical order, each with the
/// third-person form the convention asks for
///
/// Every verb here opens real doc comments on functions in the bare form.
/// A verb whose bare form also heads a noun phrase in an accessor's summary
/// is left out, because the rule must never report a noun. "Check" names a
/// CI check and "Pin" a revision pin. "Report" and "Record" name values that
/// a function returns. "Load factor", "Start offset", "Visit count", "Reset
/// token", "Compile errors", and "Close code" are noun phrases too. "Test"
/// heads "Test vectors", and "Map" heads "Map from keys to values".
///
/// "Set" stays. It is one of the most common verbs to open a function
/// summary, and the check for "of" catches its noun reading, "Set of names".
const VERBS: &[(&str, &str)] = &[
    ("Add", "Adds"),
    ("Allocate", "Allocates"),
    ("Append", "Appends"),
    ("Apply", "Applies"),
    ("Ask", "Asks"),
    ("Assert", "Asserts"),
    ("Attach", "Attaches"),
    ("Calculate", "Calculates"),
    ("Classify", "Classifies"),
    ("Clear", "Clears"),
    ("Collect", "Collects"),
    ("Combine", "Combines"),
    ("Compare", "Compares"),
    ("Compute", "Computes"),
    ("Configure", "Configures"),
    ("Construct", "Constructs"),
    ("Consume", "Consumes"),
    ("Convert", "Converts"),
    ("Create", "Creates"),
    ("Decide", "Decides"),
    ("Decode", "Decodes"),
    ("Define", "Defines"),
    ("Delete", "Deletes"),
    ("Deserialize", "Deserializes"),
    ("Detect", "Detects"),
    ("Determine", "Determines"),
    ("Disable", "Disables"),
    ("Emit", "Emits"),
    ("Enable", "Enables"),
    ("Encode", "Encodes"),
    ("Ensure", "Ensures"),
    ("Execute", "Executes"),
    ("Extend", "Extends"),
    ("Extract", "Extracts"),
    ("Find", "Finds"),
    ("Flatten", "Flattens"),
    ("Fold", "Folds"),
    ("Generate", "Generates"),
    ("Get", "Gets"),
    ("Initialize", "Initializes"),
    ("Insert", "Inserts"),
    ("Inspect", "Inspects"),
    ("Iterate", "Iterates"),
    ("Look", "Looks"),
    ("Make", "Makes"),
    ("Obtain", "Obtains"),
    ("Parse", "Parses"),
    ("Perform", "Performs"),
    ("Pop", "Pops"),
    ("Print", "Prints"),
    ("Produce", "Produces"),
    ("Provide", "Provides"),
    ("Read", "Reads"),
    ("Remove", "Removes"),
    ("Replace", "Replaces"),
    ("Resolve", "Resolves"),
    ("Retrieve", "Retrieves"),
    ("Return", "Returns"),
    ("Send", "Sends"),
    ("Serialize", "Serializes"),
    ("Set", "Sets"),
    ("Split", "Splits"),
    ("Strip", "Strips"),
    ("Take", "Takes"),
    ("Try", "Tries"),
    ("Unpack", "Unpacks"),
    ("Validate", "Validates"),
    ("Verify", "Verifies"),
    ("Walk", "Walks"),
    ("Wrap", "Wraps"),
    ("Write", "Writes"),
];

/// How many words after the verb the rule reads for an "of"
///
/// "Set of", "Return type of", and "Read half of" are noun phrases. A verb
/// almost never has "of" as its first or second word after it, so an "of"
/// there makes the rule stay silent.
const NOUN_PHRASE_WINDOW: usize = 2;

/// Words that turn a verb before them into a noun phrase
///
/// "Return type", "Return code", "Parse tree", and "Read timeout" name
/// values, and an accessor's summary opens with them. A verb in the list
/// followed by one of these words makes the rule stay silent.
const NOUN_HEADS: &[&str] = &["address", "code", "timeout", "tree", "type"];

/// Flags a function doc summary that opens with a bare imperative
///
/// The convention is a third-person singular summary line: "Returns the
/// length", not "Return the length". The rule cannot conjugate, and it does
/// not try. It knows a fixed list of capitalized imperatives that open real
/// doc comments, and it reports only those. A summary that opens with a
/// noun, an article, a code span, or an unknown verb is left alone.
///
/// The rule reads functions and methods only, including trait methods with
/// no body. A struct or a constant is documented with a noun phrase, and
/// many nouns share a spelling with a verb.
///
/// The summary is the first line of text in the item's first outer doc
/// comment. When a `#[doc = ...]` attribute comes before that comment, the
/// summary is text the rule cannot read, so it stays silent.
pub struct ImperativeDocSummary;

impl RustLintPass for ImperativeDocSummary {
    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report(node)
    }

    fn check_function_signature_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        report(node)
    }
}

/// A verb in the imperative and in the third person
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct Verb {
    imperative: &'static str,
    third_person: &'static str,
}

impl Verb {
    /// Returns the verb whose imperative is `word`, if the list has one
    fn find(word: &str) -> Option<Self> {
        let index = VERBS
            .binary_search_by(|&(imperative, _)| imperative.cmp(word))
            .ok()?;
        let (imperative, third_person) = VERBS[index];

        Some(Self {
            imperative,
            third_person,
        })
    }
}

/// Reports the summary of a function's doc comment if it opens with a known
/// imperative
fn report(node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
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
    let Some(Verb {
        imperative,
        third_person,
    }) = opening_verb(text)
    else {
        return Vec::new();
    };

    let start = summary.offset() + summary.text().len() - text.len();
    let file = node.span();
    vec![Diagnostic::new(
        RULE_ID,
        Severity::Warn,
        format!(
            "doc summary opens with the imperative `{imperative}`, not the third-person `{third_person}`"
        ),
        Span::new(file.file_path().clone(), start, start + imperative.len()),
    )]
}

/// Returns the verb that `summary` opens with, if it is a known imperative
/// used as a verb
///
/// The first word ends at whitespace or at the end of the line. A word that
/// runs into punctuation, as in "Set-up" or "Read's", is not a bare verb.
/// An "of" in the next two words, or a noun head as the next word, makes
/// the opening a noun phrase.
fn opening_verb(summary: &str) -> Option<Verb> {
    let mut words = summary.split_whitespace();
    let first = words.next()?;
    let verb = Verb::find(first)?;

    let following: Vec<&str> = words.take(NOUN_PHRASE_WINDOW).collect();
    if following.contains(&"of") {
        return None;
    }
    if following
        .first()
        .is_some_and(|word| NOUN_HEADS.contains(word))
    {
        return None;
    }

    Some(verb)
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

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for ImperativeDocSummary {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

whisker_plugin::export_lints![rust: [ImperativeDocSummary]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn check(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(ImperativeDocSummary))];

        execute(&tree, &mut passes)
    }

    #[test]
    fn article_opening_is_not_flagged() {
        let diagnostics = check("/// The length of the string\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn associated_function_in_impl_is_flagged() {
        let diagnostics =
            check("impl Foo {\n    /// Create a new foo\n    pub fn new() -> Self {}\n}");

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .message_contains("`Create`, not the third-person `Creates`");
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
    fn block_doc_comment_is_flagged() {
        let source = "/**\n * Return the length\n */\nfn len() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
        let start = source.find("Return").unwrap();
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + "Return".len());
    }

    #[test]
    fn code_span_opening_is_not_flagged() {
        let diagnostics = check("/// `Return` the length\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn constant_is_not_flagged() {
        let diagnostics = check("/// Return code for a clean exit\nconst OK: i32 = 0;");

        assert_no_diagnostics(&diagnostics);
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
    fn doc_hidden_attribute_is_flagged() {
        let diagnostics = check("#[doc(hidden)]\n/// Return the length\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn enum_variant_is_not_flagged() {
        let diagnostics = check("enum Mode {\n    /// Print the plan\n    Print,\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn extern_function_declaration_is_flagged() {
        let diagnostics =
            check("extern \"C\" {\n    /// Return the length\n    fn len() -> usize;\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn fenced_code_opening_is_not_flagged() {
        let diagnostics = check("/// ```\n/// Return\n/// ```\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
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
    fn imperative_after_the_first_line_is_not_flagged() {
        let diagnostics =
            check("/// Returns the length\n///\n/// Return early on empty input.\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn imperative_alone_is_flagged() {
        let diagnostics = check("/// Clear\nfn clear() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn imperative_is_flagged() {
        let source = "/// Return the length of the string\nfn len() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.imperative-doc-summary")
            .has_severity(Severity::Warn)
            .message_contains(
                "doc summary opens with the imperative `Return`, not the third-person `Returns`",
            )
            .has_span("<test>", 4, 10);
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
    fn lowercase_imperative_is_not_flagged() {
        let diagnostics = check("/// return the length\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn multibyte_summary_is_not_flagged() {
        let diagnostics = check("/// Ünïcödé — 日本語 summary\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn noun_head_after_an_article_is_flagged() {
        let diagnostics = check("/// Return the type the provider resolved\nfn ty() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn noun_head_after_the_verb_is_not_flagged() {
        let diagnostics = check("/// Parse tree the provider built\nfn tree() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn noun_opening_is_not_flagged() {
        let diagnostics = check("/// Source text the rule reads\nfn source() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn noun_phrase_with_of_after_one_word_is_not_flagged() {
        let diagnostics = check("/// Return type of the function\nfn return_type() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn noun_phrase_with_of_right_after_is_not_flagged() {
        let diagnostics = check("/// Set of names the rule skips\nfn names() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn of_after_the_window_is_flagged() {
        let diagnostics = check("/// Return the length of the string\nfn len() {}");

        assert_eq!(diagnostics.len(), 1);
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
    fn struct_is_not_flagged() {
        let diagnostics = check("/// Set of names\npub struct Names;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn third_person_is_not_flagged() {
        let diagnostics = check("/// Returns the length of the string\nfn len() {}");

        assert_no_diagnostics(&diagnostics);
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
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ImperativeDocSummary>();
        assert_send::<Verb>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ImperativeDocSummary>();
        assert_sync::<Verb>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ImperativeDocSummary>();
        assert_unpin::<Verb>();
    }

    #[test]
    fn unknown_verb_is_not_flagged() {
        let diagnostics = check("/// Frobnicate the widget\nfn frob() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn verb_list_is_sorted_and_unique() {
        let imperatives: Vec<&str> = VERBS.iter().map(|&(imperative, _)| imperative).collect();

        assert!(imperatives.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn verb_list_leaves_out_noun_readings() {
        let risky = [
            "Check", "Close", "Compile", "Count", "Load", "Map", "Match", "Pin", "Record",
            "Report", "Reset", "Start", "Test", "Visit",
        ];

        let listed: Vec<&str> = risky
            .into_iter()
            .filter(|word| Verb::find(word).is_some())
            .collect();

        assert!(listed.is_empty(), "{listed:?}");
    }

    #[test]
    fn verb_list_third_person_forms_end_in_s() {
        let unconjugated: Vec<&str> = VERBS
            .iter()
            .filter(|&&(imperative, third_person)| {
                !third_person.ends_with('s') || third_person == imperative
            })
            .map(|&(imperative, _)| imperative)
            .collect();

        assert!(unconjugated.is_empty(), "{unconjugated:?}");
    }

    #[test]
    fn word_joined_to_punctuation_is_not_flagged() {
        let diagnostics = check("/// Set-up for the test harness\nfn setup() {}");

        assert_no_diagnostics(&diagnostics);
    }
}
