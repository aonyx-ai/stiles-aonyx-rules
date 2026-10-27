use crate::doc_line::DocLine;

/// Abbreviations that end in a period but never end a sentence
///
/// `is_abbreviation` covers forms with an inner period, such as `e.g.` and
/// `i.e.`, so this list omits them. It also omits `etc.`, which ends a
/// sentence as often as not.
const ABBREVIATIONS: [&str; 6] = ["approx.", "cf.", "fig.", "resp.", "viz.", "vs."];

/// Characters that may sit between a sentence terminator and the whitespace
const CLOSERS: [u8; 9] = *b".!?)]\"'*_";

/// Markers that open a Markdown list item or a block quote
const LIST_MARKERS: [&str; 4] = ["- ", "* ", "+ ", "> "];

/// Characters that can end a sentence
const TERMINATORS: [u8; 3] = *b".!?";

/// One sentence of doc comment prose
///
/// The range is in file bytes, so it covers the `///` markers of the lines
/// the sentence continues onto. A [`Span`] is one contiguous range, so the
/// markers cannot be cut out.
///
/// [`Span`]: whisker_types::Span
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub(crate) struct Sentence {
    words: usize,
    start: usize,
    end: usize,
}

impl Sentence {
    /// Returns how many words the sentence has
    pub(crate) fn words(&self) -> usize {
        self.words
    }

    /// Returns the byte offset where the sentence starts in the file
    pub(crate) fn start(&self) -> usize {
        self.start
    }

    /// Returns the byte offset one past the end of the sentence
    pub(crate) fn end(&self) -> usize {
        self.end
    }
}

/// Splits the prose of one doc comment into sentences
///
/// Fenced code blocks, section headings, link definitions, and table rows
/// are not prose, so no sentence contains them. A blank line or a new list
/// item ends the current sentence, because Markdown reads them as a break
/// even without a period.
pub(crate) fn sentences(lines: &[DocLine<'_>]) -> Vec<Sentence> {
    let mut found = Vec::new();
    let mut prose = Prose::default();
    let mut fence = None;

    for line in lines {
        let text = line.text();
        let trimmed = text.trim_start();

        match (fence, Fence::of(trimmed)) {
            (None, None) => {}
            (None, Some(opened)) => {
                fence = Some(opened);
                prose.flush(&mut found);
                continue;
            }
            (Some(_), None) => continue,
            (Some(opened), Some(candidate)) => {
                fence = match candidate.closes(opened) {
                    true => None,
                    false => Some(opened),
                };
                continue;
            }
        }

        if trimmed.is_empty()
            || is_heading(trimmed)
            || is_link_definition(trimmed)
            || trimmed.starts_with('|')
        {
            prose.flush(&mut found);
            continue;
        }

        let indent = text.len() - trimmed.len();
        let marker = list_marker(trimmed);
        if marker > 0 {
            prose.flush(&mut found);
        }
        let start = indent + marker;
        prose.push(&text[start..], line.offset() + start);
    }

    prose.flush(&mut found);
    found
}

/// The prose of one paragraph, reassembled from the lines it spans
///
/// Each line keeps the file offset it came from, so a sentence can point
/// back into the file. The segments stay sorted by their position in `text`,
/// which the offset lookup relies on.
#[derive(Clone, Eq, PartialEq, Debug, Default)]
struct Prose {
    text: String,
    segments: Vec<(usize, usize)>,
}

impl Prose {
    /// Appends one line of prose that starts at `offset` in the file
    fn push(&mut self, text: &str, offset: usize) {
        let indent = text.len() - text.trim_start().len();
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.segments.push((self.text.len(), offset + indent));
        self.text.push_str(text);
    }

    /// Appends the sentences of the paragraph to `found` and empties it
    fn flush(&mut self, found: &mut Vec<Sentence>) {
        let masked = mask_code_spans(&self.text);
        for (start, end) in split(&masked) {
            found.push(Sentence {
                words: count_words(&masked[start..end]),
                start: self.offset(start),
                end: self.offset(end),
            });
        }

        self.text.clear();
        self.segments.clear();
    }

    /// Maps an offset in `text` to the file offset it came from
    ///
    /// # Panics
    ///
    /// Panics if the paragraph holds no segments. Only [`Prose::flush`] calls
    /// this, and a paragraph without segments has no text to split, so it asks
    /// for no offsets.
    fn offset(&self, offset: usize) -> usize {
        let index = self
            .segments
            .partition_point(|(position, _)| *position <= offset);
        let (position, file) = self.segments[index - 1];
        file + (offset - position)
    }
}

/// The run of backticks or tildes that opens or closes a code block
///
/// Treating every fence line as a toggle ends a block at the first fence line
/// inside it. Markdown closes a block only with the character that opened it,
/// repeated at least as many times, and with no language label.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
struct Fence {
    character: u8,
    length: usize,
    labeled: bool,
}

impl Fence {
    /// Returns the fence the line carries, or nothing when it is prose
    fn of(text: &str) -> Option<Self> {
        let character = *text.as_bytes().first()?;
        if character != b'`' && character != b'~' {
            return None;
        }

        let length = text.bytes().take_while(|byte| *byte == character).count();
        if length < 3 {
            return None;
        }

        Some(Self {
            character,
            length,
            labeled: !text[length..].trim().is_empty(),
        })
    }

    /// Returns whether this fence closes the block that `opened` started
    fn closes(&self, opened: Self) -> bool {
        self.character == opened.character && self.length >= opened.length && !self.labeled
    }
}

/// Returns whether the line is a rustdoc section heading such as `# Errors`
fn is_heading(text: &str) -> bool {
    let rest = text.trim_start_matches('#');
    rest.len() < text.len() && (rest.is_empty() || rest.starts_with(' '))
}

/// Returns whether the line defines a reference-style Markdown link
fn is_link_definition(text: &str) -> bool {
    text.starts_with('[') && text.contains("]:")
}

/// Returns the length of the list or quote marker that opens the line
///
/// A line that opens neither yields zero.
fn list_marker(text: &str) -> usize {
    for marker in LIST_MARKERS {
        if text.starts_with(marker) {
            return marker.len();
        }
    }

    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return 0;
    }
    let rest = &text[digits..];
    match rest.starts_with(". ") || rest.starts_with(") ") {
        true => digits + 2,
        false => 0,
    }
}

/// Replaces the inside of every inline code span with `x`, byte for byte
///
/// A code span may hold anything, including periods and spaces, so the
/// splitter must not read it. The replacement keeps every offset intact and
/// leaves the span as a single word.
fn mask_code_spans(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut masked = String::with_capacity(text.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'`' {
            let next = text[index..]
                .find('`')
                .map_or(bytes.len(), |offset| index + offset);
            masked.push_str(&text[index..next]);
            index = next;
            continue;
        }

        let fence = backtick_run(bytes, index);
        masked.push_str(&text[index..index + fence]);
        index += fence;

        let Some(close) = closing_backticks(bytes, index, fence) else {
            continue;
        };
        for _ in index..close {
            masked.push('x');
        }
        masked.push_str(&text[close..close + fence]);
        index = close + fence;
    }

    masked
}

/// Returns how many backticks start at `index`
fn backtick_run(bytes: &[u8], index: usize) -> usize {
    bytes[index..].iter().take_while(|&&b| b == b'`').count()
}

/// Finds the run of exactly `fence` backticks that closes a code span
///
/// A span that never closes yields nothing, and the text stays unmasked.
fn closing_backticks(bytes: &[u8], from: usize, fence: usize) -> Option<usize> {
    let mut index = from;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let run = backtick_run(bytes, index);
        if run == fence {
            return Some(index);
        }
        index += run;
    }
    None
}

/// Splits masked prose into sentence ranges
fn split(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut index = 0;

    while index < bytes.len() {
        if !TERMINATORS.contains(&bytes[index]) {
            index += 1;
            continue;
        }

        let mut end = index + 1;
        while end < bytes.len() && CLOSERS.contains(&bytes[end]) {
            end += 1;
        }
        if end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            index = end;
            continue;
        }
        if !ends_sentence(text, start, index, end) {
            index = end;
            continue;
        }

        ranges.push((start, end));
        let rest = &text[end..];
        start = end + (rest.len() - rest.trim_start().len());
        index = start;
    }

    if start < bytes.len() {
        ranges.push((start, bytes.len()));
    }
    ranges
}

/// Decides whether the terminator at `at` ends a sentence
///
/// An abbreviation and a sentence end look the same. For a period, the word
/// before it decides. A `!` or a `?` also needs the word after it, because a
/// macro name ends in `!` and prose writes a bare `?`.
///
/// The word after a period decides nothing. Rust prose starts sentences with
/// names such as `rust-analyzer`, so lower case is no sign that the sentence
/// goes on.
fn ends_sentence(text: &str, start: usize, at: usize, end: usize) -> bool {
    let word = text[start..at]
        .rfind(char::is_whitespace)
        .map_or(start, |offset| start + offset + 1);
    let word = &text[word..=at];
    if ABBREVIATIONS
        .iter()
        .any(|abbreviation| abbreviation.eq_ignore_ascii_case(word))
    {
        return false;
    }
    if is_abbreviation(word) {
        return false;
    }
    if text.as_bytes()[at] == b'.' {
        return true;
    }

    match text[end..].trim_start().chars().next() {
        None => true,
        Some(next) => !next.is_lowercase(),
    }
}

/// Returns whether the word is an abbreviation such as `e.g.` or `a.k.a.`
///
/// Only letters and periods qualify, so a version number such as `1.97.0.`
/// still ends a sentence.
fn is_abbreviation(word: &str) -> bool {
    let Some(stem) = word.strip_suffix('.') else {
        return false;
    };
    stem.contains('.') && stem.bytes().all(|b| b.is_ascii_alphabetic() || b == b'.')
}

/// Counts the words of one sentence
///
/// An inline code span counts as one word, because a reader takes
/// `Option<T>` or `crates/whisker-rust` as a single name. Punctuation on its
/// own is not a word.
fn count_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<DocLine<'_>> {
        let mut offset = 0;
        let mut lines = Vec::new();
        for line in text.split('\n') {
            lines.push(DocLine::builder().text(line).offset(offset).build());
            offset += line.len() + 1;
        }
        lines
    }

    fn words(text: &str) -> Vec<usize> {
        sentences(&lines(text))
            .iter()
            .map(Sentence::words)
            .collect()
    }

    #[test]
    fn count_words_ignores_bare_punctuation() {
        assert_eq!(count_words("a -- b"), 2);
    }

    #[test]
    fn count_words_treats_a_masked_code_span_as_one_word() {
        assert_eq!(count_words(&mask_code_spans("takes `Vec<String>` now")), 3);
    }

    #[test]
    fn fence_closes_accepts_a_longer_run() {
        let opened = Fence::of("```").expect("a fence");
        let candidate = Fence::of("`````").expect("a fence");

        let closes = candidate.closes(opened);

        assert!(closes);
    }

    #[test]
    fn fence_closes_rejects_a_different_character() {
        let opened = Fence::of("```").expect("a fence");
        let candidate = Fence::of("~~~").expect("a fence");

        let closes = candidate.closes(opened);

        assert!(!closes);
    }

    #[test]
    fn fence_closes_rejects_a_labeled_run() {
        let opened = Fence::of("```").expect("a fence");
        let candidate = Fence::of("```rust").expect("a fence");

        let closes = candidate.closes(opened);

        assert!(!closes);
    }

    #[test]
    fn fence_closes_rejects_a_shorter_run() {
        let opened = Fence::of("````").expect("a fence");
        let candidate = Fence::of("```").expect("a fence");

        let closes = candidate.closes(opened);

        assert!(!closes);
    }

    #[test]
    fn fence_of_rejects_a_short_run() {
        assert!(Fence::of("`` code").is_none());
    }

    #[test]
    fn fence_of_rejects_prose() {
        assert!(Fence::of("Returns the length").is_none());
    }

    #[test]
    fn is_abbreviation_accepts_a_three_letter_abbreviation() {
        assert!(is_abbreviation("a.k.a."));
    }

    #[test]
    fn is_abbreviation_accepts_inner_periods() {
        assert!(is_abbreviation("e.g."));
    }

    #[test]
    fn is_abbreviation_rejects_a_plain_word() {
        assert!(!is_abbreviation("word."));
    }

    #[test]
    fn is_abbreviation_rejects_a_version_number() {
        assert!(!is_abbreviation("1.97.0."));
    }

    #[test]
    fn is_heading_accepts_a_second_level_heading() {
        assert!(is_heading("## Panics"));
    }

    #[test]
    fn is_heading_accepts_a_section_heading() {
        assert!(is_heading("# Errors"));
    }

    #[test]
    fn is_heading_rejects_an_issue_reference() {
        assert!(!is_heading("#239 changed this"));
    }

    #[test]
    fn is_link_definition_accepts_a_reference_target() {
        assert!(is_link_definition("[`io::Error`]: std::io::Error"));
    }

    #[test]
    fn is_link_definition_rejects_a_reference_use() {
        assert!(!is_link_definition("[the walker][ignore] honors gitignore"));
    }

    #[test]
    fn list_marker_measures_an_ordered_item() {
        assert_eq!(list_marker("12. first"), 4);
    }

    #[test]
    fn list_marker_measures_an_unordered_item() {
        assert_eq!(list_marker("- first"), 2);
    }

    #[test]
    fn list_marker_rejects_a_decimal() {
        assert_eq!(list_marker("1.5 seconds"), 0);
    }

    #[test]
    fn mask_code_spans_hides_a_period_inside_a_span() {
        assert_eq!(mask_code_spans("see `a. b` now"), "see `xxxx` now");
    }

    #[test]
    fn mask_code_spans_leaves_an_unclosed_span_alone() {
        assert_eq!(mask_code_spans("a ` b c"), "a ` b c");
    }

    #[test]
    fn mask_code_spans_respects_a_double_backtick_span() {
        assert_eq!(mask_code_spans("``a ` b`` c"), "``xxxxx`` c");
    }

    #[test]
    fn sentences_counts_a_sentence_that_spans_lines() {
        assert_eq!(words("one two three\nfour five six."), vec![6]);
    }

    #[test]
    fn sentences_ignores_a_fenced_code_block() {
        let text = "Text here.\n\n```\nlet a = 1. let b = 2;\n```\n\nMore text.";

        assert_eq!(words(text), vec![2, 2]);
    }

    #[test]
    fn sentences_ignores_a_link_definition() {
        assert_eq!(
            words("Uses [`Span`] here.\n\n[`Span`]: whisker_types::Span"),
            vec![3]
        );
    }

    #[test]
    fn sentences_ignores_a_section_heading() {
        assert_eq!(words("# Errors\n\nReturns an error."), vec![3]);
    }

    #[test]
    fn sentences_ignores_a_shorter_fence_inside_a_longer_one() {
        let text = "Text here.\n\n````\n``` a. b\n````\n\nMore text.";

        assert_eq!(words(text), vec![2, 2]);
    }

    #[test]
    fn sentences_ignores_a_table_row() {
        assert_eq!(words("Text.\n\n| a | b |\n| - | - |"), vec![1]);
    }

    #[test]
    fn sentences_ignores_a_tilde_line_inside_a_backtick_fence() {
        let text = "Text here.\n\n```\n~~~\nlet a = 1. let b = 2;\n~~~\n```\n\nMore text.";

        assert_eq!(words(text), vec![2, 2]);
    }

    #[test]
    fn sentences_keeps_a_version_number_whole() {
        assert_eq!(words("Needs 1.97.0 or later"), vec![4]);
    }

    #[test]
    fn sentences_maps_offsets_of_the_first_sentence_back_to_the_file() {
        let found = sentences(&lines("Abc def.\nGhi."));

        assert_eq!((found[0].start(), found[0].end()), (0, 8));
    }

    #[test]
    fn sentences_maps_offsets_of_the_second_sentence_back_to_the_file() {
        let found = sentences(&lines("Abc def.\nGhi."));

        assert_eq!((found[1].start(), found[1].end()), (9, 13));
    }

    #[test]
    fn sentences_maps_two_lines_to_two_sentences() {
        let found = sentences(&lines("Abc def.\nGhi."));

        assert_eq!(found.len(), 2);
    }

    #[test]
    fn sentences_reports_nothing_for_empty_prose() {
        assert!(sentences(&lines("\n\n")).is_empty());
    }

    #[test]
    fn sentences_splits_at_a_list_item() {
        assert_eq!(
            words("The rules are\n- one thing\n- two things"),
            vec![3, 2, 2]
        );
    }

    #[test]
    fn sentences_splits_at_a_paragraph_break() {
        assert_eq!(words("One two\n\nthree four five"), vec![2, 3]);
    }

    #[test]
    fn sentences_splits_before_a_code_span() {
        assert_eq!(words("It runs. `Foo` stops."), vec![2, 2]);
    }

    #[test]
    fn sentences_splits_on_a_period() {
        assert_eq!(words("One two three. Four five."), vec![3, 2]);
    }

    #[test]
    fn sentences_treats_an_unnamed_abbreviation_as_one_sentence() {
        assert_eq!(words("Prefers a.k.a. Names over others."), vec![5]);
    }

    #[test]
    fn split_keeps_a_bare_macro_name_whole() {
        assert_eq!(words("The matches! macro hides the arms."), vec![6]);
    }

    #[test]
    fn split_keeps_a_capitalized_listed_abbreviation_whole() {
        assert_eq!(words("Compare Cf. Rust and this."), vec![5]);
    }

    #[test]
    fn split_keeps_a_listed_abbreviation_whole() {
        assert_eq!(words("Compare cf. Rust and this."), vec![5]);
    }

    #[test]
    fn split_keeps_a_path_whole() {
        assert_eq!(words("Reads std::io::Error from disk."), vec![4]);
    }

    #[test]
    fn split_keeps_a_url_whole() {
        assert_eq!(words("See https://flox.dev for more."), vec![4]);
    }

    #[test]
    fn split_keeps_e_g_whole() {
        assert_eq!(
            words("The kind, e.g. `function_item`, is a string."),
            vec![7]
        );
    }

    #[test]
    fn split_keeps_i_e_whole() {
        assert_eq!(words("One thing, i.e. the only thing."), vec![6]);
    }

    #[test]
    fn split_stops_at_a_lowercase_name() {
        assert_eq!(words("Interns it. rust-analyzer panics."), vec![2, 2]);
    }

    #[test]
    fn split_stops_at_a_question_mark() {
        assert_eq!(words("Did it run? It did."), vec![3, 2]);
    }

    #[test]
    fn trait_send_holds_for_fence() {
        fn assert_send<T: Send>() {}
        assert_send::<Fence>();
    }

    #[test]
    fn trait_send_holds_for_prose() {
        fn assert_send<T: Send>() {}
        assert_send::<Prose>();
    }

    #[test]
    fn trait_send_holds_for_sentence() {
        fn assert_send<T: Send>() {}
        assert_send::<Sentence>();
    }

    #[test]
    fn trait_sync_holds_for_fence() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Fence>();
    }

    #[test]
    fn trait_sync_holds_for_prose() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Prose>();
    }

    #[test]
    fn trait_sync_holds_for_sentence() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Sentence>();
    }

    #[test]
    fn trait_unpin_holds_for_fence() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Fence>();
    }

    #[test]
    fn trait_unpin_holds_for_prose() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Prose>();
    }

    #[test]
    fn trait_unpin_holds_for_sentence() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Sentence>();
    }
}
