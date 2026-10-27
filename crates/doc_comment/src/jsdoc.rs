//! The prose of a TypeScript doc comment, line by line
//!
//! A TypeScript doc comment is a `/** */` block. Its prose sits among
//! syntax a prose rule must not read: the delimiters, the `*` that opens
//! each line, and block tags such as `@param name - description`. The
//! hyphen of a block tag separates a name from its description, and it
//! does not interrupt a sentence. An `@example` section usually holds code
//! without a fence.
//!
//! [`prose_lines`] strips that syntax and keeps every line's position in the
//! file. A rule that finds something in the prose thus reports it at the
//! right bytes.

use crate::{DocLine, strip_star};

/// The tags whose first word after any `{type}` is a name, not prose
///
/// JSDoc and TSDoc write a hyphen between such a name and its description,
/// and the hyphen is part of the syntax too.
const NAMED_TAGS: &[&str] = &[
    "arg",
    "argument",
    "param",
    "prop",
    "property",
    "template",
    "typeParam",
];

/// The tag whose section holds code rather than prose
const EXAMPLE_TAG: &str = "example";

/// Returns the prose lines of a doc comment, or [`None`] when `comment` is
/// not one
///
/// `comment` is the whole comment as the source spells it, and `start` is
/// the file offset of its first byte. A doc comment opens with `/**`, so a
/// `//` comment, a `/* */` comment, and the empty `/**/` return [`None`].
///
/// Every line of the comment yields one prose line, so paragraphs keep their
/// shape. The line loses its leading `*` and the space after it. A block
/// tag line loses the tag, its `{type}`, and, for a tag that names
/// something, the name and the hyphen after it. The lines of an `@example`
/// section come back empty.
///
/// [`None`]: std::option::Option::None
pub(crate) fn prose_lines(comment: &str, start: usize) -> Option<Vec<DocLine<'_>>> {
    let body = comment.strip_prefix("/**")?.strip_suffix("*/")?;
    let base = start + "/**".len();

    let mut lines = Vec::new();
    let mut in_example = false;
    let mut consumed = 0;

    for raw in body.split('\n') {
        let line = DocLine {
            text: raw.strip_suffix('\r').unwrap_or(raw),
            offset: base + consumed,
        };
        consumed += raw.len() + 1;

        let line = strip_star(line);
        let Some(tag) = block_tag(line.text) else {
            lines.push(match in_example {
                true => line.blank(),
                false => line,
            });
            continue;
        };

        in_example = tag == EXAMPLE_TAG;
        lines.push(strip_tag(line, tag));
    }

    Some(lines)
}

/// Returns the name of the block tag that opens `text`, if any
fn block_tag(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('@')?;
    let length = rest
        .find(|character: char| !character.is_ascii_alphanumeric())
        .unwrap_or(rest.len());

    match length {
        0 => None,
        _ => Some(&rest[..length]),
    }
}

/// Returns the line without the syntax of its block tag
fn strip_tag<'a>(line: DocLine<'a>, tag: &str) -> DocLine<'a> {
    let leading = line.text.len() - line.text.trim_start().len();
    let mut line = line.advance(leading + "@".len() + tag.len());

    line = skip_spaces(line);
    if line.text.starts_with('{')
        && let Some(close) = closing_brace(line.text)
    {
        line = skip_spaces(line.advance(close + 1));
    }

    if !NAMED_TAGS.contains(&tag) {
        return line;
    }

    line = skip_spaces(line.advance(name_length(line.text)));
    if line.text == "-" {
        return line.advance(1);
    }
    match line.text.strip_prefix("- ") {
        Some(_) => skip_spaces(line.advance(2)),
        None => line,
    }
}

/// Returns the line without its leading spaces and tabs
fn skip_spaces(line: DocLine<'_>) -> DocLine<'_> {
    let rest = line.text.trim_start_matches([' ', '\t']);
    line.advance(line.text.len() - rest.len())
}

/// Returns the byte index of the brace that closes the one `text` opens
///
/// A type may nest braces, as in `{{ a: string }}`.
fn closing_brace(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, character) in text.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }

    None
}

/// Returns the length of the name that opens `text`
///
/// An optional parameter is written in brackets, as in `[name=default]`,
/// and its default may hold spaces, so a bracketed name runs to its
/// closing bracket.
fn name_length(text: &str) -> usize {
    if text.starts_with('[') {
        return text.find(']').map_or(text.len(), |close| close + 1);
    }

    text.find([' ', '\t']).unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn texts<'a>(lines: &[DocLine<'a>]) -> Vec<&'a str> {
        lines.iter().map(DocLine::text).collect()
    }

    #[test]
    fn prose_lines_blank_an_example_section_until_the_next_tag() {
        let comment = "/**\n * @example Adding\n * add(1 - 2)\n * @returns the sum\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(texts(&lines), ["", "Adding", "", "the sum", ""]);
    }

    #[test]
    fn prose_lines_drop_a_bracketed_name_with_a_default() {
        let comment = "/**\n * @param [label = \"a b\"] - the label\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(lines[1].text(), "the label");
    }

    #[test]
    fn prose_lines_drop_a_nested_type() {
        let comment = "/**\n * @returns {{ ok: boolean }} whether it worked\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(lines[1].text(), "whether it worked");
    }

    #[test]
    fn prose_lines_drop_the_name_and_hyphen_of_a_param() {
        let comment = "/**\n * @param {number} count - how many - at most ten\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(lines[1].text(), "how many - at most ten");
    }

    #[test]
    fn prose_lines_keep_a_param_without_a_hyphen() {
        let comment = "/**\n * @param count how many\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(lines[1].text(), "how many");
    }

    #[test]
    fn prose_lines_keep_an_email_address() {
        let comment = "/**\n * Ask team@example.com first\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(lines[1].text(), "Ask team@example.com first");
    }

    #[test]
    fn prose_lines_keep_bold_text_that_opens_a_line() {
        let comment = "/**\n**Note** this matters\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(lines[1].text(), "**Note** this matters");
    }

    #[test]
    fn prose_lines_keep_file_offsets() {
        let comment = "/**\n * Returns the sum\n */";

        let lines = prose_lines(comment, 40).expect("a doc comment");

        let line = lines[1];
        assert_eq!(&comment[line.offset() - 40..][..15], "Returns the sum");
    }

    #[test]
    fn prose_lines_keep_the_offset_of_a_param_description() {
        let comment = "/**\n * @param {number} count - how many - at most ten\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(&comment[lines[1].offset()..][..8], "how many");
    }

    #[test]
    fn prose_lines_of_a_line_comment_return_none() {
        let lines = prose_lines("// Returns the sum", 0);

        assert_eq!(lines, None);
    }

    #[test]
    fn prose_lines_of_a_one_line_comment_keep_its_text() {
        let lines = prose_lines("/** Returns the sum */", 0).expect("a doc comment");

        assert_eq!(texts(&lines), [" Returns the sum "]);
    }

    #[test]
    fn prose_lines_of_a_plain_block_comment_return_none() {
        let lines = prose_lines("/* Returns the sum */", 0);

        assert_eq!(lines, None);
    }

    #[test]
    fn prose_lines_of_an_empty_block_comment_return_none() {
        let lines = prose_lines("/**/", 0);

        assert_eq!(lines, None);
    }

    #[test]
    fn prose_lines_with_a_file_offset_keep_the_text() {
        let comment = "/**\n * Returns the sum\n */";

        let lines = prose_lines(comment, 40).expect("a doc comment");

        let line = lines[1];
        assert_eq!(line.text(), "Returns the sum");
    }

    #[test]
    fn prose_lines_with_crlf_endings_drop_the_carriage_return() {
        let comment = "/**\r\n * Returns the sum\r\n */";

        let lines = prose_lines(comment, 0).expect("a doc comment");

        assert_eq!(lines[1].text(), "Returns the sum");
    }

    proptest! {
        #[test]
        fn every_prose_line_is_a_slice_of_the_comment_at_its_offset(
            body in "( \\* ([a-z@{}\\[\\]=-]| ){0,30}\n){0,6}",
            start in 0usize..1000,
        ) {
            let comment = format!("/**\n{body} */");

            let lines = prose_lines(&comment, start).expect("a doc comment");

            for line in lines {
                let offset = line.offset() - start;
                prop_assert_eq!(&comment[offset..offset + line.text().len()], line.text());
            }
        }
    }
}
