//! Reads the negative example and its fix from the docs of a rule's pass
//!
//! The docs of each pass hold a `text` block that opens with a `// DON'T`
//! line. The code below that line is what the rule reports. The code below
//! a later `// DO` line is the same code changed so that the rule does not
//! report it. A reader takes the pair as what the rule flags and what it
//! wants instead, so each rule's tests run the rule on both halves.

/// The marker line that opens the negative example
const EXAMPLE_MARKER: &str = "// DON'T";

/// The marker line that opens the fix
const FIX_MARKER: &str = "// DO";

/// The negative example and the fix in the docs of a rule's pass
///
/// # Examples
///
/// ```
/// use example_pair::ExamplePair;
///
/// let source = "/// ```text\n/// // DON'T\n/// let a = 1;\n///\n/// // DO\n/// let b = 1;\n/// ```\n";
/// let pair = ExamplePair::read(source);
///
/// assert_eq!(pair.fix(), "let b = 1;\n");
/// ```
pub struct ExamplePair {
    example: String,
    fix: String,
}

impl ExamplePair {
    /// Reads the pair from the source of a rule
    ///
    /// The pair is the first `text` block in a `///` doc comment whose
    /// first line is `// DON'T`. A blank line that ends a half belongs to
    /// neither half.
    ///
    /// # Panics
    ///
    /// Panics when the source holds no such block, or when the block has no
    /// `// DO` line.
    ///
    /// # Examples
    ///
    /// ```
    /// use example_pair::ExamplePair;
    ///
    /// let source = "/// ```text\n/// // DON'T\n/// let a = 1;\n///\n/// // DO\n/// let b = 1;\n/// ```\n";
    /// let pair = ExamplePair::read(source);
    ///
    /// assert_eq!(pair.example(), "let a = 1;\n");
    /// ```
    pub fn read(source: &str) -> Self {
        let docs: Vec<&str> = source.lines().filter_map(doc_text).collect();
        let start = docs
            .windows(2)
            .position(|lines| lines == ["```text", EXAMPLE_MARKER])
            .expect("the source should hold a `text` block that opens with `// DON'T`");
        let block: Vec<&str> = docs[start + 2..]
            .iter()
            .copied()
            .take_while(|line| *line != "```")
            .collect();
        let middle = block
            .iter()
            .position(|line| *line == FIX_MARKER)
            .expect("the block should hold a `// DO` line");

        Self {
            example: joined(&block[..middle]),
            fix: joined(&block[middle + 1..]),
        }
    }

    /// Returns the code that the rule reports
    ///
    /// # Examples
    ///
    /// ```
    /// use example_pair::ExamplePair;
    ///
    /// let source = "/// ```text\n/// // DON'T\n/// let a = 1;\n/// // DO\n/// let b = 1;\n/// ```\n";
    ///
    /// assert_eq!(ExamplePair::read(source).example(), "let a = 1;\n");
    /// ```
    pub fn example(&self) -> &str {
        &self.example
    }

    /// Returns the code that the rule does not report
    ///
    /// # Examples
    ///
    /// ```
    /// use example_pair::ExamplePair;
    ///
    /// let source = "/// ```text\n/// // DON'T\n/// let a = 1;\n/// // DO\n/// let b = 1;\n/// ```\n";
    ///
    /// assert_eq!(ExamplePair::read(source).fix(), "let b = 1;\n");
    /// ```
    pub fn fix(&self) -> &str {
        &self.fix
    }
}

/// Returns the text of a `///` doc line without its marker
fn doc_text(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("///")?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// Joins lines into source text, without the blank lines at its end
fn joined(lines: &[&str]) -> String {
    let end = lines
        .iter()
        .rposition(|line| !line.is_empty())
        .map_or(0, |last| last + 1);
    lines[..end]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str =
        "/// ```text\n/// // DON'T\n/// let a = 1;\n///\n/// // DO\n/// let b = 1;\n///\n/// ```\n";

    fn assert_auto_traits<T: Send + Sync + Unpin>() {}

    #[test]
    fn example_pair_is_send_sync_and_unpin() {
        assert_auto_traits::<ExamplePair>();
    }

    #[test]
    fn read_with_a_blank_line_before_the_fence_drops_it_from_the_fix() {
        let pair = ExamplePair::read(BLOCK);

        assert_eq!(pair.fix(), "let b = 1;\n");
    }

    #[test]
    fn read_with_a_blank_line_before_the_fix_drops_it_from_the_example() {
        let pair = ExamplePair::read(BLOCK);

        assert_eq!(pair.example(), "let a = 1;\n");
    }

    #[test]
    fn read_with_an_earlier_text_block_skips_it() {
        let source = format!("/// ```text\n/// plain\n/// ```\n{BLOCK}");

        let pair = ExamplePair::read(&source);

        assert_eq!(pair.example(), "let a = 1;\n");
    }

    #[test]
    fn read_with_indented_doc_lines_reads_them() {
        let source = BLOCK.replace("///", "    ///");

        let pair = ExamplePair::read(&source);

        assert_eq!(pair.fix(), "let b = 1;\n");
    }

    #[test]
    #[should_panic(expected = "`// DO` line")]
    fn read_without_a_fix_panics() {
        ExamplePair::read("/// ```text\n/// // DON'T\n/// let a = 1;\n/// ```\n");
    }

    #[test]
    #[should_panic(expected = "opens with `// DON'T`")]
    fn read_without_a_pair_panics() {
        ExamplePair::read("/// ```text\n/// let a = 1;\n/// ```\n");
    }
}
