mod link_kind;

pub(crate) use self::link_kind::LinkKind;

/// One inline Markdown link inside a doc comment
///
/// The offsets index the joined text of a [`DocBlock`], not the source
/// file, because this module never sees the file. [`DocBlock::span`] turns
/// them back into a span.
///
/// [`DocBlock`]: crate::doc_block::DocBlock
/// [`DocBlock::span`]: crate::doc_block::DocBlock::span
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub(crate) struct InlineLink {
    kind: LinkKind,
    start: usize,
    end: usize,
}

impl InlineLink {
    /// Finds every inline link in a Markdown document
    ///
    /// The search follows CommonMark closely, so text that only looks like a
    /// link stays unflagged. It skips fenced and indented code blocks, code
    /// spans, and bracket runs that a backslash escapes. Reference links
    /// such as `[text]` and `[text][label]` are the forms this rule asks
    /// for, so they never match.
    pub(crate) fn find_all(content: &str) -> Vec<Self> {
        let bytes = content.as_bytes();
        let mut links = Vec::new();

        for prose in prose_ranges(content) {
            scan(bytes, prose, &mut links);
        }

        links
    }

    /// Returns the form the author wrote
    pub(crate) fn kind(self) -> LinkKind {
        self.kind
    }

    /// Returns the offset of the link's first byte
    pub(crate) fn start(self) -> usize {
        self.start
    }

    /// Returns the offset one past the link's last byte
    pub(crate) fn end(self) -> usize {
        self.end
    }
}

/// A run of consecutive lines that hold Markdown prose
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct ProseRange {
    /// Offset of the run's first byte
    start: usize,
    /// Offset one past the run's last byte
    end: usize,
}

/// The bytes of a document that one scan may read
///
/// A scan stops at `end`, the end of one run of prose. Neither a link nor a
/// code span crosses a paragraph break or a code block. The helpers take a
/// window rather than the bytes and the end apart, so a caller cannot pass
/// an offset where the end belongs.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct Window<'a> {
    /// The whole document
    bytes: &'a [u8],
    /// Offset one past the last byte the scan may read
    end: usize,
}

/// What a line contributes to the document
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum LineRole {
    /// The line holds prose, so the link scanner reads it
    Prose,
    /// The line holds code or nothing, so the link scanner skips it
    Skip,
}

/// The kind of block the block scanner is inside
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum BlockState {
    /// Inside a fenced code block opened by `marker` repeated `length` times
    Fenced { marker: u8, length: usize },
    /// Inside a code block that four spaces of indentation introduced
    IndentedCode,
    /// Outside any code block
    Prose,
}

/// Whether a line carries anything but whitespace
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum Fill {
    /// The line is empty or holds only whitespace
    Blank,
    /// The line holds content
    Text,
}

/// One line of the document, split at the end of its indentation
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct Line<'a> {
    /// The number of spaces the line starts with
    indent: usize,
    /// The line without its leading spaces
    rest: &'a str,
    /// Whether `rest` holds anything but whitespace
    fill: Fill,
}

impl<'a> Line<'a> {
    /// Splits one line of the document
    fn new(text: &'a str) -> Self {
        let indent = text.bytes().take_while(|byte| *byte == b' ').count();
        let rest = &text[indent..];
        let fill = match rest.trim_end().is_empty() {
            true => Fill::Blank,
            false => Fill::Text,
        };

        Self { indent, rest, fill }
    }
}

/// Splits a document into the byte ranges that hold prose
///
/// A code block can span many lines, so the rule cannot judge one comment
/// line on its own. Blank lines end a range as well, because neither a code
/// span nor a link may cross a paragraph break.
fn prose_ranges(content: &str) -> Vec<ProseRange> {
    let mut ranges: Vec<ProseRange> = Vec::new();
    let mut state = BlockState::Prose;
    let mut previous = Fill::Blank;
    let mut offset = 0;

    for text in content.split('\n') {
        let start = offset;
        let end = offset + text.len();
        offset = end + 1;

        let line = Line::new(text);
        let (next, role) = advance(state, line, previous);
        state = next;
        previous = line.fill;

        let extends = match ranges.last() {
            Some(last) => last.end + 1 == start,
            None => false,
        };

        match role {
            LineRole::Prose => match extends {
                true => {
                    let last = ranges.last_mut().expect("a range exists to extend");
                    last.end = end;
                }
                false => ranges.push(ProseRange { start, end }),
            },
            LineRole::Skip => {}
        }
    }

    ranges
}

/// Advances the block scanner across one line
fn advance(state: BlockState, line: Line<'_>, previous: Fill) -> (BlockState, LineRole) {
    let Line { indent, rest, fill } = line;
    match state {
        BlockState::Fenced { marker, length } => {
            match indent <= 3 && closes_fence(rest, marker, length) {
                true => (BlockState::Prose, LineRole::Skip),
                false => (BlockState::Fenced { marker, length }, LineRole::Skip),
            }
        }
        BlockState::IndentedCode => match fill == Fill::Blank || indent >= 4 {
            true => (BlockState::IndentedCode, LineRole::Skip),
            false => classify(line, previous),
        },
        BlockState::Prose => classify(line, previous),
    }
}

/// Classifies a line that starts outside any code block
fn classify(line: Line<'_>, previous: Fill) -> (BlockState, LineRole) {
    let Line { indent, rest, fill } = line;
    if let Some(fence) = opens_fence(rest, indent) {
        return (fence, LineRole::Skip);
    }

    match fill {
        Fill::Blank => (BlockState::Prose, LineRole::Skip),
        Fill::Text => match previous == Fill::Blank && indent >= 4 {
            true => (BlockState::IndentedCode, LineRole::Skip),
            false => (BlockState::Prose, LineRole::Prose),
        },
    }
}

/// Returns the fence a line opens, if it opens one
fn opens_fence(rest: &str, indent: usize) -> Option<BlockState> {
    if indent > 3 {
        return None;
    }
    let marker = rest.bytes().next()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = rest.bytes().take_while(|byte| *byte == marker).count();
    if length < 3 {
        return None;
    }
    let info = &rest[length..];
    if marker == b'`' && info.contains('`') {
        return None;
    }
    Some(BlockState::Fenced { marker, length })
}

/// Returns whether a line closes the fence that is currently open
fn closes_fence(rest: &str, marker: u8, length: usize) -> bool {
    let count = rest.bytes().take_while(|byte| *byte == marker).count();
    count >= length && rest[count..].trim().is_empty()
}

/// Collects the inline links in one run of prose
fn scan(bytes: &[u8], prose: ProseRange, links: &mut Vec<InlineLink>) {
    let ProseRange { start, end } = prose;
    let window = Window { bytes, end };
    let mut index = start;

    while index < end {
        let byte = bytes[index];

        if byte == b'\\' {
            index += 2;
        } else if byte == b'`' {
            index = skip_code_span(window, index);
        } else if byte == b'!' && bytes.get(index + 1) == Some(&b'[') && index + 1 < end {
            index = match link_end(window, index + 1) {
                Some(link) => {
                    links.push(InlineLink {
                        kind: LinkKind::Image,
                        start: index,
                        end: link,
                    });
                    link
                }
                None => index + 1,
            };
        } else if byte == b'[' {
            index = match link_end(window, index) {
                Some(link) => {
                    links.push(InlineLink {
                        kind: LinkKind::Text,
                        start: index,
                        end: link,
                    });
                    link
                }
                None => index + 1,
            };
        } else {
            index += 1;
        }
    }
}

/// Returns the offset just past an inline link that starts at `open`
///
/// `open` is the offset of the `[` that begins the link text. Returns
/// [`None`] for a reference link, for an unclosed bracket, and for a `(`
/// group that is not a well-formed destination.
///
/// [`None`]: std::option::Option::None
fn link_end(window: Window<'_>, open: usize) -> Option<usize> {
    let Window { bytes, end } = window;
    let mut index = open + 1;
    let mut depth = 0usize;

    let close = loop {
        if index >= end {
            return None;
        }
        let byte = bytes[index];
        if byte == b'\\' {
            index += 2;
        } else if byte == b'`' {
            index = skip_code_span(window, index);
        } else if byte == b'[' {
            depth += 1;
            index += 1;
        } else if byte == b']' {
            if depth == 0 {
                break index;
            }
            depth -= 1;
            index += 1;
        } else {
            index += 1;
        }
    };

    let paren = close + 1;
    if paren >= end || bytes[paren] != b'(' {
        return None;
    }

    destination_end(window, paren + 1)
}

/// Returns the offset just past the `)` that closes the destination and its
/// optional title
fn destination_end(window: Window<'_>, open: usize) -> Option<usize> {
    let Window { bytes, end } = window;
    let mut index = skip_whitespace(window, open);
    if index >= end {
        return None;
    }

    if bytes[index] == b'<' {
        index += 1;
        loop {
            if index >= end {
                return None;
            }
            let byte = bytes[index];
            if byte == b'\\' {
                index += 2;
            } else if byte == b'\n' || byte == b'<' {
                return None;
            } else if byte == b'>' {
                index += 1;
                break;
            } else {
                index += 1;
            }
        }
    } else {
        let mut depth = 0usize;
        while index < end {
            let byte = bytes[index];
            if byte == b'\\' {
                index += 2;
            } else if byte.is_ascii_whitespace() || byte.is_ascii_control() {
                break;
            } else if byte == b'(' {
                depth += 1;
                index += 1;
            } else if byte == b')' {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                index += 1;
            } else {
                index += 1;
            }
        }
    }

    let index = skip_whitespace(window, index);
    if index < end && bytes[index] == b')' {
        return Some(index + 1);
    }

    title_end(window, index)
}

/// Returns the offset just past the `)` that follows a link title
fn title_end(window: Window<'_>, open: usize) -> Option<usize> {
    let Window { bytes, end } = window;
    if open >= end {
        return None;
    }

    let closer = match bytes[open] {
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };

    let mut index = open + 1;
    loop {
        if index >= end {
            return None;
        }
        let byte = bytes[index];
        if byte == b'\\' {
            index += 2;
        } else if byte == closer {
            index += 1;
            break;
        } else {
            index += 1;
        }
    }

    let index = skip_whitespace(window, index);
    match index < end && bytes[index] == b')' {
        true => Some(index + 1),
        false => None,
    }
}

/// Returns the offset just past a code span
///
/// A backtick run that nothing of the same length closes is literal text in
/// CommonMark, not the opening of a span. The result is then the offset just
/// past those backticks, so one stray backtick cannot hide every link after
/// it.
fn skip_code_span(window: Window<'_>, open: usize) -> usize {
    let Window { bytes, end } = window;
    let length = backtick_run(window, open);
    let mut index = open + length;

    while index < end {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let run = backtick_run(window, index);
        if run == length {
            return index + run;
        }
        index += run;
    }

    open + length
}

/// Counts the backticks that start at `open`
fn backtick_run(window: Window<'_>, open: usize) -> usize {
    let Window { bytes, end } = window;
    let mut index = open;
    while index < end && bytes[index] == b'`' {
        index += 1;
    }
    index - open
}

/// Returns the offset of the first byte that is not ASCII whitespace
fn skip_whitespace(window: Window<'_>, open: usize) -> usize {
    let Window { bytes, end } = window;
    let mut index = open;
    while index < end && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(content: &str) -> Vec<LinkKind> {
        InlineLink::find_all(content)
            .into_iter()
            .map(InlineLink::kind)
            .collect()
    }

    fn texts(content: &str) -> Vec<&str> {
        InlineLink::find_all(content)
            .into_iter()
            .map(|link| &content[link.start()..link.end()])
            .collect()
    }

    #[test]
    fn find_all_with_angle_bracket_destination_matches() {
        let content = "See [the docs](<https://example.com/a b>) now";

        let found = texts(content);

        assert_eq!(found, vec!["[the docs](<https://example.com/a b>)"]);
    }

    #[test]
    fn find_all_with_autolink_finds_nothing() {
        let content = "See <https://example.com/docs> for details";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_backslash_escaped_bracket_finds_nothing() {
        let content = "Literal \\[text\\](url) stays literal";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_balanced_parens_in_destination_matches_whole_link() {
        let content = "See [x](https://example.com/a_(b)_c) now";

        let found = texts(content);

        assert_eq!(found, vec!["[x](https://example.com/a_(b)_c)"]);
    }

    #[test]
    fn find_all_with_bare_url_finds_nothing() {
        let content = "See https://example.com/docs for details";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_blank_line_between_bracket_and_paren_finds_nothing() {
        let content = "See [foo]\n\n(a parenthetical)";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_bracket_inside_code_span_finds_nothing() {
        let content = "Write `[text](url)` to make a link";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_code_span_in_link_text_matches() {
        let content = "Uses [`HashMap`](std::collections::HashMap) internally";

        let found = texts(content);

        assert_eq!(found, vec!["[`HashMap`](std::collections::HashMap)"]);
    }

    #[test]
    fn find_all_with_collapsed_reference_link_finds_nothing() {
        let content = "See [the docs][] for details";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_empty_destination_matches() {
        let content = "See [the docs]() for details";

        let found = texts(content);

        assert_eq!(found, vec!["[the docs]()"]);
    }

    #[test]
    fn find_all_with_empty_input_finds_nothing() {
        let content = "";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_fenced_block_after_prose_reads_the_prose() {
        let content = "See [a](b) here\n\n```rust\nlet x = [c](d);\n```";

        let found = texts(content);

        assert_eq!(found, vec!["[a](b)"]);
    }

    #[test]
    fn find_all_with_fenced_block_finds_nothing() {
        let content = "# Examples\n\n```\nlet url = [text](https://example.com);\n```";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_full_reference_link_finds_nothing() {
        let content = "See [the docs][docs] for details";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_image_link_reports_an_image() {
        let content = "![diagram](https://example.com/diagram.png)";

        let found = kinds(content);

        assert_eq!(found, vec![LinkKind::Image]);
    }

    #[test]
    fn find_all_with_indented_code_block_finds_nothing() {
        let content = "Example:\n\n    let url = [text](https://example.com);\n";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_inline_link_reports_a_text_link() {
        let content = "See [the docs](https://example.com/docs) for details";

        let found = kinds(content);

        assert_eq!(found, vec![LinkKind::Text]);
    }

    #[test]
    fn find_all_with_link_split_across_lines_matches() {
        let content = "See [the\ndocs](https://example.com) for details";

        let found = texts(content);

        assert_eq!(found, vec!["[the\ndocs](https://example.com)"]);
    }

    #[test]
    fn find_all_with_nested_brackets_matches_the_inner_link() {
        let content = "Text [outer [inner](https://example.com)] tail";

        let found = texts(content);

        assert_eq!(found, vec!["[inner](https://example.com)"]);
    }

    #[test]
    fn find_all_with_non_ascii_after_backslash_finds_the_link() {
        let content = "Caf\\é [a](b) tail";

        let found = texts(content);

        assert_eq!(found, vec!["[a](b)"]);
    }

    #[test]
    fn find_all_with_reference_definition_finds_nothing() {
        let content = "See [the docs]\n\n[the docs]: https://example.com/docs";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_shortcut_reference_link_finds_nothing() {
        let content = "Returns [`Option<T>`] if the value exists";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_space_before_paren_finds_nothing() {
        let content = "See [the docs] (https://example.com/docs)";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_tilde_fence_finds_nothing() {
        let content = "~~~\n[text](https://example.com)\n~~~";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_titled_link_matches_whole_link() {
        let content = "See [x](https://example.com \"a title\") now";

        let found = texts(content);

        assert_eq!(found, vec!["[x](https://example.com \"a title\")"]);
    }

    #[test]
    fn find_all_with_two_links_on_one_line_matches_both() {
        let content = "See [a](https://example.com/a) and [b](https://example.com/b)";

        let found = texts(content);

        assert_eq!(
            found,
            vec!["[a](https://example.com/a)", "[b](https://example.com/b)"]
        );
    }

    #[test]
    fn find_all_with_unclosed_bracket_finds_nothing() {
        let content = "See [the docs for details";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn find_all_with_unclosed_fence_skips_the_rest() {
        let content = "Prose [a](b)\n\n```\n[c](d)\nstill code";

        let found = texts(content);

        assert_eq!(found, vec!["[a](b)"]);
    }

    #[test]
    fn find_all_with_unmatched_backtick_finds_the_link() {
        let content = "A stray ` tick then [a](https://example.com)";

        let found = texts(content);

        assert_eq!(found, vec!["[a](https://example.com)"]);
    }

    #[test]
    fn find_all_with_word_after_destination_finds_nothing() {
        let content = "The value [x](y z) is undefined";

        let found = texts(content);

        assert!(found.is_empty());
    }

    #[test]
    fn trait_send_block_state() {
        fn assert_send<T: Send>() {}
        assert_send::<BlockState>();
    }

    #[test]
    fn trait_send_fill() {
        fn assert_send<T: Send>() {}
        assert_send::<Fill>();
    }

    #[test]
    fn trait_send_inline_link() {
        fn assert_send<T: Send>() {}
        assert_send::<InlineLink>();
    }

    #[test]
    fn trait_send_line() {
        fn assert_send<T: Send>() {}
        assert_send::<Line<'_>>();
    }

    #[test]
    fn trait_send_line_role() {
        fn assert_send<T: Send>() {}
        assert_send::<LineRole>();
    }

    #[test]
    fn trait_send_prose_range() {
        fn assert_send<T: Send>() {}
        assert_send::<ProseRange>();
    }

    #[test]
    fn trait_send_window() {
        fn assert_send<T: Send>() {}
        assert_send::<Window<'_>>();
    }

    #[test]
    fn trait_sync_block_state() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<BlockState>();
    }

    #[test]
    fn trait_sync_fill() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Fill>();
    }

    #[test]
    fn trait_sync_inline_link() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<InlineLink>();
    }

    #[test]
    fn trait_sync_line() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Line<'_>>();
    }

    #[test]
    fn trait_sync_line_role() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<LineRole>();
    }

    #[test]
    fn trait_sync_prose_range() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<ProseRange>();
    }

    #[test]
    fn trait_sync_window() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Window<'_>>();
    }

    #[test]
    fn trait_unpin_block_state() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<BlockState>();
    }

    #[test]
    fn trait_unpin_fill() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Fill>();
    }

    #[test]
    fn trait_unpin_inline_link() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<InlineLink>();
    }

    #[test]
    fn trait_unpin_line() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Line<'_>>();
    }

    #[test]
    fn trait_unpin_line_role() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<LineRole>();
    }

    #[test]
    fn trait_unpin_prose_range() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<ProseRange>();
    }

    #[test]
    fn trait_unpin_window() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Window<'_>>();
    }
}
