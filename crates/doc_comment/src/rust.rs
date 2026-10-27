use whisker_types::DecoratedNode;

use crate::{DocLine, strip_star, unindent};

/// Which marker introduces a Rust doc comment
///
/// A `///` run documents the item below it. A `//!` run documents the item
/// around it. The two therefore never form one comment, even when they touch.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum Marker {
    Inner,
    Outer,
}

/// Returns the prose lines of a Rust doc comment, or [`None`] when `node`
/// does not start one
///
/// A block doc comment, `/** */` or `/*! */`, is one node. Each of its lines
/// loses the `*` that opens it. Rustdoc reads a run of `///` lines on
/// consecutive rows as one comment, and a run of `//!` lines too. The first
/// line of a run therefore returns the whole run, and a later line returns
/// [`None`]. A rule
/// that checks every comment node thus reads each doc comment once. The
/// lines lose the indentation they share, such as the space after `///`.
///
/// A plain comment returns [`None`].
///
/// [`None`]: std::option::Option::None
///
/// # Examples
///
/// ```
/// use whisker_testing::parse;
/// use whisker_types::Language;
///
/// let tree = parse("/// Adds two\n/// numbers\nfn add() {}", Language::Rust);
/// let first = tree.root_node().named_child(0).unwrap();
/// let second = tree.root_node().named_child(1).unwrap();
///
/// let lines = doc_comment::rust(&first).unwrap();
///
/// let texts: Vec<&str> = lines.iter().map(|line| line.text()).collect();
/// assert_eq!(texts, ["Adds two", "numbers"]);
/// assert!(doc_comment::rust(&second).is_none());
/// ```
pub fn rust<'a>(node: &DecoratedNode<'a>) -> Option<Vec<DocLine<'a>>> {
    let mut lines = match node.kind() {
        "block_comment" => block_lines(node)?,
        "line_comment" => run_lines(node)?,
        _ => return None,
    };

    unindent(&mut lines);
    Some(lines)
}

/// Returns the marker of a doc comment, or [`None`] for a plain comment
///
/// The grammar records the documentation in the `doc` field and the marker
/// in `outer` or `inner`.
///
/// [`None`]: std::option::Option::None
fn marker(node: &DecoratedNode<'_>) -> Option<Marker> {
    node.child_by_field_name("doc")?;

    match node.child_by_field_name("inner") {
        Some(_) => Some(Marker::Inner),
        None => node.child_by_field_name("outer").map(|_| Marker::Outer),
    }
}

/// Returns the lines of a block doc comment, each without its leading `*`
fn block_lines<'a>(node: &DecoratedNode<'a>) -> Option<Vec<DocLine<'a>>> {
    marker(node)?;
    let doc = node.child_by_field_name("doc")?;
    let base = doc.span().start();

    let mut lines = Vec::new();
    let mut consumed = 0;
    for raw in doc.text().split('\n') {
        let text = raw.strip_suffix('\r').unwrap_or(raw);
        lines.push(strip_star(DocLine {
            text,
            offset: base + consumed,
        }));
        consumed += raw.len() + 1;
    }

    Some(lines)
}

/// Returns the lines of the run of line doc comments that `node` starts
///
/// Returns [`None`] when `node` is not a line doc comment, or when the line
/// above it belongs to the same run.
fn run_lines<'a>(node: &DecoratedNode<'a>) -> Option<Vec<DocLine<'a>>> {
    let marker = marker(node)?;
    let parent = node.parent()?;
    let siblings = parent.named_children();
    let index = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())?;

    if index > 0 && continues(&siblings[index - 1], node, marker) {
        return None;
    }

    let mut lines = vec![line_of(node)?];
    for pair in siblings[index..].windows(2) {
        let [previous, next] = pair else {
            break;
        };
        if !continues(previous, next, marker) {
            break;
        }
        lines.push(line_of(next)?);
    }

    Some(lines)
}

/// Returns whether `next` continues the run that `previous` is part of
///
/// Both carry `marker`, and `next` starts on the row after `previous`.
fn continues(previous: &DecoratedNode<'_>, next: &DecoratedNode<'_>, marker: Marker) -> bool {
    let same_kind = previous.kind() == "line_comment" && next.kind() == "line_comment";
    let same_marker = self::marker(previous) == Some(marker) && self::marker(next) == Some(marker);

    same_kind
        && same_marker
        && previous.raw().start_position().row + 1 == next.raw().start_position().row
}

/// Returns the documentation of one `///` or `//!` line
///
/// The `doc` field also covers the line's newline, which the line drops.
fn line_of<'a>(node: &DecoratedNode<'a>) -> Option<DocLine<'a>> {
    let doc = node.child_by_field_name("doc")?;
    let text = doc.text();
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = text.strip_suffix('\r').unwrap_or(text);

    Some(DocLine {
        text,
        offset: doc.span().start(),
    })
}

#[cfg(test)]
mod tests {
    use whisker_testing::parse;
    use whisker_types::{DecoratedTree, Language};

    use super::*;

    /// Returns the texts that `rust` reads from each top-level node in turn
    fn texts_of(source: &str) -> Vec<Option<Vec<String>>> {
        let tree: DecoratedTree = parse(source, Language::Rust);

        tree.root_node()
            .named_children()
            .iter()
            .map(|node| {
                rust(node).map(|lines| lines.iter().map(|line| line.text().to_owned()).collect())
            })
            .collect()
    }

    #[test]
    fn rust_with_a_blank_line_splits_the_run() {
        let texts = texts_of("/// one\n\n/// two\nfn f() {}");

        assert_eq!(texts[0], Some(vec!["one".to_owned()]));
        assert_eq!(texts[1], Some(vec!["two".to_owned()]));
    }

    #[test]
    fn rust_with_a_block_doc_comment_strips_the_stars() {
        let texts = texts_of("/**\n * Adds two\n * numbers\n */\nfn f() {}");

        assert_eq!(
            texts[0],
            Some(vec![
                String::new(),
                "Adds two".to_owned(),
                "numbers".to_owned(),
                String::new()
            ])
        );
    }

    #[test]
    fn rust_with_a_plain_comment_returns_none() {
        let texts = texts_of("// not documentation\n/* nor this */\nfn f() {}");

        assert_eq!(texts[0], None);
        assert_eq!(texts[1], None);
    }

    #[test]
    fn rust_with_inner_and_outer_runs_keeps_them_apart() {
        let texts = texts_of("//! inner\n/// outer\nfn f() {}");

        assert_eq!(texts[0], Some(vec!["inner".to_owned()]));
        assert_eq!(texts[1], Some(vec!["outer".to_owned()]));
    }

    #[test]
    fn rust_with_a_run_returns_it_from_the_first_line_only() {
        let texts = texts_of("/// one\n/// two\n/// three\nfn f() {}");

        assert_eq!(
            texts[0],
            Some(vec!["one".to_owned(), "two".to_owned(), "three".to_owned()])
        );
        assert_eq!(texts[1], None);
        assert_eq!(texts[2], None);
    }

    #[test]
    fn rust_keeps_the_offset_of_each_line() {
        let source = "/// one\n///   two\nfn f() {}";
        let tree = parse(source, Language::Rust);
        let first = tree.root_node().named_child(0).expect("a comment");

        let lines = rust(&first).expect("a doc comment");

        assert_eq!(lines[1].text(), "  two");
        assert_eq!(&source[lines[1].offset()..][..5], "  two");
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Marker>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Marker>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Marker>();
    }
}
