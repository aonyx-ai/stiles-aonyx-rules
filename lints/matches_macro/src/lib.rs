use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity};

/// The advice for a `matches!` whose `bool` is the value the code wants
const VALUE_MESSAGE: &str = "use a full `match` expression instead of `matches!`";

/// The advice for a `matches!` whose `bool` only decides a branch
const CONDITION_MESSAGE: &str =
    "let a full `match` carry this branch instead of testing a `bool` from `matches!`";

/// Flags uses of the `matches!` macro
///
/// The `matches!` macro hides the full match expression, making it easy to
/// miss unhandled variants when an enum gains new members. A full `match`
/// expression forces you to consider each variant deliberately.
///
/// Every call is reported, but the advice depends on where the call sits.
/// When it is the condition of an `if` or a `while`, or a match guard, the
/// `bool` exists only to pick a branch. A `match` there carries the branch
/// bodies itself, so a new variant leads the compiler to the code that
/// decides about it. The `bool` stays a condition through `!`, `&&`, `||`,
/// parentheses, and a let chain. Anywhere else the `bool` is the value the
/// code wants, and a `match` whose arms yield `true` or `false` replaces it.
///
/// A `match` that yields a `bool` to a name the `if` then tests would also
/// silence the rule, but the `bool` stays in between. The condition message
/// therefore asks for the branch. A `matches!` bound by a `let` stays
/// reported, so moving the call out of the condition does not silence the
/// rule either.
///
/// The value rewrite conflicts with clippy's `match_like_matches_macro`,
/// which is on by default. Clippy suggests `matches!` for a `match` whose
/// arms all yield `bool` literals and whose last arm is a wildcard. A
/// `match` that names every variant passes both checks. A `match` on a
/// pattern that needs a wildcard, such as `Some(5)`, passes only when the
/// workspace sets `match_like_matches_macro = "allow"` in
/// `[workspace.lints.clippy]`.
pub struct MatchesMacro;

impl RustLintPass for MatchesMacro {
    fn check_macro_invocation(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(macro_node) = node.child_by_field_name("macro") else {
            return Vec::new();
        };
        if macro_node.kind() != "identifier" {
            return Vec::new();
        }
        if macro_node.text() != "matches" {
            return Vec::new();
        }

        let message = match position(node) {
            Position::Condition => CONDITION_MESSAGE,
            Position::Value => VALUE_MESSAGE,
        };

        vec![Diagnostic::new(
            RuleId::new("lint.matches-macro"),
            Severity::Warn,
            message.into(),
            node.span(),
        )]
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for MatchesMacro {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.matches-macro")]
    }
}

whisker_plugin::export_lints![rust: [MatchesMacro]];

/// Where a `matches!` call sits, which decides the rewrite its finding asks for
enum Position {
    /// The `bool` decides a branch and nothing else
    Condition,
    /// The `bool` is kept, returned, compared, or passed on
    Value,
}

/// Returns the position of the expression `node`
///
/// The walk climbs through the expressions that keep a `bool` a condition:
/// parentheses, `!`, `&&`, and `||`. It stops at the first ancestor of any
/// other kind, which then decides the position.
fn position(node: &DecoratedNode<'_>) -> Position {
    let mut current = *node;
    loop {
        let Some(parent) = current.parent() else {
            return Position::Value;
        };
        match parent.kind() {
            "parenthesized_expression" => {}
            "unary_expression" if is_negation(&parent) => {}
            "binary_expression" if is_logical(&parent) => {}
            "let_chain" => return Position::Condition,
            "if_expression" | "while_expression" | "match_pattern" => {
                return position_in(&parent, &current);
            }
            _ => return Position::Value,
        }
        current = parent;
    }
}

/// Returns the position of `child` inside an `if`, a `while`, or a match arm
///
/// Only the condition counts. A call in the body of the same `if` or `while`
/// yields a value like any other.
fn position_in(parent: &DecoratedNode<'_>, child: &DecoratedNode<'_>) -> Position {
    let Some(condition) = parent.child_by_field_name("condition") else {
        return Position::Value;
    };
    if condition.id() == child.id() {
        Position::Condition
    } else {
        Position::Value
    }
}

/// Returns whether a unary expression is a logical `!`
fn is_negation(node: &DecoratedNode<'_>) -> bool {
    node.child(0).is_some_and(|operator| operator.kind() == "!")
}

/// Returns whether a binary expression is a logical `&&` or `||`
fn is_logical(node: &DecoratedNode<'_>) -> bool {
    node.child_by_field_name("operator")
        .is_some_and(|operator| operator.kind() == "&&" || operator.kind() == "||")
}

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(MatchesMacro))]
    }

    fn messages_for(source: &str) -> Vec<String> {
        let tree = parse(source, Language::Rust);
        execute(&tree, &mut passes())
            .iter()
            .map(|diagnostic| diagnostic.message().to_owned())
            .collect()
    }

    #[test]
    fn assert_is_not_flagged() {
        let tree = parse("fn f() { assert!(true); }", Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn matches_as_else_if_condition_asks_for_branch() {
        let source = "fn f() { if a {} else if matches!(x, Some(5)) {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_as_if_condition_asks_for_branch() {
        let source = "fn f() { if matches!(x, Some(5)) { g(); } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_as_if_condition_has_rule_id_and_severity() {
        let tree = parse("fn f() { if matches!(x, Some(5)) {} }", Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.matches-macro")
            .has_severity(Severity::Warn)
            .message_contains("branch");
    }

    #[test]
    fn matches_as_if_condition_reports_one_diagnostic() {
        let tree = parse("fn f() { if matches!(x, Some(5)) {} }", Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn matches_as_match_guard_asks_for_branch() {
        let source = "fn f() { match y { Some(z) if matches!(z, A) => {} _ => {} } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_as_scrutinee_of_let_condition_asks_for_value() {
        let source = "fn f() { if let true = matches!(x, A) {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_as_scrutinee_of_match_asks_for_value() {
        let source = "fn f() { match matches!(x, A) { true => g(), false => h() } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_as_while_condition_asks_for_branch() {
        let source = "fn f() { while matches!(it.peek(), Some(' ')) { it.next(); } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_compared_in_if_condition_asks_for_value() {
        let source = "fn f() { if matches!(x, A) == expected {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_argument_of_if_condition_asks_for_value() {
        let source = "fn f() { if check(matches!(x, A)) {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_closure_body_asks_for_value() {
        let source = "fn f() { xs.iter().filter(|x| matches!(x, A)); }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_function_argument_asks_for_value() {
        let source = "fn f() { foo(matches!(x, 1)); }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_if_body_asks_for_value() {
        let source = "fn f() -> bool { if c { matches!(x, A) } else { false } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_let_binding_asks_for_value() {
        let source = "fn f() { let _ = matches!(x, Some(_)); }";
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_eq!(diagnostics[0].message(), VALUE_MESSAGE);
    }

    #[test]
    fn matches_in_let_binding_has_rule_id_and_severity() {
        let source = "fn f() { let _ = matches!(x, Some(_)); }";
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.matches-macro")
            .has_severity(Severity::Warn)
            .message_contains("match");
    }

    #[test]
    fn matches_in_let_binding_reports_one_diagnostic() {
        let source = "fn f() { let _ = matches!(x, Some(_)); }";
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn matches_in_let_binding_tested_by_next_if_is_flagged() {
        let source = "fn f() { let is_five = matches!(x, Some(5)); if is_five {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_let_chain_asks_for_branch() {
        let source = "fn f() { if let Some(y) = x && matches!(y, A) {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_in_let_chain_before_let_asks_for_branch() {
        let source = "fn f() { if a && matches!(x, A) && let Some(y) = z {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_in_logical_and_condition_asks_for_branch() {
        let source = "fn f() { if ready && matches!(x, A) {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_in_logical_and_let_binding_asks_for_value() {
        let source = "fn f() { let ok = ready && matches!(x, A); }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_logical_or_condition_asks_for_branch() {
        let source = "fn f() { while matches!(x, A) || done {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_in_match_arm_body_asks_for_value() {
        let source = "fn f() -> bool { match y { Some(z) => matches!(z, A), None => false } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_negated_condition_asks_for_branch() {
        let source = "fn f() { if !matches!(x, A) { return; } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_in_negated_let_binding_asks_for_value() {
        let source = "fn f() { let other = !matches!(x, A); }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_nested_conditions_asks_for_branch_each_time() {
        let source = "fn f() { if matches!(x, A) { while matches!(y, B) {} } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE, CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_in_parenthesized_condition_asks_for_branch() {
        let source = "fn f() { if !(ready || (matches!(x, A))) {} }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![CONDITION_MESSAGE]);
    }

    #[test]
    fn matches_in_return_expression_asks_for_value() {
        let source = "fn f() -> bool { if c { return matches!(x, A); } false }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_return_position_asks_for_value() {
        let source = "fn f() -> bool { matches!(x, Some(_)) }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_in_while_body_asks_for_value() {
        let source = "fn f() { while c { flags.push(matches!(x, A)); } }";

        let messages = messages_for(source);

        assert_eq!(messages, vec![VALUE_MESSAGE]);
    }

    #[test]
    fn matches_with_multiple_patterns_is_flagged() {
        let source = "fn f() { let _ = matches!(x, Foo | Bar); }";
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn println_is_not_flagged() {
        let tree = parse("fn f() { println!(\"hello\"); }", Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn regular_match_expression_is_not_flagged() {
        let source = "fn f() { match x { 0 => {} _ => {} } }";
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send_matches_macro() {
        fn assert_send<T: Send>() {}
        assert_send::<MatchesMacro>();
    }

    #[test]
    fn trait_send_position() {
        fn assert_send<T: Send>() {}
        assert_send::<Position>();
    }

    #[test]
    fn trait_sync_matches_macro() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<MatchesMacro>();
    }

    #[test]
    fn trait_sync_position() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Position>();
    }

    #[test]
    fn trait_unpin_matches_macro() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<MatchesMacro>();
    }

    #[test]
    fn trait_unpin_position() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Position>();
    }
}
