use whisker_rust::{RuleId, RustLintPass};
use whisker_types::{DecoratedNode, Diagnostic, Severity, Span};

const RULE_ID: RuleId = RuleId::new("lint.repeated-field-access");

/// Distinct fields one statement must read before the lint fires
///
/// One field read is not a pattern. Two is what the loop example in
/// `CLAUDE.md` reads, so the lint cannot ask for more.
const FIELD_THRESHOLD: usize = 2;

/// Flags a statement that reads two or more fields of one binding
///
/// Repeated field access hides which parts of a value a statement needs. A
/// destructuring `let` names those parts once and shortens every use.
///
/// The lint counts reads inside a single statement, so the fix is one `let`
/// line above that statement. It stays quiet when the statement also names
/// the binding on its own, because the code still needs the whole value.
///
/// The lint ignores `self`. A method body reads `self.field` everywhere, and
/// destructuring `self` fights the borrow checker.
pub struct RepeatedFieldAccess;

/// The fields one statement reads from one binding
///
/// `used_whole` records that the statement also needs the binding itself, so
/// the lint cannot suggest a destructuring `let`. A method call, an
/// assignment target, and a bare mention of the name all set it.
struct BindingReads<'a> {
    name: &'a str,
    fields: Vec<&'a str>,
    span: Span,
    used_whole: bool,
}

impl RustLintPass for RepeatedFieldAccess {
    fn check_block(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let loop_name = for_pattern_binding(node);

        node.named_children()
            .iter()
            .filter(|statement| statement.kind() != "block")
            .flat_map(|statement| check_statement(statement, loop_name))
            .collect()
    }
}

/// Reports every binding the statement reads too many fields from
///
/// Diagnostics come out in the order the bindings first appear.
fn check_statement<'a>(statement: &DecoratedNode<'a>, loop_name: Option<&str>) -> Vec<Diagnostic> {
    let mut reads = Vec::new();
    collect(statement, &mut reads);

    reads
        .into_iter()
        .filter_map(|binding| diagnose(binding, loop_name))
        .collect()
}

/// Walks one statement and records what it reads
///
/// The walk stops at a nested `block` because that block is a scope of its
/// own and gets its own `check_block` call.
fn collect<'a>(node: &DecoratedNode<'a>, reads: &mut Vec<BindingReads<'a>>) {
    record(node, reads);

    for child in node.named_children() {
        if child.kind() == "block" {
            continue;
        }
        collect(&child, reads);
    }
}

/// Records the field this node reads, or marks its binding as used whole
fn record<'a>(node: &DecoratedNode<'a>, reads: &mut Vec<BindingReads<'a>>) {
    let kind = node.kind();

    if kind == "identifier" {
        if !is_field_base(node) {
            entry(reads, node.text(), node.span()).used_whole = true;
        }
        return;
    }

    if kind != "field_expression" {
        return;
    }

    let Some(value) = node.child_by_field_name("value") else {
        return;
    };
    if value.kind() != "identifier" {
        return;
    }
    let name = value.text();

    if is_call_target(node) || is_assignment_target(node) {
        entry(reads, name, node.span()).used_whole = true;
        return;
    }

    let Some(field) = node.child_by_field_name("field") else {
        return;
    };
    if field.kind() != "field_identifier" {
        return;
    }

    let binding = entry(reads, name, node.span());
    if !binding.fields.contains(&field.text()) {
        binding.fields.push(field.text());
    }
}

/// Returns the record for `name`, creating it at `span` when it is new
fn entry<'a, 'r>(
    reads: &'r mut Vec<BindingReads<'a>>,
    name: &'a str,
    span: Span,
) -> &'r mut BindingReads<'a> {
    let index = match reads.iter().position(|binding| binding.name == name) {
        Some(index) => index,
        None => {
            reads.push(BindingReads {
                name,
                fields: Vec::new(),
                span,
                used_whole: false,
            });
            reads.len() - 1
        }
    };

    &mut reads[index]
}

/// Turns one binding's reads into a diagnostic, when they reach the threshold
///
/// `loop_name` is the name the enclosing `for` loop binds, if any. A match
/// against it changes the advice, because the fix belongs in the loop pattern
/// rather than in a new `let`.
fn diagnose(binding: BindingReads<'_>, loop_name: Option<&str>) -> Option<Diagnostic> {
    let BindingReads {
        name,
        fields,
        span,
        used_whole,
    } = binding;

    if used_whole || fields.len() < FIELD_THRESHOLD {
        return None;
    }

    let count = fields.len();
    let message = match loop_name == Some(name) {
        true => format!(
            "this statement reads {count} fields of `{name}`; destructure `{name}` in the `for` \
             pattern"
        ),
        false => format!("this statement reads {count} fields of `{name}`; destructure `{name}`"),
    };

    Some(
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Warn)
            .message(message)
            .span(span)
            .build(),
    )
}

/// Returns the name a `for` loop binds, when `block` is that loop's body
///
/// The name is absent when the loop already destructures, because then the
/// advice has nothing left to suggest.
fn for_pattern_binding<'a>(block: &DecoratedNode<'a>) -> Option<&'a str> {
    let parent = block.parent()?;
    if parent.kind() != "for_expression" {
        return None;
    }
    if !is_field(&parent, "body", block) {
        return None;
    }

    bound_name(&parent.child_by_field_name("pattern")?)
}

/// Returns the name a pattern binds, when it binds exactly one
///
/// A binding mode wraps the name it introduces: `mut e` parses as a
/// `mut_pattern`, `ref e` as a `ref_pattern`, and `&e` as a
/// `reference_pattern`. Each of the three still binds the whole item under
/// one name, so the search looks through the wrapper to reach it. A pattern
/// that already destructures binds no single name, and the search stops.
fn bound_name<'a>(pattern: &DecoratedNode<'a>) -> Option<&'a str> {
    let kind = pattern.kind();
    if kind == "identifier" {
        return Some(pattern.text());
    }
    if kind != "mut_pattern" && kind != "ref_pattern" && kind != "reference_pattern" {
        return None;
    }

    let children = pattern.named_children();
    let inner = children.last()?;

    bound_name(inner)
}

/// Returns whether `node` is the assigned place of its parent
fn is_assignment_target(node: &DecoratedNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "assignment_expression" && parent.kind() != "compound_assignment_expr" {
        return false;
    }

    is_field(&parent, "left", node)
}

/// Returns whether `node` names the function of a call
///
/// A method call reads no field: `user.name()` runs `name`, and it needs the
/// whole `user`. The turbofish form puts a `generic_function` between the
/// call and the field expression.
fn is_call_target(node: &DecoratedNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "generic_function" {
        return is_field(&parent, "function", node);
    }
    if parent.kind() != "call_expression" {
        return false;
    }

    is_field(&parent, "function", node)
}

/// Returns whether `parent` holds `node` under the field called `name`
fn is_field(parent: &DecoratedNode<'_>, name: &str, node: &DecoratedNode<'_>) -> bool {
    let Some(child) = parent.child_by_field_name(name) else {
        return false;
    };

    child.id() == node.id()
}

/// Returns whether `node` is the value a field expression reads from
fn is_field_base(node: &DecoratedNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "field_expression" {
        return false;
    }

    is_field(&parent, "value", node)
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for RepeatedFieldAccess {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.repeated-field-access")]
    }
}

whisker_plugin::export_lints![rust: [RepeatedFieldAccess]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![Box::new(RustLintPassAdapter::new(RepeatedFieldAccess))]
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);

        execute(&tree, &mut passes())
    }

    #[test]
    fn assigned_field_is_not_flagged() {
        let diagnostics = run("fn f() { u.a = u.b; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn binding_passed_whole_is_not_flagged() {
        let diagnostics = run("fn f() { g(u, u.a, u.b); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn block_tail_expression_is_flagged_as_two_fields_of_the_binding() {
        let diagnostics = run("fn f() -> u32 { r.width * r.height }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.repeated-field-access")
            .has_severity(Severity::Warn)
            .message_contains("2 fields of `r`");
    }

    #[test]
    fn block_tail_expression_is_flagged_once() {
        let diagnostics = run("fn f() -> u32 { r.width * r.height }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn closure_parameter_is_a_whole_use() {
        let diagnostics = run("fn f() { g(it.map(|e| e.a + e.b)); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn closure_with_a_block_body_is_flagged_for_the_closure_parameter() {
        let diagnostics = run("fn f() { g(it.map(|e| { h(e.a, e.b) })); }");

        assert_diagnostic(&diagnostics[0]).message_contains("2 fields of `e`");
    }

    #[test]
    fn closure_with_a_block_body_is_flagged_once() {
        let diagnostics = run("fn f() { g(it.map(|e| { h(e.a, e.b) })); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn compound_assigned_field_is_not_flagged() {
        let diagnostics = run("fn f() { u.a += u.b; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn field_read_twice_is_not_flagged() {
        let diagnostics = run("fn f() { g(u.a, u.a); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn fields_in_separate_statements_are_not_flagged() {
        let diagnostics = run("fn f() { let a = u.a; let b = u.b; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn fields_inside_a_macro_are_not_flagged() {
        let diagnostics = run(r#"fn f() { println!("{} {}", u.a, u.b); }"#);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn fields_of_a_nested_value_are_flagged_on_the_outer_binding() {
        let diagnostics = run("fn f() { g(u.a.x, u.b.y); }");

        assert_diagnostic(&diagnostics[0]).message_contains("2 fields of `u`");
    }

    #[test]
    fn fields_of_a_nested_value_are_flagged_once() {
        let diagnostics = run("fn f() { g(u.a.x, u.b.y); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn fields_of_two_bindings_are_flagged_separately() {
        let diagnostics = run("fn f() { g(u.a, u.b, v.a, v.b); }");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn fields_of_two_bindings_flag_the_first_binding_first() {
        let diagnostics = run("fn f() { g(u.a, u.b, v.a, v.b); }");

        assert_diagnostic(&diagnostics[0]).message_contains("`u`");
    }

    #[test]
    fn fields_of_two_bindings_flag_the_second_binding_second() {
        let diagnostics = run("fn f() { g(u.a, u.b, v.a, v.b); }");

        assert_diagnostic(&diagnostics[1]).message_contains("`v`");
    }

    #[test]
    fn for_pattern_binding_gets_the_pattern_advice() {
        let diagnostics = run("fn f() { for e in it { m.insert(e.key, e.value); } }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.repeated-field-access")
            .message_contains("destructure `e` in the `for` pattern");
    }

    #[test]
    fn for_pattern_binding_is_flagged_once() {
        let diagnostics = run("fn f() { for e in it { m.insert(e.key, e.value); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn for_pattern_that_destructures_behind_a_reference_gets_the_plain_advice() {
        let diagnostics = run("fn f() { for &(a, b) in it { m.insert(b.key, b.value); } }");

        assert_diagnostic(&diagnostics[0]).message_contains("destructure `b`");
    }

    #[test]
    fn for_pattern_that_destructures_behind_a_reference_is_flagged_once() {
        let diagnostics = run("fn f() { for &(a, b) in it { m.insert(b.key, b.value); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn for_pattern_that_destructures_behind_a_reference_omits_the_for_pattern_advice() {
        let diagnostics = run("fn f() { for &(a, b) in it { m.insert(b.key, b.value); } }");

        assert!(!diagnostics[0].message().contains("`for` pattern"));
    }

    #[test]
    fn for_pattern_that_destructures_gets_the_plain_advice() {
        let diagnostics = run("fn f() { for (a, b) in it { m.insert(a.key, a.value); } }");

        assert_diagnostic(&diagnostics[0]).message_contains("destructure `a`");
    }

    #[test]
    fn for_pattern_that_destructures_is_flagged_once() {
        let diagnostics = run("fn f() { for (a, b) in it { m.insert(a.key, a.value); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn for_pattern_that_destructures_omits_the_for_pattern_advice() {
        let diagnostics = run("fn f() { for (a, b) in it { m.insert(a.key, a.value); } }");

        assert!(!diagnostics[0].message().contains("`for` pattern"));
    }

    #[test]
    fn for_pattern_with_mut_binding_gets_the_pattern_advice() {
        let diagnostics = run("fn f() { for mut e in it { m.insert(e.key, e.value); } }");

        assert_diagnostic(&diagnostics[0]).message_contains("destructure `e` in the `for` pattern");
    }

    #[test]
    fn for_pattern_with_mut_binding_is_flagged_once() {
        let diagnostics = run("fn f() { for mut e in it { m.insert(e.key, e.value); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn for_pattern_with_mutable_reference_binding_gets_the_pattern_advice() {
        let diagnostics = run("fn f() { for &mut e in it { m.insert(e.key, e.value); } }");

        assert_diagnostic(&diagnostics[0]).message_contains("destructure `e` in the `for` pattern");
    }

    #[test]
    fn for_pattern_with_mutable_reference_binding_is_flagged_once() {
        let diagnostics = run("fn f() { for &mut e in it { m.insert(e.key, e.value); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn for_pattern_with_ref_binding_gets_the_pattern_advice() {
        let diagnostics = run("fn f() { for ref e in it { m.insert(e.key, e.value); } }");

        assert_diagnostic(&diagnostics[0]).message_contains("destructure `e` in the `for` pattern");
    }

    #[test]
    fn for_pattern_with_ref_binding_is_flagged_once() {
        let diagnostics = run("fn f() { for ref e in it { m.insert(e.key, e.value); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn for_pattern_with_ref_mut_binding_gets_the_pattern_advice() {
        let diagnostics = run("fn f() { for ref mut e in it { m.insert(e.key, e.value); } }");

        assert_diagnostic(&diagnostics[0]).message_contains("destructure `e` in the `for` pattern");
    }

    #[test]
    fn for_pattern_with_ref_mut_binding_is_flagged_once() {
        let diagnostics = run("fn f() { for ref mut e in it { m.insert(e.key, e.value); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn for_pattern_with_reference_binding_gets_the_pattern_advice() {
        let diagnostics = run("fn f() { for &e in it { m.insert(e.key, e.value); } }");

        assert_diagnostic(&diagnostics[0]).message_contains("destructure `e` in the `for` pattern");
    }

    #[test]
    fn for_pattern_with_reference_binding_is_flagged_once() {
        let diagnostics = run("fn f() { for &e in it { m.insert(e.key, e.value); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn generic_method_call_is_not_a_field_read() {
        let diagnostics = run("fn f() { g(u.a, u.parse::<u32>()); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_call_on_a_field_is_flagged_once() {
        let diagnostics = run("fn f() { g(u.a.len(), u.b.len()); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn method_call_on_a_field_is_still_a_field_read() {
        let diagnostics = run("fn f() { g(u.a.len(), u.b.len()); }");

        assert_diagnostic(&diagnostics[0]).message_contains("2 fields of `u`");
    }

    #[test]
    fn method_call_on_the_binding_is_not_flagged() {
        let diagnostics = run("fn f() { g(u.a, u.b, u.total()); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_calls_are_not_field_reads() {
        let diagnostics = run("fn f() { g(u.a(), u.b()); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_block_reads_are_not_merged_with_the_outer_statement() {
        let diagnostics = run("fn f() { let x = if c { u.a } else { u.b }; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn one_field_is_not_flagged() {
        let diagnostics = run("fn f() { g(u.a); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn self_fields_are_not_flagged() {
        let diagnostics = run("impl T { fn f(&self) { g(self.a, self.b, self.c); } }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_expression_fields_are_flagged_as_two_fields_of_the_binding() {
        let diagnostics = run("fn f() { let p = Point { x: u.a, y: u.b }; }");

        assert_diagnostic(&diagnostics[0]).message_contains("2 fields of `u`");
    }

    #[test]
    fn struct_expression_fields_are_flagged_once() {
        let diagnostics = run("fn f() { let p = Point { x: u.a, y: u.b }; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn three_fields_in_one_call_are_flagged_once() {
        let diagnostics = run("fn f() { process(user.id, user.name, user.email); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn three_fields_in_one_call_are_flagged_with_the_destructure_advice() {
        let diagnostics = run("fn f() { process(user.id, user.name, user.email); }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.repeated-field-access")
            .has_severity(Severity::Warn)
            .message_contains("this statement reads 3 fields of `user`; destructure `user`");
    }

    #[test]
    fn trait_send_holds_for_binding_reads() {
        fn assert_send<T: Send>() {}
        assert_send::<BindingReads<'static>>();
    }

    #[test]
    fn trait_send_holds_for_repeated_field_access() {
        fn assert_send<T: Send>() {}
        assert_send::<RepeatedFieldAccess>();
    }

    #[test]
    fn trait_sync_holds_for_binding_reads() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<BindingReads<'static>>();
    }

    #[test]
    fn trait_sync_holds_for_repeated_field_access() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<RepeatedFieldAccess>();
    }

    #[test]
    fn trait_unpin_holds_for_binding_reads() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<BindingReads<'static>>();
    }

    #[test]
    fn trait_unpin_holds_for_repeated_field_access() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<RepeatedFieldAccess>();
    }

    #[test]
    fn tuple_indices_are_not_flagged() {
        let diagnostics = run("fn f() { g(t.0, t.1); }");

        assert_no_diagnostics(&diagnostics);
    }
}
