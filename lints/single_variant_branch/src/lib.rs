mod test_code;

use stiles_rust::decorations::{AdtFlags, EnumVariants, ResolvedType};
use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, Severity};

use crate::test_code::is_test_code;

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(
///     single_variant_branch::RULE_ID.as_str(),
///     "lint.single-variant-branch"
/// );
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.single-variant-branch");

/// The macros that end a block without returning to it
const DIVERGING_MACROS: &[&str] = &["panic", "unreachable", "todo", "unimplemented"];

/// Flags a branch that one variant of an enum decides
///
/// A new variant of an enum needs a decision wherever the enum picks a path.
/// A `match` that names every variant makes the compiler ask for that
/// decision. An `if let`, a let-else, an `is_` predicate, a comparison with
/// `==`, and `matches!` all send every other variant down one path without
/// a word. So the rule reports each of them when it chooses between two
/// paths. An early exit counts as the second path.
///
/// `matches!` that lists several variants is reported wherever it stands,
/// because it sorts the variants into two groups.
///
/// A test that picks no second path is not reported, such as a `filter`, an
/// `assert!`, or an `if` that only does extra work. `Option` and `Result`
/// are not reported either, because they never gain a variant. Their
/// pattern must leave only the other variant, so `Some(name)` passes and
/// `Some(5)` does not. An enum that another crate marks `#[non_exhaustive]`
/// is not reported, because a `match` on it needs a wildcard arm anyway.
/// A let-else in test code whose `else` panics is not reported either,
/// because a new variant fails that test on its first run.
///
/// # Examples
///
/// ```text
/// // DON'T
/// fn step(direction: Direction) {
///     if let Direction::North = direction {
///         climb();
///     } else {
///         wait();
///     }
/// }
///
/// // DO
/// fn step(direction: Direction) {
///     match direction {
///         Direction::North => climb(),
///         Direction::East | Direction::South | Direction::West => wait(),
///     }
/// }
/// ```
///
/// The rule reads the type of the tested value, which the Rust provider
/// attaches as decorations. This example attaches them by hand.
///
/// ```
/// use single_variant_branch::SingleVariantBranch;
/// use stiles_rust::RustLintPassAdapter;
/// use stiles_rust::decorations::{AdtFlags, AdtKind, EnumVariants, Exhaustiveness, ResolvedType};
/// use stiles_testing::{decorate, execute, parse};
/// use stiles_types::{DecorationMap, Language, LintPass};
///
/// let mut tree = parse("fn f() { if let A = k { a() } else { b() } }", Language::Rust);
/// let function = tree.root_node().named_child(0).unwrap();
/// let statement = function.child_by_field_name("body").unwrap().named_child(0).unwrap();
/// let branch = statement.named_child(0).unwrap();
/// let condition = branch.child_by_field_name("condition").unwrap();
/// let value = condition.child_by_field_name("value").unwrap().id();
/// let mut decorations = DecorationMap::new();
/// decorations.insert(value, ResolvedType::new("Kind".into()).with_adt_kind(AdtKind::Enum));
/// decorations.insert(value, AdtFlags::from_exhaustiveness(Exhaustiveness::Exhaustive));
/// decorations.insert(value, EnumVariants::new(["A", "B"]));
/// decorate(&mut tree, decorations);
/// let pass: Box<dyn LintPass> = Box::new(RustLintPassAdapter::new(SingleVariantBranch));
/// let mut passes = vec![pass];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct SingleVariantBranch;

/// Whether an enum can gain a variant
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Growth {
    /// `Option` or `Result`, which keep the variants they have
    Closed,
    /// Any other enum that a `match` can exhaust
    Open,
}

/// An enum whose variant a piece of code tests
struct TestedEnum {
    /// The type as the provider renders it
    display: String,
    /// The names of the variants, in declaration order
    names: Vec<String>,
    /// Whether the enum can gain a variant
    growth: Growth,
}

impl RustLintPass for SingleVariantBranch {
    fn check_if_expression(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        if !picks_between_two_paths(node) {
            return Vec::new();
        }
        let Some(condition) = node.child_by_field_name("condition") else {
            return Vec::new();
        };

        tests_in(&condition)
            .into_iter()
            .map(|tested| single_variant_finding(node, &tested))
            .collect()
    }

    fn check_let_declaration(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(alternative) = node.child_by_field_name("alternative") else {
            return Vec::new();
        };
        if ends_in_panic(&alternative) && is_test_code(node) {
            return Vec::new();
        }

        pattern_test(node)
            .iter()
            .map(|tested| single_variant_finding(node, tested))
            .collect()
    }

    fn check_macro_invocation(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        if !names_matches(node) || !lists_several_patterns(node) {
            return Vec::new();
        }
        let Some(TestedEnum { display, names, .. }) = open_enum(node) else {
            return Vec::new();
        };

        vec![
            Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Error)
                .message(format!(
                    "`matches!` sorts the variants of `{display}` into two groups; write a `match` that names {}",
                    listed(&names)
                ))
                .span(node.span())
                .build(),
        ]
    }
}

/// Returns the finding for a branch that one variant decides
fn single_variant_finding(node: &DecoratedNode<'_>, tested: &TestedEnum) -> Diagnostic {
    let TestedEnum { display, names, .. } = tested;

    Diagnostic::builder()
        .rule_id(RULE_ID)
        .severity(Severity::Error)
        .message(format!(
            "one variant of `{display}` decides this branch; write a `match` that names {}",
            listed(names)
        ))
        .span(node.span())
        .build()
}

/// Returns the names in backticks, joined as a sentence would
fn listed(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();

    match quoted.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// Returns the enum that a tested value holds, when the rule applies to it
///
/// A missing decoration yields [`None`], so the rule stays silent on a value
/// that the provider could not type. An enum of one variant has no second
/// path to choose.
fn tested_enum(value: &DecoratedNode<'_>) -> Option<TestedEnum> {
    let resolved = value.decoration::<ResolvedType>()?;
    let flags = value.decoration::<AdtFlags>()?;
    let variants = value.decoration::<EnumVariants>()?;
    let names: Vec<String> = variants.names().map(str::to_owned).collect();
    let growth = match resolved.is_option() || resolved.is_result() {
        true => Growth::Closed,
        false => Growth::Open,
    };
    let applies = resolved.is_enum() && !flags.non_exhaustive_external() && names.len() > 1;

    applies.then(|| TestedEnum {
        display: resolved.display().to_owned(),
        names,
        growth,
    })
}

/// Returns the enum of a tested value, unless it can never gain a variant
fn open_enum(value: &DecoratedNode<'_>) -> Option<TestedEnum> {
    tested_enum(value).filter(|tested| tested.growth == Growth::Open)
}

/// Returns whether an `if` chooses between two paths
///
/// An `else` is the second path. So is whatever follows a block that does
/// not fall through.
fn picks_between_two_paths(node: &DecoratedNode<'_>) -> bool {
    node.child_by_field_name("alternative").is_some()
        || node
            .child_by_field_name("consequence")
            .is_some_and(|block| block.named_children().last().is_some_and(diverges))
}

/// Returns whether the last statement of a block is a macro that panics
fn ends_in_panic(block: &DecoratedNode<'_>) -> bool {
    block.named_children().last().is_some_and(|last| {
        let statement = if last.kind() == "expression_statement" {
            last.named_child(0)
        } else {
            Some(*last)
        };

        statement.is_some_and(|inner| inner.kind() == "macro_invocation" && diverges(&inner))
    })
}

/// Returns whether a statement or an expression ends its block for good
fn diverges(node: &DecoratedNode<'_>) -> bool {
    match node.kind() {
        "return_expression" | "break_expression" | "continue_expression" => true,
        "expression_statement" => node.named_child(0).is_some_and(|inner| diverges(&inner)),
        "macro_invocation" => node
            .child_by_field_name("macro")
            .is_some_and(|name| DIVERGING_MACROS.contains(&name.text())),
        _ => false,
    }
}

/// Returns the enums whose variant a condition tests
fn tests_in(condition: &DecoratedNode<'_>) -> Vec<TestedEnum> {
    match condition.kind() {
        "let_condition" => pattern_test(condition).into_iter().collect(),
        "let_chain" => condition
            .named_children()
            .iter()
            .filter(|child| child.kind() == "let_condition")
            .filter_map(pattern_test)
            .collect(),
        "parenthesized_expression" | "unary_expression" => condition
            .named_child(0)
            .map(|inner| tests_in(&inner))
            .unwrap_or_default(),
        "call_expression" => predicate_test(condition).into_iter().collect(),
        "binary_expression" => equality_test(condition).into_iter().collect(),
        "macro_invocation" => matches_test(condition).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Returns the enum that a `let` pattern tests
///
/// On `Option` and `Result`, a pattern that takes the variant whole leaves
/// exactly the other variant, so it tests nothing that can be missed.
fn pattern_test(node: &DecoratedNode<'_>) -> Option<TestedEnum> {
    let value = node.child_by_field_name("value")?;
    let pattern = node.child_by_field_name("pattern")?;
    let tested = tested_enum(&value)?;

    match (tested.growth, takes_variant_whole(&pattern)) {
        (Growth::Closed, Whole::Yes) => None,
        (Growth::Closed, Whole::No) | (Growth::Open, Whole::Yes | Whole::No) => Some(tested),
    }
}

/// Whether a pattern accepts every value of the variant it names
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Whole {
    /// The pattern binds or ignores everything inside the variant
    Yes,
    /// The pattern can still fail on what is inside the variant
    No,
}

/// Returns whether a pattern accepts every value of its variant
fn takes_variant_whole(pattern: &DecoratedNode<'_>) -> Whole {
    let accepts_everything = match pattern.kind() {
        "identifier" => true,
        "tuple_struct_pattern" => {
            let name = pattern.child_by_field_name("type").map(|name| name.id());

            pattern
                .named_children()
                .iter()
                .filter(|child| Some(child.id()) != name)
                .all(binds_everything)
        }
        _ => false,
    };

    match accepts_everything {
        true => Whole::Yes,
        false => Whole::No,
    }
}

/// Returns whether a pattern inside a variant can never fail
///
/// An identifier with a capital first letter names a constant or a variant,
/// and a lower-case one binds the value.
fn binds_everything(pattern: &DecoratedNode<'_>) -> bool {
    match pattern.kind() {
        "identifier" => pattern.text().starts_with(char::is_lowercase),
        "mut_pattern" | "ref_pattern" | "tuple_pattern" => {
            pattern.named_children().iter().all(binds_everything)
        }
        "mutable_specifier" | "remaining_field_pattern" => true,
        "struct_pattern" => is_plain_struct_binding(pattern),
        _ => false,
    }
}

/// Returns whether a struct pattern names a plain type and binds its fields
///
/// A path such as `Kind::Alpha { .. }` names a variant of an enum, which can
/// fail. A plain name is a struct, which cannot.
fn is_plain_struct_binding(pattern: &DecoratedNode<'_>) -> bool {
    let plain = pattern
        .child_by_field_name("type")
        .is_some_and(|name| name.kind() == "type_identifier");

    plain
        && pattern
            .named_children()
            .iter()
            .filter(|child| child.kind() == "field_pattern")
            .all(|field| {
                field
                    .child_by_field_name("pattern")
                    .is_none_or(|inner| binds_everything(&inner))
            })
}

/// Returns the enum that an `is_` predicate of a variant tests
fn predicate_test(call: &DecoratedNode<'_>) -> Option<TestedEnum> {
    let function = call.child_by_field_name("function")?;
    let arguments = call.child_by_field_name("arguments")?;
    let field = function.child_by_field_name("field")?;
    let variant = field.text().strip_prefix("is_")?;
    let receiver = function.child_by_field_name("value")?;
    let tested = open_enum(&receiver)?;
    let names_a_variant = arguments.named_child_count() == 0
        && tested.names.iter().any(|name| snake_case(name) == variant);

    names_a_variant.then_some(tested)
}

/// Returns the enum that `==` or `!=` compares with one of its variants
fn equality_test(binary: &DecoratedNode<'_>) -> Option<TestedEnum> {
    let operator = binary.child_by_field_name("operator")?;
    if !["==", "!="].contains(&operator.text()) {
        return None;
    }
    let left = binary.child_by_field_name("left")?;
    let right = binary.child_by_field_name("right")?;

    compared_enum(&left, &right).or_else(|| compared_enum(&right, &left))
}

/// Returns the enum of `value` when `other` names one of its variants
fn compared_enum(value: &DecoratedNode<'_>, other: &DecoratedNode<'_>) -> Option<TestedEnum> {
    let tested = open_enum(value)?;
    let variant = other.text().rsplit("::").next()?;

    tested
        .names
        .iter()
        .any(|name| name == variant)
        .then_some(tested)
}

/// Returns the enum that a `matches!` of one pattern tests
///
/// A `matches!` of several patterns has its own finding, wherever it stands.
fn matches_test(node: &DecoratedNode<'_>) -> Option<TestedEnum> {
    (names_matches(node) && !lists_several_patterns(node))
        .then(|| open_enum(node))
        .flatten()
}

/// Returns whether a macro call is `matches!`
fn names_matches(node: &DecoratedNode<'_>) -> bool {
    node.child_by_field_name("macro")
        .is_some_and(|name| name.text().rsplit("::").next() == Some("matches"))
}

/// Returns whether the pattern of a `matches!` holds a top-level `|`
///
/// The pattern starts after the first comma and ends at an `if` guard. A
/// `|` inside a nested group or a guard joins nothing at the top level.
fn lists_several_patterns(node: &DecoratedNode<'_>) -> bool {
    let Some(tree) = node
        .named_children()
        .into_iter()
        .find(|child| child.kind() == "token_tree")
    else {
        return false;
    };
    let tokens: Vec<&str> = (0..tree.child_count() as u32)
        .filter_map(|index| tree.child(index))
        .map(|child| child.text())
        .collect();

    tokens
        .iter()
        .skip_while(|token| **token != ",")
        .take_while(|token| **token != "if")
        .any(|token| *token == "|")
}

/// Returns a variant name as its `is_` predicate spells it
fn snake_case(name: &str) -> String {
    let characters: Vec<char> = name.chars().collect();
    let mut spelled = String::new();

    for (index, character) in characters.iter().enumerate() {
        let after_lower = index > 0 && !characters[index - 1].is_uppercase();
        let before_lower = characters.get(index + 1).is_some_and(|c| c.is_lowercase());
        if character.is_uppercase() && index > 0 && (after_lower || before_lower) {
            spelled.push('_');
        }
        spelled.extend(character.to_lowercase());
    }

    spelled
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for SingleVariantBranch {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [SingleVariantBranch]];

#[cfg(test)]
mod tests {
    use stiles_rust::RustLintPassAdapter;
    use stiles_rust::decorations::{AdtKind, Exhaustiveness, OptionKind, ResultKind};
    use stiles_testing::{assert_no_diagnostics, decorate, execute, parse};
    use stiles_types::{DecorationMap, Language, LintPass};

    use super::*;

    const KIND: [&str; 3] = ["Alpha", "Beta", "Gamma"];

    /// Returns whether the provider would decorate this node in a test
    fn is_tested_value(node: &DecoratedNode<'_>) -> bool {
        ["k", "direction"].contains(&node.text())
            || (node.kind() == "macro_invocation" && node.text().starts_with("matches!"))
    }

    fn mark(
        node: &DecoratedNode<'_>,
        map: &mut DecorationMap,
        ty: &ResolvedType,
        exhaustiveness: Exhaustiveness,
    ) {
        if is_tested_value(node) {
            map.insert(node.id(), ty.clone());
            map.insert(node.id(), AdtFlags::from_exhaustiveness(exhaustiveness));
            map.insert(node.id(), EnumVariants::new(KIND));
        }
        for child in node.named_children() {
            mark(&child, map, ty, exhaustiveness);
        }
    }

    fn run_with(source: &str, ty: ResolvedType, exhaustiveness: Exhaustiveness) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        let mut map = DecorationMap::new();
        mark(&tree.root_node(), &mut map, &ty, exhaustiveness);
        decorate(&mut tree, map);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(SingleVariantBranch))];

        execute(&tree, &mut passes)
    }

    fn kind() -> ResolvedType {
        ResolvedType::new("Kind".into()).with_adt_kind(AdtKind::Enum)
    }

    fn option() -> ResolvedType {
        kind().with_option_kind(OptionKind::Option)
    }

    fn result() -> ResolvedType {
        kind().with_result_kind(ResultKind::Result)
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        run_with(source, kind(), Exhaustiveness::Exhaustive)
    }

    #[test]
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        assert_eq!(run(pair.example()).len(), 1);
    }

    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        assert_no_diagnostics(&run(pair.fix()));
    }

    #[test]
    fn equality_with_a_variant_and_an_else_is_flagged() {
        let found = run("fn f() { if k == Kind::Alpha { a() } else { b() } }");

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn equality_with_another_value_is_not_flagged() {
        assert_no_diagnostics(&run("fn f() { if k == other { a() } else { b() } }"));
    }

    #[test]
    fn finding_names_every_variant() {
        let found = run("fn f() { if let Kind::Alpha = k { a() } else { b() } }");

        stiles_testing::assert_diagnostic(&found[0])
            .message_contains("`Alpha`, `Beta`, and `Gamma`");
    }

    #[test]
    fn if_let_on_a_foreign_non_exhaustive_enum_is_not_flagged() {
        let source = "fn f() { if let Kind::Alpha = k { a() } else { b() } }";

        assert_no_diagnostics(&run_with(source, kind(), Exhaustiveness::NonExhaustive));
    }

    #[test]
    fn if_let_with_an_early_return_is_flagged() {
        let found = run("fn f() { if let Kind::Alpha = k { a(); return; } b(); }");

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn if_let_with_an_else_is_flagged() {
        let found = run("fn f() { if let Kind::Alpha = k { a() } else { b() } }");

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn if_let_without_a_second_path_is_not_flagged() {
        assert_no_diagnostics(&run("fn f() { if let Kind::Alpha = k { a(); } b(); }"));
    }

    #[test]
    fn if_let_without_decorations_is_not_flagged() {
        let tree = parse(
            "fn f() { if let A = k { a() } else { b() } }",
            Language::Rust,
        );
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(SingleVariantBranch))];

        assert_no_diagnostics(&execute(&tree, &mut passes));
    }

    #[test]
    fn let_else_on_an_enum_is_flagged() {
        let found = run("fn f() { let Kind::Alpha = k else { return; }; a(); }");

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn let_else_taking_some_whole_is_not_flagged() {
        let source = "fn f() { let Some(x) = k else { return; }; a(x); }";

        assert_no_diagnostics(&run_with(source, option(), Exhaustiveness::Exhaustive));
    }

    #[test]
    fn let_else_that_panics_in_a_test_is_not_flagged() {
        let source = "#[test] fn f() { let Kind::Alpha = k else { panic!(); }; a(); }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn let_else_that_panics_outside_a_test_is_flagged() {
        let found = run("fn f() { let Kind::Alpha = k else { panic!(); }; a(); }");

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn let_else_that_returns_in_a_test_is_flagged() {
        let found = run("#[test] fn f() { let Kind::Alpha = k else { return; }; a(); }");

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn matches_of_one_variant_as_a_value_is_not_flagged() {
        let source = "fn f() -> bool { matches!(k, Kind::Alpha { .. }) }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn matches_of_one_variant_deciding_a_branch_is_flagged() {
        let source = "fn f() { if matches!(k, Kind::Alpha) { a() } else { b() } }";

        assert_eq!(run(source).len(), 1);
    }

    #[test]
    fn matches_of_several_variants_as_a_value_is_flagged() {
        let source = "fn f() -> bool { matches!(k, Kind::Alpha | Kind::Beta) }";

        assert_eq!(run(source).len(), 1);
    }

    #[test]
    fn matches_of_several_variants_deciding_a_branch_is_flagged_once() {
        let source = "fn f() { if matches!(k, Kind::Alpha | Kind::Beta) { a() } else { b() } }";

        assert_eq!(run(source).len(), 1);
    }

    #[test]
    fn matches_with_a_closure_in_its_guard_is_not_several_patterns() {
        let source = "fn f() -> bool { matches!(k, Kind::Alpha if v.iter().any(|x| x)) }";

        assert_no_diagnostics(&run(source));
    }

    #[test]
    fn negated_predicate_with_an_else_is_flagged() {
        let found = run("fn f() { if !k.is_alpha() { a() } else { b() } }");

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn option_pattern_binding_a_plain_struct_is_not_flagged() {
        let source = "fn f() { let Some(Point { x, .. }) = k else { return; }; a(x); }";

        assert_no_diagnostics(&run_with(source, option(), Exhaustiveness::Exhaustive));
    }

    #[test]
    fn option_pattern_binding_mutably_is_not_flagged() {
        let source = "fn f() { let Some(mut x) = k else { return; }; a(x); }";

        assert_no_diagnostics(&run_with(source, option(), Exhaustiveness::Exhaustive));
    }

    #[test]
    fn option_pattern_naming_a_struct_variant_is_flagged() {
        let source = "fn f() { let Some(Kind::Alpha { x }) = k else { return; }; a(x); }";

        assert_eq!(
            run_with(source, option(), Exhaustiveness::Exhaustive).len(),
            1
        );
    }

    #[test]
    fn option_pattern_that_takes_the_variant_whole_is_not_flagged() {
        let source = "fn f() { if let Some(x) = k { a(x) } else { b() } }";

        assert_no_diagnostics(&run_with(source, option(), Exhaustiveness::Exhaustive));
    }

    #[test]
    fn option_pattern_that_tests_its_content_is_flagged() {
        let source = "fn f() { if let Some(5) = k { a() } else { b() } }";

        assert_eq!(
            run_with(source, option(), Exhaustiveness::Exhaustive).len(),
            1
        );
    }

    #[test]
    fn option_predicate_is_not_flagged() {
        let source = "fn f() { if k.is_some() { a() } else { b() } }";

        assert_no_diagnostics(&run_with(source, option(), Exhaustiveness::Exhaustive));
    }

    #[test]
    fn predicate_of_a_variant_with_an_early_return_is_flagged() {
        let found = run("fn f() { if k.is_alpha() { a(); return; } b(); }");

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn predicate_of_a_variant_without_a_second_path_is_not_flagged() {
        assert_no_diagnostics(&run("fn f() { if k.is_alpha() { a(); } b(); }"));
    }

    #[test]
    fn predicate_that_names_no_variant_is_not_flagged() {
        assert_no_diagnostics(&run("fn f() { if k.is_terminal() { a() } else { b() } }"));
    }

    #[test]
    fn result_pattern_that_takes_the_variant_whole_is_not_flagged() {
        let source = "fn f() { if let Ok(x) = k { a(x) } else { b() } }";

        assert_no_diagnostics(&run_with(source, result(), Exhaustiveness::Exhaustive));
    }

    #[test]
    fn snake_case_keeps_an_acronym_together() {
        assert_eq!(snake_case("HTTPError"), "http_error");
    }

    #[test]
    fn snake_case_separates_the_words_of_a_name() {
        assert_eq!(snake_case("UnrecognizedReport"), "unrecognized_report");
    }

    #[test]
    fn trait_send_growth() {
        fn assert_send<T: Send>() {}

        assert_send::<Growth>();
    }

    #[test]
    fn trait_send_single_variant_branch() {
        fn assert_send<T: Send>() {}

        assert_send::<SingleVariantBranch>();
    }

    #[test]
    fn trait_send_tested_enum() {
        fn assert_send<T: Send>() {}

        assert_send::<TestedEnum>();
    }

    #[test]
    fn trait_send_whole() {
        fn assert_send<T: Send>() {}

        assert_send::<Whole>();
    }

    #[test]
    fn trait_sync_growth() {
        fn assert_sync<T: Sync>() {}

        assert_sync::<Growth>();
    }

    #[test]
    fn trait_sync_single_variant_branch() {
        fn assert_sync<T: Sync>() {}

        assert_sync::<SingleVariantBranch>();
    }

    #[test]
    fn trait_sync_tested_enum() {
        fn assert_sync<T: Sync>() {}

        assert_sync::<TestedEnum>();
    }

    #[test]
    fn trait_sync_whole() {
        fn assert_sync<T: Sync>() {}

        assert_sync::<Whole>();
    }

    #[test]
    fn trait_unpin_growth() {
        fn assert_unpin<T: Unpin>() {}

        assert_unpin::<Growth>();
    }

    #[test]
    fn trait_unpin_single_variant_branch() {
        fn assert_unpin<T: Unpin>() {}

        assert_unpin::<SingleVariantBranch>();
    }

    #[test]
    fn trait_unpin_tested_enum() {
        fn assert_unpin<T: Unpin>() {}

        assert_unpin::<TestedEnum>();
    }

    #[test]
    fn trait_unpin_whole() {
        fn assert_unpin<T: Unpin>() {}

        assert_unpin::<Whole>();
    }
}
