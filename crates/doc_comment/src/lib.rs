//! The prose of a doc comment, line by line, in Rust and in TypeScript
//!
//! A rule that reads documentation prose needs the comment's text without
//! its syntax. That syntax is the `///` or `/** */` markers, the `*` that
//! opens a line of a block, and the indentation every line shares. The rule
//! also needs the lines of one comment together, because a sentence or a
//! code fence may cross them. [`rust`] and [`typescript`] return exactly
//! that. They keep each line's offset in the file, so a finding in the prose
//! reports at the right bytes.

mod jsdoc;
mod rust;

use whisker_types::DecoratedNode;

pub use self::rust::rust;

/// One line of a doc comment's prose, with the offset of its first byte
///
/// The offset counts from the start of the file, so a finding at byte `i`
/// of the text sits at byte `offset + i` of the file.
///
/// # Examples
///
/// ```
/// use whisker_testing::parse;
/// use whisker_types::Language;
///
/// let tree = parse("/// Adds two\nfn add() {}", Language::Rust);
/// let comment = tree.root_node().named_child(0).unwrap();
///
/// let lines: Vec<doc_comment::DocLine> = doc_comment::rust(&comment).unwrap();
///
/// assert_eq!((lines[0].text(), lines[0].offset()), ("Adds two", 4));
/// ```
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct DocLine<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> DocLine<'a> {
    /// Returns the text of the line
    ///
    /// # Examples
    ///
    /// ```
    /// use whisker_testing::parse;
    /// use whisker_types::Language;
    ///
    /// let tree = parse("/** Adds two numbers */\nfunction add() {}", Language::TypeScript);
    /// let comment = tree.root_node().named_child(0).unwrap();
    ///
    /// let lines = doc_comment::typescript(&comment).unwrap();
    ///
    /// assert_eq!(lines[0].text(), "Adds two numbers ");
    /// ```
    pub fn text(&self) -> &'a str {
        self.text
    }

    /// Returns the file offset of the line's first byte
    ///
    /// # Examples
    ///
    /// ```
    /// use whisker_testing::parse;
    /// use whisker_types::Language;
    ///
    /// let source = "/** Adds two numbers */\nfunction add() {}";
    /// let tree = parse(source, Language::TypeScript);
    /// let comment = tree.root_node().named_child(0).unwrap();
    ///
    /// let lines = doc_comment::typescript(&comment).unwrap();
    ///
    /// assert_eq!(lines[0].offset(), source.find("Adds").unwrap());
    /// ```
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the line without its first `consumed` bytes
    fn advance(self, consumed: usize) -> Self {
        Self {
            text: &self.text[consumed..],
            offset: self.offset + consumed,
        }
    }

    /// Returns an empty line at the same place, which ends a paragraph
    fn blank(self) -> Self {
        Self {
            text: "",
            offset: self.offset,
        }
    }
}

/// Returns the prose lines of a TypeScript doc comment, or [`None`] when
/// `node` is not one
///
/// A TypeScript doc comment is a `/** */` block. Each of its lines loses the
/// `*` that opens it, and the lines lose the indentation they share. A block
/// tag line loses the tag and its `{type}`. A tag that names something also
/// loses the name and the hyphen after it, as in `@param a - the first`. The lines of an `@example` section come back empty, because they
/// hold code. A `//` comment, a `/* */` comment, and the empty `/**/` return
/// [`None`].
///
/// [`None`]: std::option::Option::None
///
/// # Examples
///
/// ```
/// use whisker_testing::parse;
/// use whisker_types::Language;
///
/// let source = "/**\n * Adds two numbers\n * @param a - the first\n */\nfunction add(a) {}";
/// let tree = parse(source, Language::TypeScript);
/// let comment = tree.root_node().named_child(0).unwrap();
///
/// let lines = doc_comment::typescript(&comment).unwrap();
///
/// let texts: Vec<&str> = lines.iter().map(|line| line.text()).collect();
/// assert_eq!(texts, ["", "Adds two numbers", "the first", ""]);
/// ```
pub fn typescript<'a>(node: &DecoratedNode<'a>) -> Option<Vec<DocLine<'a>>> {
    let mut lines = self::jsdoc::prose_lines(node.text(), node.span().start())?;
    unindent(&mut lines);
    Some(lines)
}

/// Returns the line without the `*` that opens it and one space after it
///
/// A `*` followed by anything but whitespace is prose, such as `**bold**`,
/// so it stays. A line of whitespace alone comes back empty.
fn strip_star(line: DocLine<'_>) -> DocLine<'_> {
    let trimmed = line.text.trim_start();
    if trimmed.is_empty() {
        return line.advance(line.text.len());
    }
    let Some(rest) = trimmed.strip_prefix('*') else {
        return line;
    };
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return line;
    }

    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    line.advance(line.text.len() - rest.len())
}

/// Removes the indentation every non-blank line shares
///
/// Without it, the space after `///` would indent every line of a Rust doc
/// comment by one. A line indented by four more would then not read as an
/// indented code block.
fn unindent(lines: &mut [DocLine<'_>]) {
    let common = lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .map(|line| spaces(line.text))
        .min()
        .unwrap_or(0);

    for line in lines.iter_mut() {
        let strip = common.min(spaces(line.text));
        *line = line.advance(strip);
    }
}

/// Returns the number of spaces that open `text`
fn spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_star_keeps_bold_text() {
        let line = DocLine {
            text: "**Note** this",
            offset: 0,
        };

        let stripped = strip_star(line);

        assert_eq!(stripped.text(), "**Note** this");
    }

    #[test]
    fn strip_star_removes_the_star_and_one_space() {
        let line = DocLine {
            text: "   *  indented",
            offset: 10,
        };

        let stripped = strip_star(line);

        assert_eq!(stripped.text(), " indented");
    }

    #[test]
    fn strip_star_shifts_the_offset_past_the_star_and_one_space() {
        let line = DocLine {
            text: "   *  indented",
            offset: 10,
        };

        let stripped = strip_star(line);

        assert_eq!(stripped.offset(), 15);
    }

    #[test]
    fn unindent_ignores_blank_lines_and_leaves_them_empty() {
        let mut lines = [
            DocLine {
                text: "  a",
                offset: 0,
            },
            DocLine {
                text: "",
                offset: 5,
            },
            DocLine {
                text: "  b",
                offset: 6,
            },
        ];

        unindent(&mut lines);

        assert_eq!(lines[1].text(), "");
    }

    #[test]
    fn unindent_ignores_blank_lines_and_strips_the_first_line() {
        let mut lines = [
            DocLine {
                text: "  a",
                offset: 0,
            },
            DocLine {
                text: "",
                offset: 5,
            },
            DocLine {
                text: "  b",
                offset: 6,
            },
        ];

        unindent(&mut lines);

        assert_eq!(lines[0].text(), "a");
    }

    #[test]
    fn unindent_ignores_blank_lines_and_strips_the_last_line() {
        let mut lines = [
            DocLine {
                text: "  a",
                offset: 0,
            },
            DocLine {
                text: "",
                offset: 5,
            },
            DocLine {
                text: "  b",
                offset: 6,
            },
        ];

        unindent(&mut lines);

        assert_eq!(lines[2].text(), "b");
    }

    #[test]
    fn unindent_keeps_deeper_indentation() {
        let mut lines = [
            DocLine {
                text: " text",
                offset: 0,
            },
            DocLine {
                text: "     code",
                offset: 10,
            },
        ];

        unindent(&mut lines);

        assert_eq!(lines[1].text(), "    code");
    }

    #[test]
    fn unindent_shifts_the_deeper_line_offset_past_the_common_indentation() {
        let mut lines = [
            DocLine {
                text: " text",
                offset: 0,
            },
            DocLine {
                text: "     code",
                offset: 10,
            },
        ];

        unindent(&mut lines);

        assert_eq!(lines[1].offset(), 11);
    }

    #[test]
    fn unindent_strips_the_common_indentation() {
        let mut lines = [
            DocLine {
                text: " text",
                offset: 0,
            },
            DocLine {
                text: "     code",
                offset: 10,
            },
        ];

        unindent(&mut lines);

        assert_eq!(lines[0].text(), "text");
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DocLine<'_>>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DocLine<'_>>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DocLine<'_>>();
    }
}
