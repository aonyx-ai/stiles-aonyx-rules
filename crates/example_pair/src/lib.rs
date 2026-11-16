//! Reads the negative example and its fix from the docs of a rule's pass
//!
//! The docs of each pass hold a `text` block that opens with a `// DON'T`
//! line. The code below that line is what the rule reports. The code below
//! a later `// DO` line is the same code changed so that the rule does not
//! report it. A reader takes the pair as what the rule flags and what it
//! wants instead, so each rule's tests run the rule on both halves.
//!
//! The check of the negative example also checks the fix of the rule. When
//! a finding carries a fix, the fix must turn the negative example into the
//! fix half byte for byte. The docs then show what `stiles check --fix`
//! does, and a rule cannot add a fix that its docs do not show.

use stiles_core::FixRound;
use stiles_types::Diagnostic;

/// The marker line that opens the negative example
const EXAMPLE_MARKER: &str = "// DON'T";

/// The marker line that opens the fix
const FIX_MARKER: &str = "// DO";

/// The most rounds of fixes that the example may take
///
/// `stiles check --fix` stops at the same number.
const MAX_ROUNDS: usize = 10;

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
    /// assert_eq!(pair.fix(), "let b = 1;\n");
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

    /// Asserts that the rule reports the negative example, and that its
    /// fixes, if any, turn it into the fix half
    ///
    /// `run` runs the rule on a source text and returns its findings. When
    /// no finding carries a fix, the check ends there. Otherwise the fixes
    /// apply in rounds, as `stiles check --fix` applies them, and each round
    /// runs the rule afresh. The result must equal the fix half byte for
    /// byte.
    ///
    /// # Panics
    ///
    /// Panics when the rule reports nothing on the example. Panics when the
    /// fixes do not settle within the rounds that `--fix` allows, or when
    /// they produce other text.
    ///
    /// # Examples
    ///
    /// ```
    /// use example_pair::ExamplePair;
    /// use stiles_types::{Diagnostic, Edit, Fix, RuleId, Severity, Span};
    ///
    /// /// Reports `let a` and renames it to `let b`
    /// fn run(text: &str) -> Vec<Diagnostic> {
    ///     let Some(start) = text.find("let a") else {
    ///         return Vec::new();
    ///     };
    ///     let span = Span::builder().file("<test>").start(start + 4).end(start + 5).build();
    ///     let edit = Edit::builder().span(span.clone()).replacement("b").build();
    ///     let fix = Fix::builder().message("rename it").edits(vec![edit]).build();
    ///     let diagnostic = Diagnostic::builder()
    ///         .rule_id(RuleId::new("lint.a"))
    ///         .severity(Severity::Error)
    ///         .message("a variable named `a`")
    ///         .span(span)
    ///         .build()
    ///         .with_fix(fix.expect("the fix should build"))
    ///         .expect("the fix should attach");
    ///     vec![diagnostic]
    /// }
    ///
    /// let source = "/// ```text\n/// // DON'T\n/// let a = 1;\n/// // DO\n/// let b = 1;\n/// ```\n";
    ///
    /// ExamplePair::read(source).assert_example(run);
    /// ```
    pub fn assert_example<F>(&self, run: F)
    where
        F: FnMut(&str) -> Vec<Diagnostic>,
    {
        let mut run = run;
        let diagnostics = run(&self.example);
        assert!(
            !diagnostics.is_empty(),
            "the rule should report the example"
        );
        if diagnostics
            .iter()
            .all(|diagnostic| diagnostic.fix().is_none())
        {
            return;
        }

        let mut text = self.example.clone();
        let mut diagnostics = diagnostics;
        for _round in 0..MAX_ROUNDS {
            let round = FixRound::apply()
                .text(&text)
                .diagnostics(&diagnostics)
                .call()
                .unwrap_or_else(|error| panic!("{error}"));
            if round.applied().is_empty() {
                assert_eq!(
                    text, self.fix,
                    "the fixes of the example should produce the fix half"
                );
                return;
            }
            text = round.into_text();
            diagnostics = run(&text);
        }

        panic!(
            "the fixes of the example did not settle within {MAX_ROUNDS} rounds; the last text was:\n{text}"
        );
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
    use stiles_types::{Edit, Fix, RuleId, Severity, Span};

    use super::*;

    const BLOCK: &str =
        "/// ```text\n/// // DON'T\n/// let a = 1;\n///\n/// // DO\n/// let b = 1;\n///\n/// ```\n";

    /// What the test rule does with `let a`
    #[derive(Copy, Clone)]
    enum Answer {
        /// Reports it and attaches no fix
        Report,
        /// Reports it with a fix that renames it to `aa`, which never settles
        ToAa,
        /// Reports it with a fix that renames it to `b`
        ToB,
        /// Reports it with a fix that renames it to `c`
        ToC,
    }

    /// Returns a rule that reports `let a` as `answer` says
    fn rule(answer: Answer) -> impl FnMut(&str) -> Vec<Diagnostic> {
        move |text| {
            let Some(start) = text.find("let a") else {
                return Vec::new();
            };
            let span = Span::builder()
                .file("<test>")
                .start(start + 4)
                .end(start + 5)
                .build();
            let diagnostic = Diagnostic::builder()
                .rule_id(RuleId::new("lint.a"))
                .severity(Severity::Error)
                .message("a variable named `a`")
                .span(span.clone())
                .build();
            let replacement = match answer {
                Answer::Report => None,
                Answer::ToAa => Some("aa"),
                Answer::ToB => Some("b"),
                Answer::ToC => Some("c"),
            };
            let diagnostic = match replacement {
                Some(replacement) => {
                    let edit = Edit::builder().span(span).replacement(replacement).build();
                    let fix = Fix::builder()
                        .message("rename the variable")
                        .edits(vec![edit])
                        .build()
                        .expect("the fix should build");
                    diagnostic.with_fix(fix).expect("the fix should attach")
                }
                None => diagnostic,
            };
            vec![diagnostic]
        }
    }

    #[test]
    fn assert_example_with_a_finding_without_a_fix_passes() {
        ExamplePair::read(BLOCK).assert_example(rule(Answer::Report));
    }

    #[test]
    #[should_panic(expected = "should produce the fix half")]
    fn assert_example_with_a_fix_that_gives_other_text_panics() {
        ExamplePair::read(BLOCK).assert_example(rule(Answer::ToC));
    }

    #[test]
    fn assert_example_with_a_fix_that_gives_the_fix_half_passes() {
        ExamplePair::read(BLOCK).assert_example(rule(Answer::ToB));
    }

    #[test]
    #[should_panic(expected = "did not settle within 10 rounds")]
    fn assert_example_with_a_fix_that_never_settles_panics() {
        ExamplePair::read(BLOCK).assert_example(rule(Answer::ToAa));
    }

    #[test]
    #[should_panic(expected = "should report the example")]
    fn assert_example_without_a_finding_panics() {
        ExamplePair::read(BLOCK).assert_example(|_| Vec::new());
    }

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

        assert_eq!(pair.example, "let a = 1;\n");
    }

    #[test]
    fn read_with_an_earlier_text_block_skips_it() {
        let source = format!("/// ```text\n/// plain\n/// ```\n{BLOCK}");

        let pair = ExamplePair::read(&source);

        assert_eq!(pair.example, "let a = 1;\n");
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
