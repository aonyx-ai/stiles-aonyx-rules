use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity};

const RULE_ID: RuleId = RuleId::new("lint.split-binding");

/// Flags a `let` binding that only exists to initialize a later binding
///
/// Names such as `raw_input` and `trimmed_input` split one value across two
/// bindings, and the reader then has to track which name holds which stage.
/// The rule fires when two conditions hold. Every read of the first binding
/// sits inside the second binding's initializer. The two names differ but
/// share their last word. That second condition spares `path` and `file`,
/// which name two values rather than one.
pub struct SplitBinding;

/// A `let` statement that binds one plain, immutable name to a value
///
/// The rule looks at this shape only. It ignores a destructuring pattern, a
/// `let ... else`, a `mut` binding, a name that starts with `_`, and a `let`
/// with no value.
struct Binding<'a> {
    name: &'a str,
    pattern: DecoratedNode<'a>,
    value: DecoratedNode<'a>,
    end: usize,
}

impl<'a> Binding<'a> {
    /// Returns the [`Binding`] this statement declares, if it has that shape
    fn from_statement(statement: &DecoratedNode<'a>) -> Option<Self> {
        if statement.kind() != "let_declaration" {
            return None;
        }
        if statement.child_by_field_name("alternative").is_some() {
            return None;
        }

        let children = statement.named_children();
        if children.iter().any(|c| c.kind() == "mutable_specifier") {
            return None;
        }

        let pattern = statement.child_by_field_name("pattern")?;
        if pattern.kind() != "identifier" {
            return None;
        }

        let name = pattern.text();
        if name.starts_with('_') {
            return None;
        }

        let value = statement.child_by_field_name("value")?;
        let end = statement.raw().end_byte();

        Some(Self {
            name,
            pattern,
            value,
            end,
        })
    }
}

impl RustLintPass for SplitBinding {
    fn check_block(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        analyze(node)
    }
}

/// Returns one diagnostic for every binding in `block` whose only reader is a
/// later binding with the same last word
///
/// Only statements directly inside `block` can start a chain, but a name counts
/// as used wherever it appears below `block`. A read outside the successor's
/// initializer therefore keeps the binding alive, even from a nested scope.
fn analyze(block: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    let statements = block.named_children();
    let bindings: Vec<_> = statements
        .iter()
        .filter_map(Binding::from_statement)
        .collect();
    if bindings.len() < 2 {
        return Vec::new();
    }

    let mut identifiers = Vec::new();
    collect_identifiers(block, &mut identifiers);

    let text = block.text();
    let start = block.raw().start_byte();

    let mut diagnostics = Vec::new();
    for (index, binding) in bindings.iter().enumerate() {
        let uses: Vec<usize> = identifiers
            .iter()
            .filter(|identifier| identifier.text() == binding.name)
            .map(|identifier| identifier.raw().start_byte())
            .filter(|offset| *offset >= binding.end)
            .collect();

        let Some(first) = uses.first() else { continue };

        let Some(successor) = bindings[index + 1..]
            .iter()
            .find(|candidate| contains(&candidate.value, *first))
        else {
            continue;
        };

        if !uses
            .iter()
            .all(|offset| contains(&successor.value, *offset))
        {
            continue;
        }
        let Some(shared) = shared_head(binding.name, successor.name) else {
            continue;
        };
        if !draws_only_on(&successor.value, binding.name) {
            continue;
        }
        if is_captured_by_format(&text[binding.end - start..], binding.name) {
            continue;
        }

        diagnostics.push(
            Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Warn)
                .message(format!(
                    "`{}` is only used to initialize `{}`; use `{}` for both and shadow",
                    binding.name, successor.name, shared
                ))
                .span(binding.pattern.span())
                .build(),
        );
    }

    diagnostics
}

/// Appends every `identifier` node below `node` to `out`
///
/// A macro body holds `identifier` nodes too, so a macro call counts as a
/// read.
fn collect_identifiers<'a>(node: &DecoratedNode<'a>, out: &mut Vec<DecoratedNode<'a>>) {
    for child in node.named_children() {
        if child.kind() == "identifier" {
            out.push(child);
        }
        collect_identifiers(&child, out);
    }
}

/// Returns whether `node` covers the byte at `offset`
fn contains(node: &DecoratedNode<'_>, offset: usize) -> bool {
    let range = node.raw().byte_range();
    range.start <= offset && offset < range.end
}

/// Returns whether `value` is built from `name` and nothing else in scope
///
/// A chain of transformations has one source. `input.trim()` and
/// `parse(input)?` each draw on `input` alone, so the two bindings hold one
/// value at two stages and shadowing says so.
///
/// A value built from several things is not that. `conn.query_row(sql, (kind,
/// payload, app_id), read)` names an app's id among its arguments and returns
/// a job's id, and giving both names to one binding would say they are the
/// same value. The same goes for `author.unwrap_or(&default_author)`, where
/// the second binding borrows from the first and the two cannot merge at all.
///
/// The names of the things being called do not count as sources, and neither
/// does a name the initializer binds itself, so a closure parameter in
/// `xs.iter().map(|x| f(x))` leaves `xs` the only source.
fn draws_only_on(value: &DecoratedNode<'_>, name: &str) -> bool {
    let mut bound = Vec::new();
    collect_closure_parameters(value, &mut bound);

    let mut sources = Vec::new();
    collect_sources(value, &mut sources);

    let outer: Vec<&str> = sources
        .into_iter()
        .filter(|source| !bound.iter().any(|parameter| parameter == source))
        .collect();

    !outer.is_empty() && outer.iter().all(|source| *source == name)
}

/// Appends the name of every identifier below `node` that reads a value
///
/// A method's name, a called function's name, the path of a scoped name, and
/// the field named on the left of a struct literal all appear as identifiers
/// and none of them reads a value, so each is skipped.
fn collect_sources<'a>(node: &DecoratedNode<'a>, out: &mut Vec<&'a str>) {
    let skip = match node.kind() {
        "field_expression" => node.child_by_field_name("field"),
        "call_expression" => node
            .child_by_field_name("function")
            .filter(|function| function.kind() == "identifier"),
        "field_initializer" => node.child_by_field_name("field"),
        "scoped_identifier" | "generic_function" => Some(*node),
        _ => None,
    };

    for child in node.named_children() {
        if let Some(skipped) = &skip
            && skipped.id() == child.id()
        {
            continue;
        }
        if child.kind() == "identifier" {
            let text = child.text();
            let is_type = match text.chars().next() {
                Some(first) => first.is_uppercase(),
                None => false,
            };
            if !is_type {
                out.push(text);
            }
        }
        collect_sources(&child, out);
    }

    if node.kind() == "scoped_identifier" || node.kind() == "generic_function" {
        out.retain(|source| !node.text().contains(source));
    }
}

/// Appends the name of every closure parameter below `node` to `out`
///
/// Such a name is bound by the initializer itself, so it is not something the
/// initializer draws on from the scope around it.
fn collect_closure_parameters<'a>(node: &DecoratedNode<'a>, out: &mut Vec<&'a str>) {
    if node.kind() == "closure_expression"
        && let Some(parameters) = node.child_by_field_name("parameters")
    {
        collect_identifiers_named(&parameters, out);
    }

    for child in node.named_children() {
        collect_closure_parameters(&child, out);
    }
}

/// Appends the name of every identifier below `node` to `out`
fn collect_identifiers_named<'a>(node: &DecoratedNode<'a>, out: &mut Vec<&'a str>) {
    if node.kind() == "identifier" {
        out.push(node.text());
    }
    for child in node.named_children() {
        collect_identifiers_named(&child, out);
    }
}

/// Returns the last word of a snake_case name
///
/// English puts the head noun last, so `raw_input` and `trimmed_input` both
/// describe an input, while `config_path` describes a path.
fn head(name: &str) -> &str {
    name.rsplit('_')
        .find(|word| !word.is_empty())
        .unwrap_or(name)
}

/// Returns whether `text` captures `name` inline, as `{name}` or `{name:`
///
/// An inline capture such as `println!("{raw_input}")` reads the binding
/// without an `identifier` node, so the rule must find it in the text. The
/// check is textual, so a match in any other string also counts.
fn is_captured_by_format(text: &str, name: &str) -> bool {
    text.contains(&format!("{{{name}}}")) || text.contains(&format!("{{{name}:"))
}

/// Returns the last word two differing binding names share
///
/// A shared last word is the rule's proxy for one value at two stages, and the
/// diagnostic offers that word as the name to keep. A word that does not begin
/// with a letter is a disambiguator rather than a head noun, so `left_1` and
/// `right_1` share nothing.
fn shared_head<'a>(first: &'a str, second: &str) -> Option<&'a str> {
    if first == second {
        return None;
    }

    let shared = head(first);
    if shared != head(second) {
        return None;
    }
    if !shared.starts_with(|character: char| character.is_alphabetic()) {
        return None;
    }

    Some(shared)
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for SplitBinding {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.split-binding")]
    }
}

stiles_plugin::export_lints![rust: [SplitBinding]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use stiles_types::{Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(SplitBinding))]
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        execute(&tree, &mut passes())
    }

    /// A binding the next one borrows from cannot merge with it at all
    ///
    /// Taken from raft. `author` is already a parameter, so the suggested
    /// rewrite would have the binding borrow from itself and would not
    /// compile.
    #[test]
    fn a_binding_borrowed_by_its_successor_is_not_flagged() {
        let source = "fn f(author: Option<&Author>) -> &str {\n    \
             let default_author = Author::default_raft();\n    \
             let author = author.unwrap_or(&default_author);\n    \
             author.name\n}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    /// A binding that is one argument among several is not a stage of the
    /// value that follows it
    ///
    /// Taken from raft, where `app_id` names an app and `id` names the job
    /// row the insert returned. Shadowing them into one name would say an app
    /// and a job have the same id.
    #[test]
    fn a_binding_passed_beside_other_values_is_not_flagged() {
        let source = "fn f(conn: &C, payload: &P, kind: &str) -> R {\n    \
             let app_id = app_id_for(conn, payload);\n    \
             let id: i64 = conn.query_row(SQL, (kind, payload, app_id), read)?;\n    \
             id\n}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    /// A closure parameter is bound by the initializer, so it does not make
    /// the initializer draw on a second thing
    #[test]
    fn a_closure_parameter_does_not_count_as_a_second_source() {
        let source = "fn f() -> Vec<i32> {\n    \
             let raw_values = read_values();\n    \
             let mapped_values = raw_values.iter().map(|value| value + 1).collect();\n    \
             mapped_values\n}";

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.split-binding")
            .has_severity(Severity::Warn)
            .message_contains("`raw_values`");
    }

    #[test]
    fn binding_read_after_its_successor_is_not_flagged() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                log(raw_input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn binding_read_by_a_macro_is_not_flagged() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                dbg!(raw_input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn binding_read_in_a_nested_block_is_not_flagged() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                if cond {
                    use_it(raw_input);
                }
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn chain_inside_a_nested_block_is_flagged_once() {
        let diagnostics = run("fn f() {
                if cond {
                    let raw_input = read();
                    let input = raw_input.trim();
                    use_it(input);
                }
            }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn chain_inside_a_nested_block_names_the_first_binding() {
        let diagnostics = run("fn f() {
                if cond {
                    let raw_input = read();
                    let input = raw_input.trim();
                    use_it(input);
                }
            }");

        assert_diagnostic(&diagnostics[0]).message_contains("`raw_input`");
    }

    #[test]
    fn chain_of_three_bindings_flags_every_link() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                let parsed_input = parse(trimmed_input);
                use_it(parsed_input);
            }");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn chain_of_three_bindings_flags_the_first_link() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                let parsed_input = parse(trimmed_input);
                use_it(parsed_input);
            }");

        assert_diagnostic(&diagnostics[0])
            .message_contains("`raw_input` is only used to initialize `trimmed_input`");
    }

    #[test]
    fn chain_of_three_bindings_flags_the_second_link() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                let parsed_input = parse(trimmed_input);
                use_it(parsed_input);
            }");

        assert_diagnostic(&diagnostics[1])
            .message_contains("`trimmed_input` is only used to initialize `parsed_input`");
    }

    #[test]
    fn destructured_successor_is_not_flagged() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let (head_input, tail_input) = split(raw_input);
                let input = join(head_input, tail_input);
                use_it(input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn diagnostic_for_two_bindings_with_a_shared_word_is_reported_once() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                use_it(trimmed_input);
            }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn diagnostic_names_the_word_both_bindings_share() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                use_it(trimmed_input);
            }");

        assert_diagnostic(&diagnostics[0]).message_contains("use `input` for both and shadow");
    }

    #[test]
    fn head_with_single_word_returns_the_whole_name() {
        assert_eq!(head("input"), "input");
    }

    #[test]
    fn head_with_snake_case_path_name_returns_the_last_word() {
        assert_eq!(head("config_path"), "path");
    }

    #[test]
    fn head_with_snake_case_returns_the_last_word() {
        assert_eq!(head("raw_input"), "input");
    }

    #[test]
    fn head_with_trailing_underscore_returns_the_last_word() {
        assert_eq!(head("input_"), "input");
    }

    #[test]
    fn inline_format_capture_is_not_flagged() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                println!(\"{raw_input}\");
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn is_captured_by_format_with_bare_name_returns_true() {
        assert!(is_captured_by_format("println!(\"{input}\")", "input"));
    }

    #[test]
    fn is_captured_by_format_with_format_spec_returns_true() {
        assert!(is_captured_by_format("println!(\"{input:?}\")", "input"));
    }

    #[test]
    fn is_captured_by_format_with_other_name_returns_false() {
        assert!(!is_captured_by_format("println!(\"{other}\")", "input"));
    }

    #[test]
    fn let_else_binding_is_not_flagged() {
        let diagnostics = run("fn f() {
                let Some(raw_input) = read() else { return };
                let trimmed_input = raw_input.trim();
                use_it(trimmed_input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn mutable_binding_is_not_flagged() {
        let diagnostics = run("fn f() {
                let mut raw_input = read();
                let trimmed_input = raw_input.trim();
                use_it(trimmed_input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn non_adjacent_binding_is_flagged_naming_the_first_binding() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let count = 0;
                let trimmed_input = raw_input.trim();
                use_it(trimmed_input, count);
            }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.split-binding")
            .has_severity(Severity::Warn)
            .message_contains("`raw_input`");
    }

    #[test]
    fn non_adjacent_binding_is_flagged_once() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let count = 0;
                let trimmed_input = raw_input.trim();
                use_it(trimmed_input, count);
            }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn numeric_last_word_is_not_flagged() {
        let diagnostics = run("fn f() {
                let left_1 = read();
                let right_1 = wrap(left_1);
                use_it(right_1);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn redeclared_name_after_the_successor_is_not_flagged() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = raw_input.trim();
                let raw_input = read_again();
                use_it(trimmed_input, raw_input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn repeated_use_inside_one_initializer_is_flagged_as_only_initializing_its_successor() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = join(raw_input, raw_input);
                use_it(trimmed_input);
            }");

        assert_diagnostic(&diagnostics[0])
            .message_contains("`raw_input` is only used to initialize `trimmed_input`");
    }

    #[test]
    fn repeated_use_inside_one_initializer_is_flagged_once() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let trimmed_input = join(raw_input, raw_input);
                use_it(trimmed_input);
            }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn shadowed_chain_is_not_flagged() {
        let diagnostics = run("fn f() {
                let input = read();
                let input = input.trim();
                use_it(input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn shared_head_with_bare_last_word_returns_it() {
        assert_eq!(shared_head("raw_input", "input"), Some("input"));
    }

    #[test]
    fn shared_head_with_different_last_words_returns_none() {
        assert!(shared_head("left", "right").is_none());
    }

    #[test]
    fn shared_head_with_equal_names_returns_none() {
        assert!(shared_head("input", "input").is_none());
    }

    #[test]
    fn shared_head_with_numeric_last_word_returns_none() {
        assert!(shared_head("left_1", "right_1").is_none());
    }

    #[test]
    fn shared_head_with_one_name_a_prefix_of_the_other_returns_none() {
        assert!(shared_head("config", "config_path").is_none());
    }

    #[test]
    fn shared_head_with_shared_last_word_returns_it() {
        assert_eq!(shared_head("raw_input", "trimmed_input"), Some("input"));
    }

    #[test]
    fn trait_send_holds_for_binding() {
        fn assert_send<T: Send>() {}
        assert_send::<Binding<'_>>();
    }

    #[test]
    fn trait_send_holds_for_split_binding() {
        fn assert_send<T: Send>() {}
        assert_send::<SplitBinding>();
    }

    #[test]
    fn trait_sync_holds_for_binding() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Binding<'_>>();
    }

    #[test]
    fn trait_sync_holds_for_split_binding() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<SplitBinding>();
    }

    #[test]
    fn trait_unpin_holds_for_binding() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Binding<'_>>();
    }

    #[test]
    fn trait_unpin_holds_for_split_binding() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<SplitBinding>();
    }

    #[test]
    fn two_bindings_with_shared_head_are_flagged_as_only_initializing_the_first() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let input = raw_input.trim();
                use_it(input);
            }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.split-binding")
            .has_severity(Severity::Warn)
            .message_contains("`raw_input` is only used to initialize `input`");
    }

    #[test]
    fn two_bindings_with_shared_head_are_flagged_once() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let input = raw_input.trim();
                use_it(input);
            }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn underscore_prefixed_binding_is_not_flagged() {
        let diagnostics = run("fn f() {
                let _raw_input = read();
                let input = _raw_input.trim();
                use_it(input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn unread_binding_is_not_flagged() {
        let diagnostics = run("fn f() {
                let raw_input = read();
                let input = other();
                use_it(input);
            }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn unrelated_names_are_not_flagged() {
        let diagnostics = run("fn f() {
                let path = build();
                let file = open(path);
                use_it(file);
            }");

        assert_no_diagnostics(&diagnostics);
    }
}
