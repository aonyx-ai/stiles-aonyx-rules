use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity, Span};
use stiles_typescript::TypeScriptLintPass;

const RULE_ID: RuleId = RuleId::new("lint.em-dash-aside");

/// The byte that stands in for text the lint must not read as prose
///
/// It is neither a dash nor whitespace, so masked text still counts as
/// content on either side of a dash.
const FILLER: u8 = b'x';

const EM_DASH: &[u8] = "—".as_bytes();
const EN_DASH: &[u8] = "–".as_bytes();

/// Flags a doc comment that interrupts a sentence with a dash aside
///
/// An em dash, a spaced en dash, or a spaced hyphen splices a second
/// thought into a sentence. A colon, a period, or a subordinate clause
/// carries the same meaning without the splice. The lint reads prose
/// only. Code spans, code blocks, quotations, headings, tables, link
/// definitions, and list markers are exempt.
///
/// In TypeScript the lint reads a `/** */` doc comment. The hyphen a block
/// tag writes between a name and its description, as in `@param a - the
/// first`, is syntax rather than an aside, and an `@example` section is
/// code, so neither is read.
pub struct EmDashAside;

impl RustLintPass for EmDashAside {
    fn check_block_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        diagnose(doc_comment::rust(node), node)
    }

    fn check_line_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        diagnose(doc_comment::rust(node), node)
    }
}

impl TypeScriptLintPass for EmDashAside {
    fn check_comment(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        diagnose(doc_comment::typescript(node), node)
    }
}

/// The dash a writer used to interrupt a sentence
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum AsideKind {
    EmDash,
    EnDash,
    SpacedHyphen,
}

impl AsideKind {
    /// Names the dash for the diagnostic message
    fn dash(self) -> &'static str {
        match self {
            AsideKind::EmDash => "em dash",
            AsideKind::EnDash => "en dash",
            AsideKind::SpacedHyphen => "spaced hyphen",
        }
    }
}

/// One line of Markdown taken from a doc comment
///
/// The offset locates the first byte of `text` in the file, so a finding
/// inside the line converts straight into a [`Span`].
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct DocLine<'a> {
    text: &'a str,
    offset: usize,
}

/// A dash aside, as a byte range in the file
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct Aside {
    kind: AsideKind,
    offset: usize,
    length: usize,
}

/// Turns the asides found in one doc comment into diagnostics
fn diagnose(
    lines: Option<Vec<doc_comment::DocLine<'_>>>,
    node: &DecoratedNode<'_>,
) -> Vec<Diagnostic> {
    let Some(lines) = lines else {
        return Vec::new();
    };
    let lines: Vec<DocLine<'_>> = lines
        .into_iter()
        .map(|line| DocLine {
            text: line.text(),
            offset: line.offset(),
        })
        .collect();
    let span = node.span();
    let file = span.file_path().clone();

    find_asides(&lines)
        .into_iter()
        .map(|aside| {
            let Aside {
                kind,
                offset,
                length,
            } = aside;
            let dash = kind.dash();

            Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Warn)
                .message(format!(
                    "{dash} interrupts the sentence; use a colon, a period, or a subordinate clause"
                ))
                .span(
                    Span::builder()
                        .file(file.clone())
                        .start(offset)
                        .end(offset + length)
                        .build(),
                )
                .build()
        })
        .collect()
}

/// Returns the prose lines of a doc block, with list markers removed
///
/// The result keeps one slot per input line and holds [`None`] where a
/// line is not prose. The caller splits on those gaps to recover the
/// paragraphs, because a dash is only an aside when prose surrounds it.
fn prose_lines<'a>(lines: &[DocLine<'a>]) -> Vec<Option<DocLine<'a>>> {
    let mut prose = Vec::with_capacity(lines.len());
    let mut fence: Option<&str> = None;
    let mut indented_code = false;

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.text.trim_start();

        if let Some(open) = fence {
            if closes_fence(trimmed, open) {
                fence = None;
            }
            prose.push(None);
            continue;
        }

        if trimmed.is_empty() {
            indented_code = false;
            prose.push(None);
            continue;
        }

        let indent = indent_width(line.text);
        let after_blank = index == 0 || lines[index - 1].text.trim().is_empty();

        if indented_code && indent >= 4 {
            prose.push(None);
            continue;
        }
        indented_code = false;

        if after_blank && indent >= 4 {
            indented_code = true;
            prose.push(None);
            continue;
        }

        if let Some(run) = fence_run(trimmed) {
            fence = Some(run);
            prose.push(None);
            continue;
        }

        if is_heading(trimmed)
            || is_rule(trimmed)
            || is_table_row(trimmed)
            || is_link_definition(trimmed)
        {
            prose.push(None);
            continue;
        }

        prose.push(Some(strip_marker(*line)));
    }

    prose
}

/// Returns the run of backticks or tildes that opens a code fence
fn fence_run(trimmed: &str) -> Option<&str> {
    for marker in ['`', '~'] {
        let width = trimmed.len() - trimmed.trim_start_matches(marker).len();
        if width >= 3 {
            return Some(&trimmed[..width]);
        }
    }

    None
}

/// Returns whether a line closes the fence that `open` started
fn closes_fence(trimmed: &str, open: &str) -> bool {
    let Some(marker) = open.chars().next() else {
        return false;
    };

    let width = trimmed.len() - trimmed.trim_start_matches(marker).len();
    width >= open.len() && trimmed[width..].trim().is_empty()
}

/// Returns how far a line is indented, where a tab is four columns
fn indent_width(text: &str) -> usize {
    let mut width = 0;
    for character in text.chars() {
        if character == ' ' {
            width += 1;
            continue;
        }
        if character == '\t' {
            width += 4;
            continue;
        }
        break;
    }

    width
}

/// Returns whether a line is an ATX heading, such as `# Errors`
///
/// CommonMark accepts a tab after the hashes as readily as a space, so
/// a heading written with one still has to be exempt from the scan.
fn is_heading(trimmed: &str) -> bool {
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return false;
    }

    let rest = &trimmed[hashes..];
    rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')
}

/// Returns whether a line is a horizontal rule or a heading underline
///
/// A row of hyphens under a paragraph would otherwise look to the scan
/// like a spaced hyphen in the middle of a sentence.
fn is_rule(trimmed: &str) -> bool {
    let mut characters = trimmed.chars().filter(|c| !c.is_whitespace());
    let Some(first) = characters.next() else {
        return false;
    };
    if !"-=*_".contains(first) {
        return false;
    }

    characters.all(|character| character == first)
}

/// Returns whether a line is a row of a Markdown table
///
/// A cell holds a fragment rather than a sentence, and the delimiter row
/// is built from dashes.
fn is_table_row(trimmed: &str) -> bool {
    if trimmed.starts_with('|') {
        return true;
    }

    trimmed.contains('|') && trimmed.chars().all(|character| "-:| ".contains(character))
}

/// Returns whether a line defines a reference-style Markdown link
fn is_link_definition(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix('[') else {
        return false;
    };
    let Some(close) = rest.find(']') else {
        return false;
    };

    rest[close + 1..].starts_with(':')
}

/// Removes a list marker from the start of a line
///
/// A dash that opens a list item is punctuation, not an aside, so the
/// scan never sees it.
fn strip_marker(line: DocLine<'_>) -> DocLine<'_> {
    let trimmed = line.text.trim_start();
    let Some(rest) = marker_rest(trimmed) else {
        return line;
    };
    let consumed = line.text.len() - rest.len();

    DocLine {
        text: rest,
        offset: line.offset + consumed,
    }
}

/// Returns the text that follows a list marker, if the line has one
fn marker_rest(trimmed: &str) -> Option<&str> {
    for marker in ['-', '*', '+'] {
        let Some(rest) = trimmed.strip_prefix(marker) else {
            continue;
        };
        if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
            return Some(rest.trim_start());
        }
    }

    let digits = trimmed.len()
        - trimmed
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    if digits == 0 {
        return None;
    }

    let numbered = &trimmed[digits..];
    for marker in ['.', ')'] {
        let Some(rest) = numbered.strip_prefix(marker) else {
            continue;
        };
        if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
            return Some(rest.trim_start());
        }
    }

    None
}

/// Returns every dash aside in a doc comment
fn find_asides(lines: &[DocLine<'_>]) -> Vec<Aside> {
    let prose = prose_lines(lines);
    let mut asides = Vec::new();

    for run in prose.split(Option::is_none) {
        let paragraph: Vec<DocLine<'_>> = run.iter().flatten().copied().collect();
        scan(&paragraph, &mut asides);
    }

    asides
}

/// Hides code spans and quoted text behind the filler byte
///
/// A dash inside a code span or a quotation belongs to the code or to
/// whoever is quoted. The filler is as long as what it hides, so byte
/// offsets stay correct. The text is a whole paragraph rather than one
/// line, so a delimiter that opens on one line and closes on the next
/// still pairs up.
fn mask(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    mask_code_spans(&mut bytes);
    mask_quotes(&mut bytes);

    bytes
}

/// Masks every `` `code` `` span in the paragraph
///
/// A span that never closes is masked to the end of the paragraph,
/// which keeps an unbalanced backtick from exposing the code after it.
fn mask_code_spans(bytes: &mut [u8]) {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }

        let start = index;
        index = run_end(bytes, index);
        let width = index - start;

        let end = closing_run(bytes, index, width).unwrap_or(bytes.len());
        bytes[start..end].fill(FILLER);
        index = end;
    }
}

/// Returns where the backtick run that started at `from` ends
fn run_end(bytes: &[u8], from: usize) -> usize {
    let mut index = from;
    while index < bytes.len() && bytes[index] == b'`' {
        index += 1;
    }

    index
}

/// Returns the end of the backtick run of `width` that closes a span
fn closing_run(bytes: &[u8], from: usize, width: usize) -> Option<usize> {
    let mut index = from;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }

        let start = index;
        index = run_end(bytes, index);
        if index - start == width {
            return Some(index);
        }
    }

    None
}

/// Masks every double-quoted run in the paragraph
///
/// A quotation reproduces someone else's punctuation, so the writer of
/// the doc comment cannot repair it. An unpaired quote masks nothing.
fn mask_quotes(bytes: &mut [u8]) {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }

        let Some(offset) = bytes[index + 1..].iter().position(|byte| *byte == b'"') else {
            return;
        };
        let end = index + offset + 2;
        bytes[index..end].fill(FILLER);
        index = end;
    }
}

/// Records every dash aside in one paragraph
///
/// The paragraph is scanned as a single text, because a dash only
/// interrupts a sentence when prose sits on both sides of it and that
/// prose may be a line away. Joining also lets the mask cover a code
/// span or a quotation that wraps across two lines.
fn scan(paragraph: &[DocLine<'_>], asides: &mut Vec<Aside>) {
    let (text, starts) = join(paragraph);
    let masked = mask(&text);

    let mut index = 0;
    while index < masked.len() {
        let Some((kind, end)) = dash_at(&masked, index) else {
            index += 1;
            continue;
        };

        let leading = masked[..index].iter().any(is_content);
        let trailing = masked[end..].iter().any(is_content);
        if leading && trailing {
            asides.push(Aside {
                kind,
                offset: locate(paragraph, &starts, index),
                length: end - index,
            });
        }

        index = end;
    }
}

/// Joins the lines of a paragraph, and reports where each one begins
///
/// The newline between two lines stands in for the space the writer
/// would have typed, so a dash at the end of a line still reads as
/// spaced.
fn join(paragraph: &[DocLine<'_>]) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(paragraph.len());

    for (index, line) in paragraph.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        starts.push(text.len());
        text.push_str(line.text);
    }

    (text, starts)
}

/// Returns the file offset of a byte of the joined paragraph
///
/// No dash spans a line break, so the byte at `index` always belongs to
/// exactly one of the lines the join was built from.
///
/// # Panics
///
/// Panics if `paragraph` is empty, or if `starts` did not come from
/// joining it.
fn locate(paragraph: &[DocLine<'_>], starts: &[usize], index: usize) -> usize {
    let position = starts
        .partition_point(|start| *start <= index)
        .saturating_sub(1);
    let DocLine { text: _, offset } = paragraph[position];

    offset + (index - starts[position])
}

/// Returns the dash that starts at `index`, and where it ends
///
/// An en dash counts only when a space touches it, because a tight en
/// dash joins the ends of a range. A hyphen counts only when spaces
/// touch both sides, because a tight hyphen joins two words.
fn dash_at(masked: &[u8], index: usize) -> Option<(AsideKind, usize)> {
    let rest = &masked[index..];

    if rest.starts_with(EM_DASH) {
        return Some((AsideKind::EmDash, index + EM_DASH.len()));
    }

    if rest.starts_with(EN_DASH) {
        let end = index + EN_DASH.len();
        if !spaced_before(masked, index) && !spaced_after(masked, end) {
            return None;
        }
        return Some((AsideKind::EnDash, end));
    }

    if masked[index] != b'-' {
        return None;
    }

    let mut end = index;
    while end < masked.len() && masked[end] == b'-' {
        end += 1;
    }
    if !spaced_before(masked, index) || !spaced_after(masked, end) {
        return None;
    }

    Some((AsideKind::SpacedHyphen, end))
}

/// Returns whether whitespace or the start of the line precedes `index`
fn spaced_before(masked: &[u8], index: usize) -> bool {
    match index.checked_sub(1) {
        Some(previous) => masked[previous].is_ascii_whitespace(),
        None => true,
    }
}

/// Returns whether whitespace or the end of the line follows `index`
fn spaced_after(masked: &[u8], index: usize) -> bool {
    match masked.get(index) {
        Some(byte) => byte.is_ascii_whitespace(),
        None => true,
    }
}

/// Returns whether a byte is part of the sentence rather than a gap
fn is_content(byte: &u8) -> bool {
    !byte.is_ascii_whitespace()
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for EmDashAside {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.em-dash-aside")]
    }
}

stiles_plugin::export_lints![rust: [EmDashAside], typescript: [EmDashAside]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(EmDashAside))]
    }

    fn check(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);

        execute(&tree, &mut passes())
    }

    #[test]
    fn block_doc_comment_with_em_dash_is_flagged() {
        let source = "/** Returns the type — or nothing */\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn block_doc_comment_with_star_margin_is_flagged() {
        let source = "/**\n * Returns the type\n *\n * The walk records it — and carries on.\n */\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn code_fence_with_spaced_hyphen_is_not_flagged() {
        let source = "/// Adds two numbers\n///\n/// ```\n/// let c = a - b;\n/// ```\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn code_span_wrapped_across_lines_is_not_flagged() {
        let source = "/// Uses the operator `a\n/// — b` in place\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn dash_after_a_closed_fence_is_flagged() {
        let source = "/// Adds\n///\n/// ```\n/// let c = a - b;\n/// ```\n///\n/// It adds — always.\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn dash_on_a_continuation_line_is_flagged_once() {
        let source =
            "/// Returns the resolved type\n/// — or nothing, when there is no span\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn double_hyphen_flag_name_is_not_flagged() {
        let source = "/// Pass --deny-warnings to fail on a warning\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn em_dash_in_a_code_span_is_not_flagged() {
        let source = "/// Renders `a — b` verbatim\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn em_dash_in_a_plain_block_comment_is_not_flagged() {
        let source = "/* Returns the type — or nothing */\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn em_dash_in_a_plain_comment_is_not_flagged() {
        let source = "// Returns the type — or nothing\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn em_dash_in_a_quotation_is_not_flagged() {
        let source = "/// The report reads \"resolved — or nothing\" verbatim\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn em_dash_in_an_inner_doc_comment_is_flagged_as_an_em_dash_warning() {
        let source = "//! Loads a project — and reports what it skipped\nfn f() {}";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.em-dash-aside")
            .has_severity(Severity::Warn)
            .message_contains("em dash");
    }

    #[test]
    fn em_dash_in_an_inner_doc_comment_is_flagged_once() {
        let source = "//! Loads a project — and reports what it skipped\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn em_dash_inside_a_nested_module_is_flagged() {
        let source = "mod inner {\n    /// Returns the type — or nothing\n    fn f() {}\n}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn em_dash_on_a_wrapped_line_is_flagged() {
        let source =
            "/// Returns the resolved type\n/// — or nothing, when the item has no span\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn em_dash_without_spaces_is_flagged() {
        let source = "/// Read the file first—do not guess at its contents\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn en_dash_in_a_range_is_not_flagged() {
        let source = "/// Keeps every sentence between 15–25 words\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn en_dash_with_spaces_is_flagged_as_an_en_dash() {
        let source = "/// Returns the type – or nothing, when there is no span\nfn f() {}";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).message_contains("en dash");
    }

    #[test]
    fn en_dash_with_spaces_is_flagged_once() {
        let source = "/// Returns the type – or nothing, when there is no span\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn heading_with_a_dash_is_not_flagged() {
        let source = "/// Loads a project\n///\n/// # Errors — and how to read them\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn heading_with_a_tab_after_the_hashes_is_not_flagged() {
        let source = "/// Loads a project\n///\n/// #\tErrors — and how to read them\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn hyphenated_word_is_not_flagged() {
        let source = "/// Reports a reference-style link definition\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn indented_code_block_is_not_flagged() {
        let source = "/// Adds two numbers\n///\n///     let c = a - b;\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn link_definition_is_not_flagged() {
        let source =
            "/// Returns an [`Option<T>`]\n///\n/// [`Option<T>`]: std::option::Option\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn list_bullet_is_not_flagged() {
        let source = "/// Reports two gaps\n///\n/// - outside the workspace root\n/// - not a member crate\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn many_documented_items_are_each_flagged_once() {
        let source =
            "/// One — two\nfn a() {}\n\n/// Three — four\nfn b() {}\n\n/// Five — six\nfn c() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 3);
    }

    #[test]
    fn negative_number_is_not_flagged() {
        let source = "/// Returns -1 when the item has no span\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn numbered_list_marker_is_not_flagged() {
        let source = "/// Runs in order\n///\n/// 1. parse\n/// 2. decorate\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn quotation_wrapped_across_lines_is_not_flagged() {
        let source = "/// The report reads \"resolved\n/// — or nothing\" verbatim\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn separate_doc_blocks_are_each_scanned() {
        let source = "/// Returns a type — or nothing\nfn f() {}\n\n/// Returns a span — or nothing\nfn g() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn single_em_dash_is_flagged_once() {
        let source = "/// Returns a type — or nothing\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn spaced_hyphen_mid_sentence_is_flagged_as_a_spaced_hyphen() {
        let source =
            "/// The walk records the failure - and carries on - so it stays complete\nfn f() {}";

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).message_contains("spaced hyphen");
    }

    #[test]
    fn spaced_hyphen_mid_sentence_is_flagged_twice() {
        let source =
            "/// The walk records the failure - and carries on - so it stays complete\nfn f() {}";

        let diagnostics = check(source);

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn span_covers_a_dash_on_a_continuation_line() {
        let source =
            "/// Returns the resolved type\n/// — or nothing, when there is no span\nfn f() {}";
        let start = source.find('—').expect("source should contain an em dash");

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + "—".len());
    }

    #[test]
    fn span_covers_only_the_dash() {
        let source = "/// Returns a type — or nothing\nfn f() {}";
        let start = source.find('—').expect("source should contain an em dash");

        let diagnostics = check(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + "—".len());
    }

    #[test]
    fn table_row_is_not_flagged() {
        let source = "/// Maps a gap to a fix\n///\n/// | gap | fix |\n/// | --- | --- |\n/// | outside - the root | move it |\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn thematic_break_is_not_flagged() {
        let source =
            "/// Loads a project\n///\n/// ---\n///\n/// Reports what it skipped\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn tilde_fence_with_spaced_hyphen_is_not_flagged() {
        let source = "/// Adds two numbers\n///\n/// ~~~\n/// let c = a - b;\n/// ~~~\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trailing_hyphen_at_the_end_of_a_block_is_not_flagged() {
        let source = "/// Reports the gap -\nfn f() {}";

        let diagnostics = check(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send_aside() {
        fn assert_send<T: Send>() {}
        assert_send::<Aside>();
    }

    #[test]
    fn trait_send_aside_kind() {
        fn assert_send<T: Send>() {}
        assert_send::<AsideKind>();
    }

    #[test]
    fn trait_send_doc_line() {
        fn assert_send<T: Send>() {}
        assert_send::<DocLine<'_>>();
    }

    #[test]
    fn trait_send_pass() {
        fn assert_send<T: Send>() {}
        assert_send::<EmDashAside>();
    }

    #[test]
    fn trait_sync_aside() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Aside>();
    }

    #[test]
    fn trait_sync_aside_kind() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<AsideKind>();
    }

    #[test]
    fn trait_sync_doc_line() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DocLine<'_>>();
    }

    #[test]
    fn trait_sync_pass() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<EmDashAside>();
    }

    #[test]
    fn trait_unpin_aside() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Aside>();
    }

    #[test]
    fn trait_unpin_aside_kind() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<AsideKind>();
    }

    #[test]
    fn trait_unpin_doc_line() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DocLine<'_>>();
    }

    #[test]
    fn trait_unpin_pass() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<EmDashAside>();
    }

    mod typescript {
        use stiles_typescript::TypeScriptLintPassAdapter;

        use super::*;

        fn check(source: &str) -> Vec<Diagnostic> {
            let tree = parse(source, Language::TypeScript);
            let mut passes: Vec<Box<dyn LintPass>> =
                vec![Box::new(TypeScriptLintPassAdapter::new(EmDashAside))];

            execute(&tree, &mut passes)
        }

        #[test]
        fn doc_comment_with_em_dash_is_flagged_at_the_dash() {
            let source = "/**\n * Adds two numbers \u{2014} quickly \u{2014} and returns them\n */\nfunction f() {}";

            let diagnostics = check(source);

            let start = source.find('\u{2014}').unwrap();
            assert_diagnostic(&diagnostics[0])
                .has_rule_id("lint.em-dash-aside")
                .has_severity(Severity::Warn)
                .has_span("<test>", start, start + '\u{2014}'.len_utf8());
        }

        #[test]
        fn doc_comment_with_em_dash_is_flagged_twice() {
            let source = "/**\n * Adds two numbers \u{2014} quickly \u{2014} and returns them\n */\nfunction f() {}";

            let diagnostics = check(source);

            assert_eq!(diagnostics.len(), 2);
        }

        #[test]
        fn doc_comment_with_spaced_hyphen_in_prose_is_flagged_as_a_spaced_hyphen() {
            let source = "/**\n * Reads the file - all of it - into memory\n */\nfunction f() {}";

            let diagnostics = check(source);

            assert_diagnostic(&diagnostics[0]).message_contains("spaced hyphen");
        }

        #[test]
        fn doc_comment_with_spaced_hyphen_in_prose_is_flagged_twice() {
            let source = "/**\n * Reads the file - all of it - into memory\n */\nfunction f() {}";

            let diagnostics = check(source);

            assert_eq!(diagnostics.len(), 2);
        }

        #[test]
        fn example_section_is_not_read() {
            let source = "/**\n * Subtracts\n * @example\n * sub(3 - 2)\n */\nfunction sub() {}";

            let diagnostics = check(source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn line_and_block_comments_are_not_read() {
            let source =
                "// a note \u{2014} not documentation\n/* another - aside */\nconst a = 1;";

            let diagnostics = check(source);

            assert_no_diagnostics(&diagnostics);
        }

        #[test]
        fn one_line_doc_comment_is_read() {
            let source = "/** Counts the rows \u{2014} slowly. */\nconst rows = 0;";

            let diagnostics = check(source);

            assert_eq!(diagnostics.len(), 1);
        }

        #[test]
        fn param_description_with_an_aside_is_flagged() {
            let source = "/**\n * @param a - the first \u{2014} never negative \u{2014} value\n */\nfunction f(a: number) {}";

            let diagnostics = check(source);

            assert_eq!(diagnostics.len(), 2);
        }

        #[test]
        fn param_hyphen_is_not_flagged() {
            let source = "/**\n * Adds two numbers\n * @param a - the first\n * @param {number} b - the second\n */\nfunction add(a: number, b: number) {}";

            let diagnostics = check(source);

            assert_no_diagnostics(&diagnostics);
        }
    }
}
