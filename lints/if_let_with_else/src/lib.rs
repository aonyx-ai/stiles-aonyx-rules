use whisker_rust::{RuleId, RustLintPass};
use whisker_types::{DecoratedNode, Diagnostic, Severity};

/// Flags `if let` expressions that have a non-diverging `else` branch
///
/// An `if let` with an `else` branch is equivalent to a two-arm `match`
/// but obscures the pattern-matching intent. A `match` expression makes
/// both arms equally visible and encourages exhaustive handling.
///
/// `if let` without an `else` is acceptable for short, single-branch
/// pattern actions. `if let` with a diverging `else` (return, panic,
/// etc.) is also acceptable since that pattern maps to `let ... else`.
pub struct IfLetWithElse;

/// Diverging macro names that indicate the else branch will never
/// complete normally
const DIVERGING_MACROS: &[&str] = &["panic", "unreachable", "todo", "unimplemented"];

impl RustLintPass for IfLetWithElse {
    fn check_if_expression(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(condition) = node.child_by_field_name("condition") else {
            return Vec::new();
        };

        if !has_let_condition(&condition) {
            return Vec::new();
        }

        let Some(alternative) = node.child_by_field_name("alternative") else {
            return Vec::new();
        };

        if is_diverging(&alternative) {
            return Vec::new();
        }

        vec![
            Diagnostic::builder()
                .rule_id(RuleId::new("lint.if-let-with-else"))
                .severity(Severity::Warn)
                .message("`if let` with `else` should be written as a `match` expression")
                .span(node.span())
                .build(),
        ]
    }
}

/// Checks whether a condition node contains a `let` pattern
///
/// Returns `true` if the node itself is a `let_condition` or
/// `let_chain`, or if any of its named children are.
fn has_let_condition(node: &DecoratedNode<'_>) -> bool {
    let kind = node.kind();
    if kind == "let_condition" || kind == "let_chain" {
        return true;
    }
    for child in node.named_children() {
        if has_let_condition(&child) {
            return true;
        }
    }
    false
}

/// Checks whether an else clause diverges syntactically
///
/// An else clause diverges if its last statement or expression is a
/// `return`, `break`, `continue`, or a call to a known-diverging macro
/// like `panic!`, `unreachable!`, `todo!`, or `unimplemented!`.
fn is_diverging(else_clause: &DecoratedNode<'_>) -> bool {
    let Some(body) = else_clause.named_child(0) else {
        return false;
    };

    match body.kind() {
        "block" => is_block_diverging(&body),
        "if_expression" => false,
        _ => false,
    }
}

/// Checks whether a block's last statement/expression diverges
fn is_block_diverging(block: &DecoratedNode<'_>) -> bool {
    let children = block.named_children();
    let Some(last) = children.last() else {
        return false;
    };
    is_node_diverging(last)
}

/// Checks whether a single node represents a diverging expression
fn is_node_diverging(node: &DecoratedNode<'_>) -> bool {
    match node.kind() {
        "return_expression" | "break_expression" | "continue_expression" => true,
        "expression_statement" => {
            let Some(inner) = node.named_child(0) else {
                return false;
            };
            is_node_diverging(&inner)
        }
        "macro_invocation" => is_diverging_macro(node),
        _ => false,
    }
}

/// Checks whether a macro invocation calls a known-diverging macro
fn is_diverging_macro(node: &DecoratedNode<'_>) -> bool {
    let Some(macro_node) = node.child_by_field_name("macro") else {
        return false;
    };
    let name = macro_node.text();
    DIVERGING_MACROS.contains(&name)
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for IfLetWithElse {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.if-let-with-else")]
    }
}

whisker_plugin::export_lints![rust: [IfLetWithElse]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(IfLetWithElse))]
    }

    #[test]
    fn if_let_with_break_in_else_is_not_flagged() {
        let source = r#"
            fn f() {
                loop {
                    if let Some(x) = val {
                        use_x(x);
                    } else {
                        break;
                    }
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn if_let_with_continue_in_else_is_not_flagged() {
        let source = r#"
            fn f() {
                loop {
                    if let Some(x) = val {
                        use_x(x);
                    } else {
                        continue;
                    }
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn if_let_with_non_diverging_else_is_flagged_once() {
        let source = r#"
            fn f() {
                if let Some(x) = val {
                    use_x(x);
                } else {
                    do_something();
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn if_let_with_non_diverging_else_is_flagged_with_match_advice() {
        let source = r#"
            fn f() {
                if let Some(x) = val {
                    use_x(x);
                } else {
                    do_something();
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.if-let-with-else")
            .has_severity(Severity::Warn)
            .message_contains("match");
    }

    #[test]
    fn if_let_with_panic_in_else_is_not_flagged() {
        let source = r#"
            fn f() {
                if let Some(x) = val {
                    use_x(x);
                } else {
                    panic!("unexpected");
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn if_let_with_return_in_else_is_not_flagged() {
        let source = r#"
            fn f() {
                if let Some(x) = val {
                    use_x(x);
                } else {
                    return;
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn if_let_with_todo_in_else_is_not_flagged() {
        let source = r#"
            fn f() {
                if let Some(x) = val {
                    use_x(x);
                } else {
                    todo!();
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn if_let_with_unimplemented_in_else_is_not_flagged() {
        let source = r#"
            fn f() {
                if let Some(x) = val {
                    use_x(x);
                } else {
                    unimplemented!();
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn if_let_with_unreachable_in_else_is_not_flagged() {
        let source = r#"
            fn f() {
                if let Some(x) = val {
                    use_x(x);
                } else {
                    unreachable!();
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn if_let_without_else_is_not_flagged() {
        let source = r#"
            fn f() {
                if let Some(x) = val {
                    use_x(x);
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn regular_if_with_else_is_not_flagged() {
        let source = r#"
            fn f() {
                if condition {
                    do_a();
                } else {
                    do_b();
                }
            }
        "#;
        let tree = parse(source, Language::Rust);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<IfLetWithElse>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<IfLetWithElse>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<IfLetWithElse>();
    }
}
