use whisker_types::{DecoratedNode, FilePath, Span};

/// The Markdown of one doc comment, joined the way rustdoc joins it
///
/// Rustdoc reads the consecutive `///` lines above an item as one document.
/// A code fence can open on one line and close on a later one, so a rule
/// that reads each comment line alone never sees the fence close. This type
/// joins the lines and remembers where each one starts in the file. An
/// offset in the joined text maps back to a [`Span`].
///
/// [`Span`]: whisker_types::Span
pub(crate) struct DocBlock {
    file: FilePath,
    content: String,
    lines: Vec<Line>,
}

/// Where one line of the joined text came from
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct Line {
    /// Offset of the line's first byte in the joined text
    content_start: usize,
    /// Offset of the same byte in the source file
    file_start: usize,
}

impl DocBlock {
    /// Joins a run of comment nodes into one Markdown document
    ///
    /// Each node contributes the text of its `doc` field, which is what
    /// follows the `///`, `//!`, `/**`, or `/*!` marker. For a line comment
    /// that field also covers the newline, so the join drops it and puts one
    /// back between lines. The join then strips the indentation every
    /// non-blank line shares, or the space after `///` would read as an
    /// indented code block.
    ///
    /// Returns [`None`] when no node carries such a field, which is the case
    /// for an ordinary comment.
    ///
    /// [`None`]: std::option::Option::None
    pub(crate) fn from_comments(comments: &[DecoratedNode<'_>]) -> Option<Self> {
        let file = comments.first()?.span().file_path().clone();

        let mut raw: Vec<(usize, &str)> = Vec::new();
        for comment in comments {
            let Some(doc) = comment.child_by_field_name("doc") else {
                continue;
            };
            let start = doc.raw().start_byte();
            let body = doc.text();
            let body = body.strip_suffix('\n').unwrap_or(body);
            let body = body.strip_suffix('\r').unwrap_or(body);

            let mut offset = 0;
            for line in body.split('\n') {
                let text = line.strip_suffix('\r').unwrap_or(line);
                raw.push((start + offset, text));
                offset += line.len() + 1;
            }
        }

        Self::from_lines(file, raw)
    }

    /// Joins lines already taken out of a doc comment
    ///
    /// Each entry is the file offset of a line's first byte and the line's
    /// text. A TypeScript doc comment arrives this way, its delimiters and
    /// block tag syntax already gone. The join strips the indentation every
    /// non-blank line shares, as [`DocBlock::from_comments`] does.
    ///
    /// Returns [`None`] when there are no lines.
    ///
    /// [`None`]: std::option::Option::None
    pub(crate) fn from_lines(file: FilePath, raw: Vec<(usize, &str)>) -> Option<Self> {
        if raw.is_empty() {
            return None;
        }

        let indent = raw
            .iter()
            .filter(|(_, text)| !text.trim().is_empty())
            .map(|(_, text)| leading_space_count(text))
            .min()
            .unwrap_or(0);

        let mut content = String::new();
        let mut lines = Vec::with_capacity(raw.len());
        for (index, (file_start, text)) in raw.iter().enumerate() {
            if index > 0 {
                content.push('\n');
            }
            let strip = indent.min(leading_space_count(text));
            lines.push(Line {
                content_start: content.len(),
                file_start: file_start + strip,
            });
            content.push_str(&text[strip..]);
        }

        Some(Self {
            file,
            content,
            lines,
        })
    }

    /// Returns the joined Markdown text
    pub(crate) fn content(&self) -> &str {
        &self.content
    }

    /// Returns the span of the file text that `start..end` came from
    ///
    /// # Panics
    ///
    /// Panics if `start` is greater than `end`.
    pub(crate) fn span(&self, start: usize, end: usize) -> Span {
        Span::new(
            self.file.clone(),
            self.to_file_offset(start),
            self.to_file_offset(end),
        )
    }

    /// Converts an offset in the joined text to an offset in the file
    fn to_file_offset(&self, offset: usize) -> usize {
        let index = self
            .lines
            .partition_point(|line| line.content_start <= offset)
            .saturating_sub(1);
        let line = self.lines[index];

        line.file_start + (offset - line.content_start)
    }
}

/// Counts the spaces a line starts with
fn leading_space_count(text: &str) -> usize {
    text.bytes().take_while(|byte| *byte == b' ').count()
}

#[cfg(test)]
mod tests {
    use whisker_testing::parse;
    use whisker_types::{DecoratedTree, Language};

    use super::*;

    /// Collects the comment nodes of a parsed file in source order
    fn comments(tree: &DecoratedTree) -> Vec<DecoratedNode<'_>> {
        fn walk<'a>(node: &DecoratedNode<'a>, found: &mut Vec<DecoratedNode<'a>>) {
            if node.kind() == "line_comment" || node.kind() == "block_comment" {
                found.push(*node);
            }
            for child in node.named_children() {
                walk(&child, found);
            }
        }

        let mut found = Vec::new();
        walk(&tree.root_node(), &mut found);
        found
    }

    #[test]
    fn content_joins_consecutive_lines() {
        let tree = parse("/// first\n/// second\nfn f() {}", Language::Rust);
        let nodes = comments(&tree);

        let block = DocBlock::from_comments(&nodes).expect("should build a block");

        assert_eq!(block.content(), "first\nsecond");
    }

    #[test]
    fn content_keeps_extra_indentation() {
        let tree = parse("/// a\n///     b\nfn f() {}", Language::Rust);
        let nodes = comments(&tree);

        let block = DocBlock::from_comments(&nodes).expect("should build a block");

        assert_eq!(block.content(), "a\n    b");
    }

    #[test]
    fn from_comments_with_empty_slice_returns_none() {
        let nodes: Vec<DecoratedNode<'_>> = Vec::new();

        let block = DocBlock::from_comments(&nodes);

        assert!(block.is_none());
    }

    #[test]
    fn from_comments_with_plain_comment_returns_none() {
        let tree = parse("// plain\nfn f() {}", Language::Rust);
        let nodes = comments(&tree);

        let block = DocBlock::from_comments(&nodes);

        assert!(block.is_none());
    }

    #[test]
    fn span_maps_an_offset_on_a_later_line() {
        let source = "/// first\n/// second\nfn f() {}";
        let tree = parse(source, Language::Rust);
        let nodes = comments(&tree);
        let block = DocBlock::from_comments(&nodes).expect("should build a block");

        let span = block.span(6, 12);

        assert_eq!(&source[span.start()..span.end()], "second");
    }

    #[test]
    fn span_maps_an_offset_on_the_first_line() {
        let source = "/// first\n/// second\nfn f() {}";
        let tree = parse(source, Language::Rust);
        let nodes = comments(&tree);
        let block = DocBlock::from_comments(&nodes).expect("should build a block");

        let span = block.span(0, 5);

        assert_eq!(&source[span.start()..span.end()], "first");
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DocBlock>();
        assert_send::<Line>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DocBlock>();
        assert_sync::<Line>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DocBlock>();
        assert_unpin::<Line>();
    }
}
