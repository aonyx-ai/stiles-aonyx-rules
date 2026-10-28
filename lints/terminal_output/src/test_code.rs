use stiles_types::DecoratedNode;

/// Returns whether `node` sits in code that only a test build compiles
///
/// The walk climbs from the node to the file. At each level it reads the
/// attributes in front of the node and the inner attributes that open it.
/// A test attribute such as `#[test]` or `#[tokio::test]` counts, and so
/// does a `cfg` whose predicate cannot hold outside a test build.
pub(crate) fn is_test_code(node: &DecoratedNode<'_>) -> bool {
    let mut current = Some(*node);
    while let Some(level) = current {
        if outer_attributes(&level).iter().any(marks_test) {
            return true;
        }
        if inner_attributes(&level).iter().any(marks_test) {
            return true;
        }
        current = level.parent();
    }
    false
}

/// Returns the attributes written in front of a node
///
/// Tree-sitter makes each outer attribute a sibling of the item it
/// decorates. The search reads backward over attributes and comments and
/// stops at the first sibling that is neither.
fn outer_attributes<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    let siblings = parent.named_children();
    let Some(index) = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())
    else {
        return Vec::new();
    };

    let mut attributes = Vec::new();
    for sibling in siblings[..index].iter().rev() {
        match sibling.kind() {
            "attribute_item" => attributes.push(*sibling),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    attributes
}

/// Returns the inner attributes that open a file, a module, or a block
///
/// `#![cfg(test)]` sits inside the scope it gates. The search stops at the
/// first child that is neither an inner attribute nor a comment.
fn inner_attributes<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let mut attributes = Vec::new();
    for child in node.named_children() {
        match child.kind() {
            "inner_attribute_item" => attributes.push(child),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    attributes
}

/// Returns whether an attribute item marks test code
///
/// An `attribute_item` and an `inner_attribute_item` both hold the
/// attribute as their first named child. A path whose last segment is
/// `test` or `bench` marks a test. A `cfg` marks one when its predicate
/// implies `test`. `cfg_attr` never does, because it adds an attribute
/// rather than removing the item.
fn marks_test(item: &DecoratedNode<'_>) -> bool {
    let Some(attribute) = item.named_child(0) else {
        return false;
    };
    let Some(path) = attribute.named_child(0) else {
        return false;
    };
    let compact: String = path.text().chars().filter(|c| !c.is_whitespace()).collect();
    let name = compact.rsplit("::").next().unwrap_or_default();

    match name {
        "test" | "bench" => true,
        "cfg" if compact == "cfg" => attribute
            .child_by_field_name("arguments")
            .is_some_and(|arguments| implies_test(arguments.text())),
        _ => false,
    }
}

/// Returns whether a `cfg` predicate holds only in a test build
///
/// `test` does, `all(..)` does when one of its predicates does, and
/// `any(..)` does when every one of its predicates does. `not(..)` and any
/// other predicate do not. A predicate the parser cannot read does not
/// either, because an exemption needs proof. The text is the attribute's
/// argument list, parentheses included.
fn implies_test(arguments: &str) -> bool {
    let tokens = tokenize(arguments);
    let mut parser = Parser {
        tokens: &tokens,
        next: 0,
    };

    let Some(Token::Open) = parser.take() else {
        return false;
    };
    let Some(predicate) = parser.predicate() else {
        return false;
    };
    let Some(Token::Close) = parser.take() else {
        return false;
    };
    if parser.take().is_some() {
        return false;
    }
    predicate
}

/// One token of a `cfg` predicate
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
enum Token<'a> {
    Word(&'a str),
    Text,
    Open,
    Close,
    Comma,
    Equals,
    Other,
}

/// Splits a `cfg` argument list into tokens
///
/// String contents never matter to the answer, so a string becomes one
/// [`Token::Text`]. An escaped quote inside it is skipped.
fn tokenize(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();

    while let Some((start, c)) = chars.next() {
        match c {
            '(' => tokens.push(Token::Open),
            ')' => tokens.push(Token::Close),
            ',' => tokens.push(Token::Comma),
            '=' => tokens.push(Token::Equals),
            '"' => {
                while let Some((_, inner)) = chars.next() {
                    match inner {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
                tokens.push(Token::Text);
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut end = start + c.len_utf8();
                while let Some(&(index, next)) = chars.peek() {
                    if !(next.is_alphanumeric() || next == '_') {
                        break;
                    }
                    end = index + next.len_utf8();
                    chars.next();
                }
                tokens.push(Token::Word(&text[start..end]));
            }
            c if c.is_whitespace() => {}
            _ => tokens.push(Token::Other),
        }
    }
    tokens
}

/// A recursive descent over the tokens of one `cfg` predicate
struct Parser<'t, 'a> {
    tokens: &'t [Token<'a>],
    next: usize,
}

impl<'a> Parser<'_, 'a> {
    /// Returns the next token and moves past it
    fn take(&mut self) -> Option<Token<'a>> {
        let token = self.tokens.get(self.next)?.clone();
        self.next += 1;
        Some(token)
    }

    /// Returns the next token without moving past it
    fn peek(&self) -> Option<&Token<'a>> {
        self.tokens.get(self.next)
    }

    /// Reads one predicate and returns whether it implies `test`
    ///
    /// Returns [`None`] when the tokens do not form a predicate.
    fn predicate(&mut self) -> Option<bool> {
        let Some(Token::Word(name)) = self.take() else {
            return None;
        };

        match self.peek() {
            Some(Token::Equals) => {
                self.take();
                let Some(Token::Text) = self.take() else {
                    return None;
                };
                Some(false)
            }
            Some(Token::Open) => {
                self.take();
                let operands = self.operands()?;
                match name {
                    "all" => Some(operands.iter().any(|implies| *implies)),
                    "any" => Some(!operands.is_empty() && operands.iter().all(|implies| *implies)),
                    _ => Some(false),
                }
            }
            Some(Token::Word(_) | Token::Text | Token::Close | Token::Comma | Token::Other)
            | None => Some(name == "test"),
        }
    }

    /// Reads a comma-separated list of predicates up to its closing `)`
    fn operands(&mut self) -> Option<Vec<bool>> {
        let mut operands = Vec::new();
        loop {
            if let Some(Token::Close) = self.peek() {
                self.take();
                return Some(operands);
            }
            operands.push(self.predicate()?);
            match self.take()? {
                Token::Comma => {}
                Token::Close => return Some(operands),
                Token::Word(_) => return None,
                Token::Text => return None,
                Token::Open => return None,
                Token::Equals => return None,
                Token::Other => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implies_test_with_all_holding_test_returns_true() {
        let implies = implies_test("(all(unix, test))");

        assert!(implies);
    }

    #[test]
    fn implies_test_with_all_without_test_returns_false() {
        let implies = implies_test("(all(unix, feature = \"test\"))");

        assert!(!implies);
    }

    #[test]
    fn implies_test_with_any_of_only_test_returns_true() {
        let implies = implies_test("(any(test, all(test, unix)))");

        assert!(implies);
    }

    #[test]
    fn implies_test_with_any_with_other_predicate_returns_false() {
        let implies = implies_test("(any(test, debug_assertions))");

        assert!(!implies);
    }

    #[test]
    fn implies_test_with_empty_any_returns_false() {
        let implies = implies_test("(any())");

        assert!(!implies);
    }

    #[test]
    fn implies_test_with_escaped_quote_in_value_returns_true() {
        let implies = implies_test(r#"(all(feature = "a\")b", test))"#);

        assert!(implies);
    }

    #[test]
    fn implies_test_with_extra_tokens_returns_false() {
        let implies = implies_test("(test) x");

        assert!(!implies);
    }

    #[test]
    fn implies_test_with_missing_close_returns_false() {
        let implies = implies_test("(all(test, unix)");

        assert!(!implies);
    }

    #[test]
    fn implies_test_with_negation_returns_false() {
        let implies = implies_test("(not(test))");

        assert!(!implies);
    }

    #[test]
    fn implies_test_with_other_word_returns_false() {
        let implies = implies_test("(tests)");

        assert!(!implies);
    }

    #[test]
    fn implies_test_with_test_returns_true() {
        let implies = implies_test("( test )");

        assert!(implies);
    }

    #[test]
    fn implies_test_with_value_named_test_returns_false() {
        let implies = implies_test("(test = \"yes\")");

        assert!(!implies);
    }

    #[test]
    fn tokenize_with_string_returns_one_text_token() {
        let tokens = tokenize("(feature = \"a, b\")");

        assert_eq!(
            tokens,
            vec![
                Token::Open,
                Token::Word("feature"),
                Token::Equals,
                Token::Text,
                Token::Close,
            ]
        );
    }

    #[test]
    fn trait_send_parser() {
        fn assert_send<T: Send>() {}
        assert_send::<Parser<'static, 'static>>();
    }

    #[test]
    fn trait_send_token() {
        fn assert_send<T: Send>() {}
        assert_send::<Token<'static>>();
    }

    #[test]
    fn trait_sync_parser() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Parser<'static, 'static>>();
    }

    #[test]
    fn trait_sync_token() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Token<'static>>();
    }

    #[test]
    fn trait_unpin_parser() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Parser<'static, 'static>>();
    }

    #[test]
    fn trait_unpin_token() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Token<'static>>();
    }
}
