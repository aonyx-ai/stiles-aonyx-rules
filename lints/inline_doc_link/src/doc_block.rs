use bon::bon;
use doc_comment::DocLine;
use whisker_types::{FilePath, Span};

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

#[bon]
impl DocBlock {
    /// Joins the prose lines of one doc comment into one Markdown document
    ///
    /// The lines come from `doc_comment`, which already removed the comment
    /// markers and the indentation the lines share. The join puts one
    /// newline between lines.
    ///
    /// Returns [`None`] when there are no lines.
    ///
    /// [`None`]: std::option::Option::None
    #[builder]
    pub(crate) fn new(file: FilePath, doc: &[DocLine<'_>]) -> Option<Self> {
        if doc.is_empty() {
            return None;
        }

        let mut content = String::new();
        let mut lines = Vec::with_capacity(doc.len());
        for (index, line) in doc.iter().enumerate() {
            if index > 0 {
                content.push('\n');
            }
            lines.push(Line {
                content_start: content.len(),
                file_start: line.offset(),
            });
            content.push_str(line.text());
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
        Span::builder()
            .file(self.file.clone())
            .start(self.to_file_offset(start))
            .end(self.to_file_offset(end))
            .build()
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

#[cfg(test)]
mod tests {
    use whisker_testing::parse;
    use whisker_types::Language;

    use super::*;

    /// Builds the block of the doc comment that opens `source`
    fn block_of(source: &str) -> DocBlock {
        let tree = parse(source, Language::Rust);
        let first = tree.root_node().named_child(0).expect("a comment");
        let lines = doc_comment::rust(&first).expect("a doc comment");

        DocBlock::builder()
            .file(first.span().file_path().clone())
            .doc(&lines)
            .build()
            .expect("should build a block")
    }

    #[test]
    fn build_with_no_lines_returns_none() {
        let tree = parse("fn f() {}", Language::Rust);
        let file = tree.root_node().span().file_path().clone();

        let block = DocBlock::builder().file(file).doc(&[]).build();

        assert!(block.is_none());
    }

    #[test]
    fn content_joins_consecutive_lines() {
        let source = "/// first\n/// second\nfn f() {}";

        let block = block_of(source);

        assert_eq!(block.content(), "first\nsecond");
    }

    #[test]
    fn content_keeps_extra_indentation() {
        let source = "/// a\n///     b\nfn f() {}";

        let block = block_of(source);

        assert_eq!(block.content(), "a\n    b");
    }

    #[test]
    fn span_maps_an_offset_on_a_later_line() {
        let source = "/// first\n/// second\nfn f() {}";
        let block = block_of(source);

        let span = block.span(6, 12);

        assert_eq!(&source[span.start()..span.end()], "second");
    }

    #[test]
    fn span_maps_an_offset_on_the_first_line() {
        let source = "/// first\n/// second\nfn f() {}";
        let block = block_of(source);

        let span = block.span(0, 5);

        assert_eq!(&source[span.start()..span.end()], "first");
    }

    #[test]
    fn trait_send_doc_block() {
        fn assert_send<T: Send>() {}
        assert_send::<DocBlock>();
    }

    #[test]
    fn trait_send_line() {
        fn assert_send<T: Send>() {}
        assert_send::<Line>();
    }

    #[test]
    fn trait_sync_doc_block() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DocBlock>();
    }

    #[test]
    fn trait_sync_line() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Line>();
    }

    #[test]
    fn trait_unpin_doc_block() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DocBlock>();
    }

    #[test]
    fn trait_unpin_line() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Line>();
    }
}
