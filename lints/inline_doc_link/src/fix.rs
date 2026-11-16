use std::ops::Range;

use doc_comment::DocLine;
use stiles_types::{DecoratedNode, Edit, FileText, Fix};

use crate::doc_block::DocBlock;
use crate::inline_link::{Ending, InlineLink, LineRole, line_roles};

/// The characters that make link text more than a plain label
///
/// Link text with formatting or brackets leaves a choice of label, so the
/// fix leaves such a link alone.
const FORMATTING: &[char] = &['`', '*', '_', '~', '[', ']', '\\', '<', '>', '&'];

/// The longest label that CommonMark accepts
const MAX_LABEL: usize = 999;

/// The language of a doc comment, which decides where its prose ends
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(crate) enum Syntax {
    /// A Rust doc comment, whose Markdown runs to its end
    Rust,
    /// A TypeScript doc comment, whose first block tag ends the Markdown of
    /// its description
    TypeScript,
}

/// How a doc comment sits in the file
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Form {
    /// A `/** */` or `/*! */` block, which ends with `*/`
    Block,
    /// A run of `///` or `//!` lines
    LineRun,
}

/// What comes after the description that a definition joins
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Tail {
    /// The end of the doc comment
    End,
    /// A block tag, which starts a section of its own
    Tag,
}

/// How the last line of the description relates to a new definition
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Shape {
    /// The line is blank or a definition, so a definition can follow it
    Joined,
    /// The line holds prose, so a blank line has to come first
    Separated,
}

/// What the line before a line makes of a definition on it
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Previous {
    /// A blank line or the start of the comment, after which a definition
    /// can start
    Boundary,
    /// A line of code, after which a definition can start
    Code,
    /// A definition, which another definition can follow
    Definition,
    /// Prose, which a definition cannot interrupt
    Text,
}

/// Returns the text of the row of `line` before the line's prose
///
/// That is the comment syntax and the indentation the lines share, such as
/// `    /// ` or ` * `.
fn before<'a>(source: &FileText<'a>, line: DocLine<'_>) -> Option<&'a str> {
    source.get(source.line_start(line.offset())?..line.offset())
}

/// One inline link that the fix can rewrite as a reference link
#[derive(Clone, Debug)]
struct Candidate<'a> {
    /// The index of the link among the links of the comment
    index: usize,
    link: InlineLink,
    /// The link text, which becomes the label
    text: &'a str,
    /// The label as CommonMark compares it
    label: String,
    /// The destination and the optional title, as the link spells them
    body: &'a str,
}

/// One reference definition that the comment already holds
#[derive(Clone, Debug)]
struct Definition<'a> {
    /// The index of the line that holds the definition
    line: usize,
    label: String,
    body: &'a str,
}

/// The fix of one doc comment, and the links that it rewrites
pub(crate) struct CommentFix {
    fix: Fix,
    covered: Vec<usize>,
}

impl CommentFix {
    /// Returns the fix when it rewrites the link at `index`
    pub(crate) fn covering(&self, index: usize) -> Option<&Fix> {
        self.covered.contains(&index).then_some(&self.fix)
    }
}

/// The doc comment that a fix rewrites
#[derive(Copy, Clone)]
pub(crate) struct Comment<'a, 'b> {
    /// The node of the comment, or of the first line of a run
    pub(crate) node: &'b DecoratedNode<'a>,
    /// The prose lines of the comment
    pub(crate) lines: &'b [DocLine<'a>],
    /// The language the comment is written in
    pub(crate) syntax: Syntax,
}

/// Returns the fix that turns the inline links of one doc comment into
/// reference links, or [`None`] when no link can take it
///
/// One fix covers the whole comment, because every definition goes to the
/// same place. Each finding whose link it rewrites carries this same fix.
///
/// [`None`]: std::option::Option::None
pub(crate) fn fix(
    comment: Comment<'_, '_>,
    block: &DocBlock,
    links: &[InlineLink],
) -> Option<CommentFix> {
    let Comment {
        node,
        lines,
        syntax,
    } = comment;
    let source = node.file_text();
    let form = match node.kind() {
        "line_comment" => Form::LineRun,
        _ => Form::Block,
    };

    let full = description_end(&source, lines, form)?;
    let end = match syntax {
        Syntax::Rust => full,
        Syntax::TypeScript => lines[..full]
            .iter()
            .position(|line| before(&source, *line).is_some_and(|row| row.contains('@')))
            .unwrap_or(full),
    };
    let tail = match end == full {
        true => Tail::End,
        false => Tail::Tag,
    };
    let anchor = end.checked_sub(1)?;

    let content = block.content();
    let section = &content[..line_starts(lines)[anchor] + lines[anchor].text().len()];
    let (roles, ending) = line_roles(section);
    match ending {
        Ending::InsideFence => return None,
        Ending::OutsideFence => {}
    }
    let (definitions, shape) = definitions(&lines[..end], &roles);

    let candidates: Vec<Candidate<'_>> = links
        .iter()
        .enumerate()
        .filter(|(_, link)| link.end() <= section.len())
        .filter_map(|(index, link)| candidate(content, index, *link))
        .collect();
    let usages = usages(section, links, &lines[..end], &definitions);

    let mut labels: Vec<&str> = Vec::new();
    let mut new_definitions: Vec<String> = Vec::new();
    for candidate in &candidates {
        if labels.contains(&candidate.label.as_str()) {
            continue;
        }
        match verdict(candidate, &candidates, &definitions, &usages) {
            Verdict::Defined => {}
            Verdict::Rejected => continue,
            Verdict::Undefined => {
                new_definitions.push(format!("[{}]: {}", candidate.text, candidate.body));
            }
        }
        labels.push(&candidate.label);
    }
    let accepted: Vec<&Candidate<'_>> = candidates
        .iter()
        .filter(|candidate| labels.contains(&candidate.label.as_str()))
        .collect();
    if accepted.is_empty() {
        return None;
    }

    let mut edits: Vec<Edit> = accepted
        .iter()
        .map(|candidate| Edit::delete(block.span(candidate.link.close() + 1..candidate.link.end())))
        .collect();
    if !new_definitions.is_empty() {
        let layout = Layout { form, shape, tail };
        edits.push(insertion(
            &source,
            lines,
            anchor,
            layout,
            &new_definitions,
            node,
        )?);
    }

    let fix = Fix::builder()
        .message("turn the inline links into reference links")
        .edits(edits)
        .build()
        .ok()?;
    Some(CommentFix {
        fix,
        covered: accepted.iter().map(|candidate| candidate.index).collect(),
    })
}

/// What the fix makes of the label of a candidate
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Verdict {
    /// The comment already defines the label with the same body
    Defined,
    /// The fix leaves every link with the label alone
    Rejected,
    /// The fix adds a definition of the label
    Undefined,
}

/// Returns what the fix makes of the label of `candidate`
///
/// Every candidate with the label must have the same body, and so must every
/// definition of it. Without a definition, no other bracketed text may use
/// the label, because it would start to resolve to the new one.
fn verdict(
    candidate: &Candidate<'_>,
    candidates: &[Candidate<'_>],
    definitions: &[Definition<'_>],
    usages: &[String],
) -> Verdict {
    let label = &candidate.label;
    let body = candidate.body;
    if candidates
        .iter()
        .any(|other| other.label == *label && other.body != body)
    {
        return Verdict::Rejected;
    }

    let existing: Vec<&Definition<'_>> = definitions
        .iter()
        .filter(|definition| definition.label == *label)
        .collect();
    if existing.iter().any(|definition| definition.body != body) {
        return Verdict::Rejected;
    }
    match (existing.is_empty(), usages.contains(label)) {
        (false, _) => Verdict::Defined,
        (true, true) => Verdict::Rejected,
        (true, false) => Verdict::Undefined,
    }
}

/// Where the definitions go and what surrounds them
#[derive(Copy, Clone, Debug)]
struct Layout {
    form: Form,
    shape: Shape,
    tail: Tail,
}

/// Returns the number of lines that hold the prose of the comment, or
/// [`None`] when the fix cannot write into its shape
///
/// A run of line comments holds prose on every line. A block holds it on
/// every line but the last, which must hold `*/` alone. Every line between
/// the first and the last must open with `*`. Rustdoc strips that `*` only
/// when every line has it, so a new line could otherwise not match.
///
/// [`None`]: std::option::Option::None
fn description_end(source: &FileText<'_>, lines: &[DocLine<'_>], form: Form) -> Option<usize> {
    match form {
        Form::LineRun => Some(lines.len()),
        Form::Block => {
            let (close, middle) = lines.split_last()?;
            let middle = middle.get(1..)?;
            let closes_alone = close.text().trim().is_empty()
                && before(source, *close).is_some_and(|row| row.trim().is_empty())
                && lines.len() >= 2;
            let starred = middle.iter().all(|line| {
                before(source, *line).is_some_and(|row| row.trim_start().starts_with('*'))
            });
            (closes_alone && starred).then_some(lines.len() - 1)
        }
    }
}

/// Returns the offset in the joined text where each line starts
fn line_starts(lines: &[DocLine<'_>]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in lines {
        starts.push(offset);
        offset += line.text().len() + 1;
    }
    starts
}

/// Returns the link at `index` as a candidate for the fix, or [`None`] when
/// a rewrite could change what the text means
///
/// The link must lie on one line. Its text must be a plain label, and its
/// destination must not be empty. The rewrite must not join the link with
/// the text around it. A `[` or a `(` after it would
/// make a new link, and a `:` would make a definition. A `]` before it
/// would make the label of a full reference link.
///
/// [`None`]: std::option::Option::None
fn candidate(content: &str, index: usize, link: InlineLink) -> Option<Candidate<'_>> {
    if content[link.start()..link.end()].contains('\n') {
        return None;
    }
    let text = &content[link.text_start()..link.close()];
    if text.trim().is_empty() || text.contains(FORMATTING) || text.len() > MAX_LABEL {
        return None;
    }
    let body = content[link.close() + "](".len()..link.end() - ")".len()].trim();
    if body.is_empty() {
        return None;
    }
    let after = content[link.end()..].chars().next();
    let before = content[..link.start()].chars().next_back();
    if after.is_some_and(|after| ['[', '(', ':'].contains(&after)) || before == Some(']') {
        return None;
    }

    Some(Candidate {
        index,
        link,
        text,
        label: normalized(text),
        body,
    })
}

/// Returns the reference definitions of the description, and whether a
/// definition can follow its last line
///
/// A definition cannot interrupt a paragraph, so only a line after a blank
/// line, code, or another definition counts.
fn definitions<'a>(lines: &[DocLine<'a>], roles: &[LineRole]) -> (Vec<Definition<'a>>, Shape) {
    let mut definitions = Vec::new();
    let mut previous = Previous::Boundary;
    for (index, (line, role)) in lines.iter().zip(roles).enumerate() {
        let text = line.text();
        previous = match role {
            LineRole::Skip => match text.trim().is_empty() {
                true => Previous::Boundary,
                false => Previous::Code,
            },
            LineRole::Prose => match previous {
                Previous::Text => Previous::Text,
                Previous::Boundary | Previous::Code | Previous::Definition => {
                    match definition(text) {
                        Some((label, body)) => {
                            definitions.push(Definition {
                                line: index,
                                label,
                                body,
                            });
                            Previous::Definition
                        }
                        None => Previous::Text,
                    }
                }
            },
        };
    }

    let shape = match previous {
        Previous::Code | Previous::Text => Shape::Separated,
        Previous::Boundary | Previous::Definition => Shape::Joined,
    };
    (definitions, shape)
}

/// Returns the normalized label and the body of the definition that `text`
/// spells, as in `[label]: url`
fn definition(text: &str) -> Option<(String, &str)> {
    let indent = text.len() - text.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = text[indent..].strip_prefix('[')?;
    let close = rest.find(']')?;
    let label = &rest[..close];
    if label.contains(['[', '\\']) || label.trim().is_empty() {
        return None;
    }
    let body = rest[close + 1..].strip_prefix(':')?.trim();
    if body.is_empty() {
        return None;
    }

    Some((normalized(label), body))
}

/// Returns the labels of every bracketed text in the description outside
/// the inline links and the definitions
///
/// A shortcut or a full reference link with one of these labels would
/// start to resolve to a new definition. The scan reads code as well, so it
/// finds more brackets than Markdown does, never fewer.
fn usages(
    section: &str,
    links: &[InlineLink],
    lines: &[DocLine<'_>],
    definitions: &[Definition<'_>],
) -> Vec<String> {
    let starts = line_starts(lines);
    let defining: Vec<Range<usize>> = definitions
        .iter()
        .map(|definition| {
            let start = starts[definition.line];
            start..start + lines[definition.line].text().len()
        })
        .collect();
    let skipped = |offset: usize| {
        links
            .iter()
            .any(|link| (link.start()..link.end()).contains(&offset))
            || defining.iter().any(|range| range.contains(&offset))
    };

    let mut labels = Vec::new();
    for (open, _) in section.match_indices('[') {
        if skipped(open) {
            continue;
        }
        let rest = &section[open + 1..];
        let Some(close) = rest.find([']', '[']) else {
            continue;
        };
        if rest[close..].starts_with(']') {
            labels.push(normalized(&rest[..close]));
        }
    }
    labels
}

/// Returns the edit that adds the new definitions at the end of the
/// description, or [`None`] when the comment syntax around it is unclear
///
/// The definitions follow the last line of the description. A blank line
/// comes first when that line holds prose. A blank line comes after them
/// when a block tag follows.
///
/// [`None`]: std::option::Option::None
fn insertion(
    source: &FileText<'_>,
    lines: &[DocLine<'_>],
    anchor: usize,
    layout: Layout,
    new_definitions: &[String],
    node: &DecoratedNode<'_>,
) -> Option<Edit> {
    let Layout { form, shape, tail } = layout;
    let first = match form {
        Form::Block => 1,
        Form::LineRun => 0,
    };
    let marked = lines
        .get(first..=anchor)
        .unwrap_or_default()
        .iter()
        .rev()
        .find(|line| !line.text().trim().is_empty())
        .and_then(|line| before(source, *line));
    let prefix = match (form, marked) {
        (Form::LineRun, Some(prefix)) => {
            let marker = prefix.trim_start();
            (marker.starts_with("///") || marker.starts_with("//!")).then(|| prefix.to_owned())?
        }
        (Form::LineRun, None) => return None,
        (Form::Block, Some(prefix)) => prefix
            .trim_start()
            .starts_with('*')
            .then(|| prefix.to_owned())?,
        (Form::Block, None) => format!("{}* ", before(source, *lines.last()?)?),
    };
    let blank = prefix.trim_end();

    let line_end = source.line_end(lines[anchor].offset())?;
    let position = match source.get(source.start()..line_end)?.ends_with('\r') {
        true => line_end - 1,
        false => line_end,
    };
    let newline = source.line_ending().as_str();

    let mut text = String::new();
    match shape {
        Shape::Joined => {}
        Shape::Separated => text.push_str(&format!("{newline}{blank}")),
    }
    for definition in new_definitions {
        text.push_str(&format!("{newline}{prefix}{definition}"));
    }
    match tail {
        Tail::End => {}
        Tail::Tag => text.push_str(&format!("{newline}{blank}")),
    }

    Some(
        Edit::insert()
            .file(node.span().file_path().clone())
            .offset(position)
            .text(text)
            .build(),
    )
}

/// Returns a label the way CommonMark compares labels
///
/// The comparison ignores case, and it treats each run of whitespace as one
/// space.
fn normalized(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_reads_the_label_and_the_body() {
        let found = definition("[The Docs]: https://example.com \"Title\"");

        assert_eq!(
            found,
            Some(("the docs".to_owned(), "https://example.com \"Title\""))
        );
    }

    #[test]
    fn definition_with_four_spaces_of_indentation_is_code() {
        let found = definition("    [a]: https://example.com");

        assert!(found.is_none());
    }

    #[test]
    fn normalized_folds_case_and_whitespace() {
        let label = normalized("  The \t Docs ");

        assert_eq!(label, "the docs");
    }

    #[test]
    fn trait_send_candidate() {
        fn assert_send<T: Send>() {}
        assert_send::<Candidate<'_>>();
    }

    #[test]
    fn trait_send_comment() {
        fn assert_send<T: Send>() {}
        assert_send::<Comment<'_, '_>>();
    }

    #[test]
    fn trait_send_comment_fix() {
        fn assert_send<T: Send>() {}
        assert_send::<CommentFix>();
    }

    #[test]
    fn trait_send_definition() {
        fn assert_send<T: Send>() {}
        assert_send::<Definition<'_>>();
    }

    #[test]
    fn trait_send_form() {
        fn assert_send<T: Send>() {}
        assert_send::<Form>();
    }

    #[test]
    fn trait_send_layout() {
        fn assert_send<T: Send>() {}
        assert_send::<Layout>();
    }

    #[test]
    fn trait_send_previous() {
        fn assert_send<T: Send>() {}
        assert_send::<Previous>();
    }

    #[test]
    fn trait_send_shape() {
        fn assert_send<T: Send>() {}
        assert_send::<Shape>();
    }

    #[test]
    fn trait_send_syntax() {
        fn assert_send<T: Send>() {}
        assert_send::<Syntax>();
    }

    #[test]
    fn trait_send_tail() {
        fn assert_send<T: Send>() {}
        assert_send::<Tail>();
    }

    #[test]
    fn trait_send_verdict() {
        fn assert_send<T: Send>() {}
        assert_send::<Verdict>();
    }

    #[test]
    fn trait_sync_candidate() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Candidate<'_>>();
    }

    #[test]
    fn trait_sync_comment() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Comment<'_, '_>>();
    }

    #[test]
    fn trait_sync_comment_fix() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<CommentFix>();
    }

    #[test]
    fn trait_sync_definition() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Definition<'_>>();
    }

    #[test]
    fn trait_sync_form() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Form>();
    }

    #[test]
    fn trait_sync_layout() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Layout>();
    }

    #[test]
    fn trait_sync_previous() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Previous>();
    }

    #[test]
    fn trait_sync_shape() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Shape>();
    }

    #[test]
    fn trait_sync_syntax() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Syntax>();
    }

    #[test]
    fn trait_sync_tail() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Tail>();
    }

    #[test]
    fn trait_sync_verdict() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Verdict>();
    }

    #[test]
    fn trait_unpin_candidate() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Candidate<'_>>();
    }

    #[test]
    fn trait_unpin_comment() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Comment<'_, '_>>();
    }

    #[test]
    fn trait_unpin_comment_fix() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<CommentFix>();
    }

    #[test]
    fn trait_unpin_definition() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Definition<'_>>();
    }

    #[test]
    fn trait_unpin_form() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Form>();
    }

    #[test]
    fn trait_unpin_layout() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Layout>();
    }

    #[test]
    fn trait_unpin_previous() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Previous>();
    }

    #[test]
    fn trait_unpin_shape() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Shape>();
    }

    #[test]
    fn trait_unpin_syntax() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Syntax>();
    }

    #[test]
    fn trait_unpin_tail() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Tail>();
    }

    #[test]
    fn trait_unpin_verdict() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Verdict>();
    }
}
