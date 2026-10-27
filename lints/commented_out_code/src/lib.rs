use tree_sitter::{Node, Parser};
use whisker_rust::{RuleId, RustLintPass, language};
use whisker_types::{DecoratedNode, Diagnostic, Severity, Span};

/// Node kinds that a prose comment does not produce by accident
///
/// A fragment that parses as Rust proves little on its own. English is full
/// of accidental Rust: `Same as above` is a cast and `ws = *wschar` is an
/// assignment. A bare expression with no semicolon therefore counts for
/// nothing. So does `if cc == 0 { r13 = r12 }`, the notation many comments
/// use for register arithmetic. Every kind listed here needs a keyword, an
/// attribute, or a macro `!`, and prose supplies none of the three.
const CODE_EVIDENCE: &[&str] = &[
    "async_block",
    "attribute_item",
    "const_block",
    "const_item",
    "enum_item",
    "extern_crate_declaration",
    "foreign_mod_item",
    "function_item",
    "function_signature_item",
    "gen_block",
    "impl_item",
    "inner_attribute_item",
    "let_declaration",
    "macro_definition",
    "macro_invocation",
    "mod_item",
    "static_item",
    "struct_item",
    "trait_item",
    "try_block",
    "type_item",
    "union_item",
    "unsafe_block",
    "use_declaration",
];

/// The grade of a comment body
///
/// Only [`Fragment::Code`] produces a diagnostic. The split between the
/// other two matters during the walk: [`Fragment::Prose`] vetoes the whole
/// fragment, and [`Fragment::Ambiguous`] leaves the verdict to its parent.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum Fragment {
    /// The body parses and holds a construct that prose does not produce
    Code,
    /// The body parses, but nothing in it tells code and prose apart
    Ambiguous,
    /// The body is empty, fails to parse, or spaces `.` or `::` like prose
    Prose,
}

/// Flags comments that hold commented-out code
///
/// The rule strips the comment markers and parses the body inside a probe
/// function. The body is code when it holds one of the `CODE_EVIDENCE`
/// kinds or a statement that a semicolon closes. Such a comment is dead
/// source that version control already keeps.
///
/// The rule joins consecutive comment lines that each start their own
/// line, so it judges a commented-out function whole rather than line by
/// line.
///
/// The rule never reports a doc comment. A `# Examples` section is code by
/// design, and the skip costs nothing: nobody comments code out with
/// `///`.
///
/// The rule reports only fragments that are valid inside a function body.
/// It skips a commented-out match arm, struct field, or trait method
/// signature. The wrappers that accept those also accept short English
/// phrases such as `safety: unchecked`.
///
/// # Examples
///
/// ```
/// use commented_out_code::CommentedOutCode;
/// use whisker_rust::RustLintPassAdapter;
///
/// let pass = RustLintPassAdapter::new(CommentedOutCode::new());
/// ```
pub struct CommentedOutCode {
    parser: Parser,
}

impl CommentedOutCode {
    /// Creates the rule and the parser it probes comment bodies with
    ///
    /// # Panics
    ///
    /// Panics if the Rust grammar does not load.
    ///
    /// # Examples
    ///
    /// ```
    /// use commented_out_code::CommentedOutCode;
    ///
    /// let rule = CommentedOutCode::new();
    /// ```
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&language())
            .expect("the bundled Rust grammar should load");
        Self { parser }
    }

    /// Returns a diagnostic for `span` when `body` grades as [`Fragment::Code`]
    fn report(&mut self, body: &str, span: Span) -> Vec<Diagnostic> {
        match classify(&mut self.parser, body) {
            Fragment::Code => vec![
                Diagnostic::builder()
                    .rule_id(RuleId::new("lint.commented-out-code"))
                    .severity(Severity::Warn)
                    .message("remove this commented-out code; version control keeps the history")
                    .span(span)
                    .build(),
            ],
            Fragment::Ambiguous => Vec::new(),
            Fragment::Prose => Vec::new(),
        }
    }
}

impl Default for CommentedOutCode {
    fn default() -> Self {
        Self::new()
    }
}

impl RustLintPass for CommentedOutCode {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(body) = block_comment_body(node.text()) else {
            return Vec::new();
        };

        self.report(&body, node.span())
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(body) = line_comment_body(node.text()) else {
            return Vec::new();
        };

        if !starts_its_own_line(node) {
            return self.report(body, node.span());
        }

        let Some(group) = group_starting_at(node) else {
            return Vec::new();
        };

        let body = group
            .iter()
            .filter_map(|comment| line_comment_body(comment.text()))
            .collect::<Vec<_>>()
            .join("\n");
        let last = group.last().unwrap_or(node);
        let start = node.span();
        let span = Span::builder()
            .file(start.file_path().clone())
            .start(start.start())
            .end(last.span().end())
            .build();

        self.report(&body, span)
    }
}

/// Returns the text of a `//` comment without its marker
///
/// Returns [`None`] for `///` and `//!` doc comments. Four or more slashes
/// are an ordinary comment again, which matches how rustc lexes them.
fn line_comment_body(text: &str) -> Option<&str> {
    let body = text.strip_prefix("//")?;

    match body.as_bytes().first() {
        Some(b'!') => None,
        Some(b'/') => match body.as_bytes().get(1) {
            Some(b'/') => Some(body),
            None | Some(_) => None,
        },
        None | Some(_) => Some(body),
    }
}

/// Returns the text of a `/* */` comment without its markers
///
/// Returns [`None`] for `/**` and `/*!` doc comments. A body whose
/// non-blank continuation lines all open with `*` loses that decoration,
/// so the classic boxed comment reaches the parser as plain source.
fn block_comment_body(text: &str) -> Option<String> {
    let body = text.strip_prefix("/*")?;

    let is_doc = match body.as_bytes().first() {
        Some(b'!') => true,
        Some(b'*') => match body.as_bytes().get(1) {
            None | Some(b'*') | Some(b'/') => false,
            Some(_) => true,
        },
        None | Some(_) => false,
    };
    if is_doc {
        return None;
    }

    let body = body.strip_suffix("*/").unwrap_or(body);
    Some(strip_star_margin(body))
}

/// Removes the `*` margin, and the indentation before it, from a block comment
///
/// The margin comes off only when every non-blank continuation line has
/// one. A body that mixes the styles keeps its text unchanged.
fn strip_star_margin(body: &str) -> String {
    let mut lines = body.lines();
    let Some(first) = lines.next() else {
        return String::new();
    };

    let rest: Vec<&str> = lines.collect();
    let has_margin = rest
        .iter()
        .filter(|line| !line.trim().is_empty())
        .all(|line| line.trim_start().starts_with('*'));
    if rest.is_empty() || !has_margin {
        return body.to_string();
    }

    let mut result = String::from(first);
    for line in rest {
        result.push('\n');
        let line = line.trim_start();
        result.push_str(line.strip_prefix('*').unwrap_or(line));
    }
    result
}

/// Returns whether only whitespace comes before `node` on its line
///
/// The rule judges a trailing comment by itself. Joining it with the
/// comment below would splice a prose remark onto a statement, so the
/// pair would fail to parse and the commented-out code on the lower line
/// would go unreported.
fn starts_its_own_line(node: &DecoratedNode<'_>) -> bool {
    match node.raw().prev_sibling() {
        Some(previous) => previous.end_position().row < node.raw().start_position().row,
        None => true,
    }
}

/// Returns the run of comment lines that `node` opens
///
/// Returns [`None`] when the comment on the line above belongs to the same
/// run, so that one run yields one diagnostic.
fn group_starting_at<'a>(node: &DecoratedNode<'a>) -> Option<Vec<DecoratedNode<'a>>> {
    let siblings = match node.parent() {
        Some(parent) => parent.named_children(),
        None => return Some(vec![*node]),
    };
    let index = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())?;

    if index > 0 && continues_group(&siblings[index - 1], node) {
        return None;
    }

    let mut group = vec![*node];
    for sibling in &siblings[index + 1..] {
        let last = group.last().expect("the group always holds the first node");
        if !continues_group(last, sibling) {
            break;
        }
        group.push(*sibling);
    }

    Some(group)
}

/// Returns whether `next` extends the comment run that ends at `previous`
fn continues_group(previous: &DecoratedNode<'_>, next: &DecoratedNode<'_>) -> bool {
    if next.kind() != "line_comment" || previous.kind() != "line_comment" {
        return false;
    }
    if line_comment_body(next.text()).is_none() || line_comment_body(previous.text()).is_none() {
        return false;
    }
    if !starts_its_own_line(previous) || !starts_its_own_line(next) {
        return false;
    }

    previous.raw().end_position().row + 1 == next.raw().start_position().row
}

/// Grades a comment body as code, ambiguous, or prose
///
/// The function parses the body inside a probe function, so the body may
/// hold either items or statements. It grades the probe's body block and
/// not the root, so the `function_item` the wrapper adds never counts as
/// evidence.
fn classify(parser: &mut Parser, body: &str) -> Fragment {
    let body = body.trim();
    if body.is_empty() {
        return Fragment::Prose;
    }

    let source = format!("fn __whisker_probe() {{\n{body}\n}}\n");
    let Some(tree) = parser.parse(&source, None) else {
        return Fragment::Prose;
    };
    let root = tree.root_node();
    if root.has_error() {
        return Fragment::Prose;
    }
    let Some(probe) = root.named_child(0) else {
        return Fragment::Prose;
    };
    let Some(block) = probe.child_by_field_name("body") else {
        return Fragment::Prose;
    };

    classify_fragment(&source, block)
}

/// Grades one subtree of the probe
///
/// Prose wins over code, so a body that pairs a call with prose spacing
/// stays quiet.
fn classify_fragment(source: &str, node: Node<'_>) -> Fragment {
    if has_loose_path_punctuation(source, node) {
        return Fragment::Prose;
    }

    let mut result = match CODE_EVIDENCE.contains(&node.kind()) || is_terminated_statement(node) {
        true => Fragment::Code,
        false => Fragment::Ambiguous,
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        result = match classify_fragment(source, child) {
            Fragment::Prose => return Fragment::Prose,
            Fragment::Code => Fragment::Code,
            Fragment::Ambiguous => result,
        };
    }

    result
}

/// Returns whether whitespace follows a `.` or `::` token
///
/// Rust code almost never puts a space there, but English does:
/// `e.g. foo()` parses as a call to `foo` on the field `e.g`. That space
/// is the only signal that tells the two apart.
fn has_loose_path_punctuation(source: &str, node: Node<'_>) -> bool {
    match node.kind() {
        "." | "::" => source[node.end_byte()..].starts_with(char::is_whitespace),
        _ => false,
    }
}

/// Returns whether `node` is an expression closed by a semicolon
///
/// A trailing semicolon is evidence on its own. It survives a copy-paste of
/// real code and almost never ends a sentence that also parses as Rust.
fn is_terminated_statement(node: Node<'_>) -> bool {
    if node.kind() != "expression_statement" {
        return false;
    }

    let mut cursor = node.walk();
    match node.children(&mut cursor).last() {
        Some(last) => last.kind() == ";",
        None => false,
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for CommentedOutCode {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.commented-out-code")]
    }
}

whisker_plugin::export_lints![rust: [CommentedOutCode::new()]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(CommentedOutCode::new()))]
    }

    fn probe_parser() -> Parser {
        let mut parser = Parser::new();
        parser
            .set_language(&language())
            .expect("the bundled Rust grammar should load");
        parser
    }

    fn check(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);

        execute(&tree, &mut passes())
    }

    #[test]
    fn abbreviation_before_a_call_is_not_flagged() {
        let diagnostics = check("// e.g. foo();\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn blank_line_splits_a_comment_run() {
        let diagnostics = check("// let x = 1;\n\n// let y = 2;\nfn f() {}");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn block_comment_body_with_empty_comment_returns_empty_body() {
        let body = block_comment_body("/**/").expect("an empty comment is not a doc comment");

        assert_eq!(body, "");
    }

    #[test]
    fn block_comment_body_with_inner_doc_returns_none() {
        assert!(block_comment_body("/*! let x = 1; */").is_none());
    }

    #[test]
    fn block_comment_body_with_outer_doc_returns_none() {
        assert!(block_comment_body("/** let x = 1; */").is_none());
    }

    #[test]
    fn block_comment_body_with_star_margin_drops_the_margin() {
        let body = block_comment_body("/*\n * let x = 1;\n */").expect("this is not a doc comment");

        assert_eq!(body, "\n let x = 1;\n");
    }

    #[test]
    fn block_comment_with_code_is_flagged() {
        let diagnostics = check("/* let x = 1; */\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn block_comment_with_prose_is_not_flagged() {
        let diagnostics = check("/* This function handles user input */\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn block_doc_comment_with_code_is_not_flagged() {
        let diagnostics = check("/** let x = 1; */\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn boxed_block_comment_with_code_is_flagged() {
        let diagnostics = check("/*\n * let x = 1;\n */\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn classify_with_bare_call_returns_ambiguous() {
        let fragment = classify(&mut probe_parser(), "foo()");

        assert_eq!(fragment, Fragment::Ambiguous);
    }

    #[test]
    fn classify_with_empty_body_returns_prose() {
        let fragment = classify(&mut probe_parser(), "   ");

        assert_eq!(fragment, Fragment::Prose);
    }

    #[test]
    fn classify_with_item_returns_code() {
        let fragment = classify(&mut probe_parser(), "fn old_way() {}");

        assert_eq!(fragment, Fragment::Code);
    }

    #[test]
    fn classify_with_loose_dot_returns_prose() {
        let fragment = classify(&mut probe_parser(), "e.g. foo();");

        assert_eq!(fragment, Fragment::Prose);
    }

    #[test]
    fn classify_with_unparsable_body_returns_prose() {
        let fragment = classify(&mut probe_parser(), "This function handles user input");

        assert_eq!(fragment, Fragment::Prose);
    }

    #[test]
    fn commented_out_attribute_is_flagged() {
        let diagnostics = check("// #[derive(Debug)]\nstruct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn commented_out_function_is_flagged_as_a_commented_out_code_warning() {
        let diagnostics = check("// fn old_way() { }\nfn f() {}");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.commented-out-code")
            .has_severity(Severity::Warn)
            .message_contains("commented-out code");
    }

    #[test]
    fn commented_out_function_is_flagged_once() {
        let diagnostics = check("// fn old_way() { }\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn commented_out_let_binding_is_flagged() {
        let diagnostics = check("fn f() {\n    // let x = foo.bar();\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn commented_out_macro_call_is_flagged() {
        let diagnostics = check("fn f() {\n    // println!(\"{x}\");\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn commented_out_match_arm_is_not_flagged() {
        let diagnostics = check("fn f() {\n    match x {\n        // Foo::Bar => baz(),\n    }\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn commented_out_use_declaration_is_flagged() {
        let diagnostics = check("// use std::io::Read;\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn consecutive_comment_lines_report_once() {
        let diagnostics = check("// fn old() {\n//     let x = 1;\n// }\nfn f() {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn consecutive_comment_lines_report_one_span_across_all_lines() {
        let diagnostics = check("// fn old() {\n//     let x = 1;\n// }\nfn f() {}");

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 0, 36);
    }

    #[test]
    fn doc_comment_with_a_code_example_is_not_flagged() {
        let diagnostics = check("/// ```\n/// let x = 1;\n/// ```\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_comment_is_not_flagged() {
        let diagnostics = check("//\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_doc_comment_with_code_is_not_flagged() {
        let diagnostics = check("//! let x = 1;\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn line_comment_body_with_four_slashes_returns_the_body() {
        let body =
            line_comment_body("//// let x = 1;").expect("four slashes are not a doc comment");

        assert_eq!(body, "// let x = 1;");
    }

    #[test]
    fn line_comment_body_with_inner_doc_returns_none() {
        assert!(line_comment_body("//! let x = 1;").is_none());
    }

    #[test]
    fn line_comment_body_with_outer_doc_returns_none() {
        assert!(line_comment_body("/// let x = 1;").is_none());
    }

    #[test]
    fn line_comment_body_without_a_marker_returns_none() {
        assert!(line_comment_body("let x = 1;").is_none());
    }

    #[test]
    fn prose_ending_in_a_colon_is_not_flagged() {
        let diagnostics = check("// The rules are as follows:\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn prose_paragraph_is_not_flagged() {
        let diagnostics = check(
            "// This function handles user input\n// and returns the parsed value.\nfn f() {}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn prose_that_parses_as_a_cast_is_not_flagged() {
        let diagnostics = check("// Same as above\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn prose_with_a_field_access_is_not_flagged() {
        let diagnostics = check("// WebAssembly.Module\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn register_notation_is_not_flagged() {
        let diagnostics = check("// if cc == 0 { r13 = r12 }\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn separator_comment_is_not_flagged() {
        let diagnostics = check("// ---------------\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn single_identifier_is_not_flagged() {
        let diagnostics = check("// unused\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn terminated_statement_is_flagged() {
        let diagnostics = check("fn f() {\n    // return;\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn todo_comment_is_not_flagged() {
        let diagnostics = check("// TODO: rewrite this\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trailing_comment_does_not_join_the_run_below_it() {
        let diagnostics = check("fn f() {\n    let x = 1; // keep this\n    // let y = 2;\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trailing_commented_out_code_is_flagged() {
        let diagnostics = check("fn f() {\n    let x = 1; // foo();\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<CommentedOutCode>();
    }

    #[test]
    fn trait_send_fragment() {
        fn assert_send<T: Send>() {}
        assert_send::<Fragment>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<CommentedOutCode>();
    }

    #[test]
    fn trait_sync_fragment() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Fragment>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<CommentedOutCode>();
    }

    #[test]
    fn trait_unpin_fragment() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Fragment>();
    }

    #[test]
    fn url_comment_is_not_flagged() {
        let diagnostics = check("// See https://example.com/foo\nfn f() {}");

        assert_no_diagnostics(&diagnostics);
    }

    mod prop {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn check_of_a_block_comment_never_panics(text in "\\PC{0,80}") {
                let source = format!("/* {text} */\nfn f() {{}}\n");

                let _diagnostics = check(&source);
            }

            #[test]
            fn check_of_a_line_comment_never_panics(text in "\\PC{0,80}") {
                let source = format!("// {text}\nfn f() {{}}\n");

                let _diagnostics = check(&source);
            }
        }
    }
}
