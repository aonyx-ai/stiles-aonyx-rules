use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use whisker_rust::RustLintPass;
use whisker_types::{DecoratedNode, Diagnostic, RuleId, Severity, Span};

/// Flags a `struct` or `enum` that has no `Send`, `Sync`, or `Unpin` test
///
/// A type that quietly loses one of these traits breaks callers far away from
/// the field that caused it. The tests pin the traits, so the loss surfaces at
/// the definition instead.
///
/// A call proves a trait when it sits in a test, names the type in a
/// turbofish, and resolves to a callee whose generics carry that bound.
/// `assert_send::<Foo>()` inside a `#[test]` is the usual form. Shipped code
/// that happens to require the bound does not count, because the lint asks for
/// a test rather than for the constraint. A `#[test]` whose name starts with
/// `trait_not_send`, `trait_not_sync`, or `trait_not_unpin` counts too. It
/// counts only when a comment gives the reason and puts the type in backticks.
///
/// The lint skips types under a `#[cfg(test)]` module and types in the `tests`
/// and `benches` directories, because those types are fixtures. It skips types
/// declared inside a function, because a sibling test module cannot name them.
/// It also skips type aliases and unions. Whisker reads one file at a time, so
/// the proof must sit in the same file as the type.
pub struct MissingTraitTests;

impl RustLintPass for MissingTraitTests {
    fn check_source_file(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_file(node)
    }
}

/// Reports every type in one file that is missing a trait test
///
/// The lint fires on the absence of a test, so no single node carries the
/// answer. `source_file` is the one node that sees the whole file, and the
/// walker visits it once. The pass runs its own traversal from there.
fn check_file<'a>(root: &DecoratedNode<'a>) -> Vec<Diagnostic> {
    if is_test_target(root.span().file()) {
        return Vec::new();
    }

    let mut scan = FileScan::default();
    scan.collect_assertions(root, Scope::Shipped);
    scan.walk_items(root, Scope::Shipped);
    scan.resolve_calls();
    scan.report()
}

/// One of the three traits every custom type must have a test for
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
enum AutoTrait {
    Send,
    Sync,
    Unpin,
}

impl AutoTrait {
    /// The three traits, in the order the diagnostic lists them
    const ALL: [AutoTrait; 3] = [AutoTrait::Send, AutoTrait::Sync, AutoTrait::Unpin];

    /// Returns the auto trait a bound names, if it names one
    fn from_bound(bound: &str) -> Option<Self> {
        match bound.trim() {
            "Send" => Some(AutoTrait::Send),
            "Sync" => Some(AutoTrait::Sync),
            "Unpin" => Some(AutoTrait::Unpin),
            _ => None,
        }
    }

    /// Returns the name a negative test for this trait must start with
    fn negative_test_prefix(self) -> &'static str {
        match self {
            AutoTrait::Send => "trait_not_send",
            AutoTrait::Sync => "trait_not_sync",
            AutoTrait::Unpin => "trait_not_unpin",
        }
    }

    /// Returns the trait name as it appears in Rust source
    fn as_str(self) -> &'static str {
        match self {
            AutoTrait::Send => "Send",
            AutoTrait::Sync => "Sync",
            AutoTrait::Unpin => "Unpin",
        }
    }
}

/// Whether a container holds shipped items or items that only tests use
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Scope {
    Shipped,
    Test,
}

/// A type definition that needs trait tests
#[derive(Clone, Eq, PartialEq, Debug)]
struct Candidate<'a> {
    name: &'a str,
    span: Span,
}

/// A turbofish call that may prove traits for the types it names
///
/// The call proves a trait only when `callee` resolves to a function whose
/// generics carry that bound.
#[derive(Clone, Eq, PartialEq, Debug)]
struct Call<'a> {
    callee: &'a str,
    types: Vec<&'a str>,
}

/// The types one file defines and the traits its tests prove
///
/// The scan gathers calls before it knows which callees carry a bound, so
/// `resolve_calls` turns the calls into proof once both halves are in.
#[derive(Clone, Eq, PartialEq, Debug, Default)]
struct FileScan<'a> {
    candidates: Vec<Candidate<'a>>,
    helpers: HashMap<&'a str, BTreeSet<AutoTrait>>,
    calls: Vec<Call<'a>>,
    proven: HashMap<&'a str, BTreeSet<AutoTrait>>,
}

impl<'a> FileScan<'a> {
    /// Walks the items of one container and records what precedes each item
    ///
    /// Tree-sitter leaves an item's attributes and comments beside it rather
    /// than inside it, so the walk carries them forward until it reaches that
    /// item.
    fn walk_items(&mut self, container: &DecoratedNode<'a>, scope: Scope) {
        let mut attributes: Vec<&'a str> = Vec::new();
        let mut comments: Vec<&'a str> = Vec::new();

        for child in container.named_children() {
            match child.kind() {
                "line_comment" | "block_comment" => {
                    comments.push(child.text());
                    continue;
                }
                "attribute_item" => {
                    attributes.push(child.text());
                    continue;
                }
                "struct_item" | "enum_item" => self.record_candidate(&child, scope, &attributes),
                "mod_item" => {
                    let inner = match attributes.iter().any(|a| cfg_mentions_test(a)) {
                        true => Scope::Test,
                        false => scope,
                    };
                    if let Some(body) = child.child_by_field_name("body") {
                        self.walk_items(&body, inner);
                    }
                }
                "function_item" => self.record_negative_test(&child, &attributes, &comments),
                _ => {}
            }

            attributes.clear();
            comments.clear();
        }
    }

    /// Records a type definition, unless `#[cfg(test)]` hides it from the build
    fn record_candidate(&mut self, item: &DecoratedNode<'a>, scope: Scope, attributes: &[&str]) {
        match scope {
            Scope::Test => return,
            Scope::Shipped => {}
        }

        if attributes.iter().any(|a| cfg_mentions_test(a)) {
            return;
        }

        let Some(name) = item.child_by_field_name("name") else {
            return;
        };

        self.candidates.push(Candidate {
            name: name.text(),
            span: name.span(),
        });
    }

    /// Collects the bounded helpers and the turbofish calls tests make
    ///
    /// Helpers count wherever they are declared, since declaring one proves
    /// nothing on its own and a test may borrow a helper that shipped code
    /// owns. Calls count only inside a test, so a shipped call that happens to
    /// require the bound cannot stand in for the missing test. A `#[cfg(test)]`
    /// module and a `#[test]` function both open test scope.
    fn collect_assertions(&mut self, node: &DecoratedNode<'a>, scope: Scope) {
        match node.kind() {
            "function_item" => self.record_helper(node),
            "generic_function" => self.record_call(node, scope),
            _ => {}
        }

        let mut attributes: Vec<&'a str> = Vec::new();
        for child in node.named_children() {
            match child.kind() {
                "line_comment" | "block_comment" => continue,
                "attribute_item" => {
                    attributes.push(child.text());
                    continue;
                }
                "mod_item" | "function_item" => {
                    let inner = match attributes
                        .iter()
                        .any(|a| cfg_mentions_test(a) || is_test_attribute(a))
                    {
                        true => Scope::Test,
                        false => scope,
                    };
                    self.collect_assertions(&child, inner);
                }
                _ => self.collect_assertions(&child, scope),
            }

            attributes.clear();
        }
    }

    /// Records a function whose generics carry auto trait bounds
    ///
    /// Bounds written inline and bounds written in a `where` clause express
    /// the same constraint, so both are read. A `type_parameters` child and a
    /// `where_clause` child each hold entries with a `bounds` field, which is
    /// why one loop covers both.
    fn record_helper(&mut self, item: &DecoratedNode<'a>) {
        let Some(name) = item.child_by_field_name("name") else {
            return;
        };

        let mut traits = BTreeSet::new();
        for child in item.named_children() {
            match child.kind() {
                "type_parameters" | "where_clause" => {
                    for entry in child.named_children() {
                        collect_bound_traits(&entry, &mut traits);
                    }
                }
                _ => {}
            }
        }

        if traits.is_empty() {
            return;
        }

        self.helpers.entry(name.text()).or_default().extend(traits);
    }

    /// Records a call that names types in a turbofish, such as `f::<Foo>()`
    ///
    /// Only a call under [`Scope::Test`] is recorded, because the lint asks
    /// for a test and shipped code cannot supply one.
    fn record_call(&mut self, node: &DecoratedNode<'a>, scope: Scope) {
        match scope {
            Scope::Shipped => return,
            Scope::Test => {}
        }

        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let Some(callee) = last_path_segment(&function) else {
            return;
        };
        let Some(arguments) = node.child_by_field_name("type_arguments") else {
            return;
        };

        let types: Vec<&'a str> = arguments
            .named_children()
            .iter()
            .filter_map(base_type_name)
            .collect();

        if types.is_empty() {
            return;
        }

        self.calls.push(Call { callee, types });
    }

    /// Credits the types a documented negative test excuses from a trait
    ///
    /// The body of such a test cannot assert the absence of an auto trait. The
    /// comment is the only place that names the type the test covers. The
    /// comment may sit above the test or inside it.
    fn record_negative_test(
        &mut self,
        item: &DecoratedNode<'a>,
        attributes: &[&str],
        comments: &[&'a str],
    ) {
        if !attributes.iter().any(|a| is_test_attribute(a)) {
            return;
        }

        let Some(test_name) = item.child_by_field_name("name") else {
            return;
        };
        let test_name = test_name.text();

        let Some(auto_trait) = AutoTrait::ALL
            .into_iter()
            .find(|candidate| test_name.starts_with(candidate.negative_test_prefix()))
        else {
            return;
        };

        let mut comments: Vec<&'a str> = comments.to_vec();
        if let Some(body) = item.child_by_field_name("body") {
            collect_comments(&body, &mut comments);
        }

        let mut explained = false;
        let mut named: Vec<&'a str> = Vec::new();
        for comment in comments {
            let body = comment_body(comment);
            if !body.trim().is_empty() {
                explained = true;
            }
            named.extend(backticked_names(body));
        }

        if !explained {
            return;
        }

        for name in named {
            self.proven.entry(name).or_default().insert(auto_trait);
        }
    }

    /// Turns every call to a bounded function into proof for the types it names
    ///
    /// The method drains the collected calls, so a second run adds no further
    /// proof.
    fn resolve_calls(&mut self) {
        for Call { callee, types } in std::mem::take(&mut self.calls) {
            let Some(traits) = self.helpers.get(callee) else {
                continue;
            };
            for name in types {
                self.proven.entry(name).or_default().extend(traits.iter());
            }
        }
    }

    /// Reports one diagnostic per type that is missing any of the three tests
    fn report(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        for Candidate { name, span } in &self.candidates {
            let proven = self.proven.get(name);
            let missing: Vec<AutoTrait> = AutoTrait::ALL
                .into_iter()
                .filter(|auto_trait| match proven {
                    Some(traits) => !traits.contains(auto_trait),
                    None => true,
                })
                .collect();

            if missing.is_empty() {
                continue;
            }

            diagnostics.push(Diagnostic::new(
                RuleId::new("lint.missing-trait-tests"),
                Severity::Warn,
                format!("`{name}` has no {} trait test", join_traits(&missing)),
                span.clone(),
            ));
        }

        diagnostics
    }
}

/// Returns whether any component of a path is `tests` or `benches`
///
/// Cargo builds those directories only for test and benchmark targets, so the
/// types there are fixtures. That matches the types under a `#[cfg(test)]`
/// module.
fn is_test_target(path: &Path) -> bool {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .any(|name| name == "tests" || name == "benches")
}

/// Returns whether an attribute is a `cfg` that names the `test` predicate
///
/// The check skips string literals, so `#[cfg(feature = "test")]` does not
/// count while `#[cfg(any(test, unix))]` does.
fn cfg_mentions_test(attribute: &str) -> bool {
    let Some(arguments) = cfg_arguments(attribute) else {
        return false;
    };

    arguments
        .split('"')
        .step_by(2)
        .flat_map(|segment| segment.split(|c: char| !c.is_alphanumeric() && c != '_'))
        .any(|word| word == "test")
}

/// Returns the text inside `#[cfg(...)]`, if the attribute is a `cfg`
fn cfg_arguments(attribute: &str) -> Option<&str> {
    let inner = attribute.trim().strip_prefix("#[")?.strip_suffix(']')?;
    let inner = inner.trim().strip_prefix("cfg")?;
    let inner = inner.trim_start().strip_prefix('(')?;
    inner.strip_suffix(')')
}

/// Returns whether an attribute marks a test, such as `#[test]`
///
/// Arguments are dropped before the path is compared, so
/// `#[tokio::test(flavor = "multi_thread")]` counts. Dropping them also keeps
/// `#[cfg(test)]` out, since its path is `cfg`.
fn is_test_attribute(attribute: &str) -> bool {
    let Some(inner) = attribute.trim().strip_prefix("#[") else {
        return false;
    };
    let Some(inner) = inner.strip_suffix(']') else {
        return false;
    };
    let inner = match inner.split_once('(') {
        Some((path, _)) => path,
        None => inner,
    };

    match inner.trim().rsplit("::").next() {
        Some(segment) => segment.trim() == "test",
        None => false,
    }
}

/// Reads the `bounds` field of a node and inserts the auto traits it names
///
/// Both a constrained type parameter and a `where` predicate carry their
/// bounds under that field, so one reader serves both. The traits land in
/// `traits` rather than a fresh set, because one function may state bounds in
/// both places.
fn collect_bound_traits(node: &DecoratedNode<'_>, traits: &mut BTreeSet<AutoTrait>) {
    let Some(bounds) = node.child_by_field_name("bounds") else {
        return;
    };

    for bound in bounds.named_children() {
        let Some(auto_trait) = AutoTrait::from_bound(bound.text()) else {
            continue;
        };
        traits.insert(auto_trait);
    }
}

/// Collects the text of every comment inside a node
fn collect_comments<'a>(node: &DecoratedNode<'a>, out: &mut Vec<&'a str>) {
    match node.kind() {
        "line_comment" | "block_comment" => out.push(node.text()),
        _ => {}
    }

    for child in node.named_children() {
        collect_comments(&child, out);
    }
}

/// Returns a comment's text with its markers removed
fn comment_body(comment: &str) -> &str {
    let comment = comment.trim();

    if let Some(inner) = comment.strip_prefix("/*") {
        let inner = inner.strip_suffix("*/").unwrap_or(inner);
        return inner.trim_start_matches(['*', '!']);
    }

    let Some(inner) = comment.strip_prefix("//") else {
        return comment;
    };
    inner.trim_start_matches(['/', '!'])
}

/// Returns the type names a comment puts in backticks
///
/// A rustdoc link such as ``[`Foo`]`` yields `Foo`, and a qualified or generic
/// form yields its base name.
fn backticked_names(text: &str) -> Vec<&str> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter_map(base_name)
        .collect()
}

/// Returns the bare name of a possibly qualified, possibly generic type
///
/// Given `std::rc::Rc<T>`, returns `Rc`. Text with no name at all, such as a
/// run of spaces, returns [`None`].
fn base_name(text: &str) -> Option<&str> {
    let text = match text.split_once('<') {
        Some((base, _)) => base,
        None => text,
    };
    let text = match text.rsplit_once("::") {
        Some((_, name)) => name,
        None => text,
    };
    let text = text.trim();

    match text.is_empty() {
        true => None,
        false => Some(text),
    }
}

/// Returns the final segment of a path expression, such as the `f` in `a::f`
fn last_path_segment<'a>(node: &DecoratedNode<'a>) -> Option<&'a str> {
    match node.kind() {
        "identifier" => Some(node.text()),
        "scoped_identifier" => node.child_by_field_name("name").map(|name| name.text()),
        _ => None,
    }
}

/// Returns the name of the type a type argument refers to
///
/// Only the outermost name counts: `Vec<Foo>` yields `Vec`, not `Foo`.
fn base_type_name<'a>(node: &DecoratedNode<'a>) -> Option<&'a str> {
    match node.kind() {
        "type_identifier" | "identifier" => Some(node.text()),
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| base_type_name(&inner)),
        "scoped_type_identifier" | "scoped_identifier" => {
            node.child_by_field_name("name").map(|name| name.text())
        }
        _ => None,
    }
}

/// Joins trait names into the list the diagnostic message uses
///
/// Three traits become `` `Send`, `Sync`, or `Unpin` ``.
fn join_traits(traits: &[AutoTrait]) -> String {
    let names: Vec<String> = traits
        .iter()
        .map(|auto_trait| format!("`{}`", auto_trait.as_str()))
        .collect();

    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, [first])) => format!("{first} or {last}"),
        Some((last, rest)) => format!("{}, or {last}", rest.join(", ")),
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for MissingTraitTests {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.missing-trait-tests")]
    }
}

whisker_plugin::export_lints![rust: [MissingTraitTests]];

#[cfg(test)]
mod tests {
    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass};

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(MissingTraitTests))];
        execute(&tree, &mut passes)
    }

    #[test]
    fn backticked_names_with_rustdoc_link_returns_type() {
        let text = " [`Foo`] holds an [`Rc<T>`].";

        let names = backticked_names(text);

        assert_eq!(names, vec!["Foo", "Rc"]);
    }

    #[test]
    fn base_name_with_generic_path_returns_last_segment() {
        let name = base_name("std::rc::Rc<T>").expect("should find a name");

        assert_eq!(name, "Rc");
    }

    #[test]
    fn base_name_with_only_whitespace_returns_none() {
        let name = base_name("   ");

        assert!(name.is_none());
    }

    #[test]
    fn cfg_mentions_test_with_feature_named_test_returns_false() {
        assert!(!cfg_mentions_test("#[cfg(feature = \"test\")]"));
    }

    #[test]
    fn cfg_mentions_test_with_nested_predicate_returns_true() {
        assert!(cfg_mentions_test("#[cfg(any(test, unix))]"));
    }

    #[test]
    fn cfg_mentions_test_with_plain_test_returns_true() {
        assert!(cfg_mentions_test("#[cfg(test)]"));
    }

    #[test]
    fn cfg_mentions_test_with_unrelated_attribute_returns_false() {
        assert!(!cfg_mentions_test("#[derive(Debug)]"));
    }

    #[test]
    fn combined_bounds_prove_all_three_traits() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_auto() {
        fn assert_auto<T: Send + Sync + Unpin>() {}
        assert_auto::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn comment_body_with_block_comment_strips_markers() {
        let body = comment_body("/* `Foo` is not Sync */");

        assert_eq!(body.trim(), "`Foo` is not Sync");
    }

    #[test]
    fn comment_body_with_doc_comment_strips_markers() {
        let body = comment_body("/// `Foo` is not Sync");

        assert_eq!(body.trim(), "`Foo` is not Sync");
    }

    #[test]
    fn diagnostic_for_the_type_name_is_reported_once() {
        let source = "pub struct Foo;";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn diagnostic_span_covers_the_type_name() {
        let source = "pub struct Foo;";
        let start = source.find("Foo").expect("source should contain the name");

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, start + "Foo".len());
    }

    #[test]
    fn documented_negative_test_is_not_flagged() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    /// `Foo` holds an `Rc`, so it cannot be `Send` or `Sync`.
    #[test]
    fn trait_not_send() {}

    /// `Foo` holds an `Rc`, so it cannot be `Send` or `Sync`.
    #[test]
    fn trait_not_sync() {}

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_source_produces_no_diagnostics() {
        let diagnostics = run("");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn enum_without_tests_is_flagged_as_a_warning_naming_all_three_traits() {
        let diagnostics = run("pub enum Foo { A, B }");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.missing-trait-tests")
            .has_severity(Severity::Warn)
            .message_contains("`Foo` has no `Send`, `Sync`, or `Unpin` trait test");
    }

    #[test]
    fn enum_without_tests_is_flagged_once() {
        let diagnostics = run("pub enum Foo { A, B }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn generic_type_proven_with_arguments_is_not_flagged() {
        let source = r#"
pub struct Foo<'a, T> {
    value: &'a T,
}

#[cfg(test)]
mod tests {
    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Foo<'_, u8>>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Foo<'_, u8>>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Foo<'_, u8>>();
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn helper_defined_at_module_scope_proves_the_trait() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    fn assert_auto<T: Send + Sync + Unpin>() {}

    #[test]
    fn trait_auto() {
        assert_auto::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn helper_with_where_clause_bound_proves_the_trait() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    fn assert_auto<T>()
    where
        T: Send + Sync + Unpin,
    {
    }

    #[test]
    fn trait_auto() {
        assert_auto::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn is_test_attribute_with_arguments_returns_true() {
        assert!(is_test_attribute(
            "#[tokio::test(flavor = \"multi_thread\")]"
        ));
    }

    #[test]
    fn is_test_attribute_with_cfg_test_returns_false() {
        assert!(!is_test_attribute("#[cfg(test)]"));
    }

    #[test]
    fn is_test_attribute_with_plain_test_returns_true() {
        assert!(is_test_attribute("#[test]"));
    }

    #[test]
    fn is_test_attribute_with_qualified_test_returns_true() {
        assert!(is_test_attribute("#[tokio::test]"));
    }

    #[test]
    fn is_test_target_with_benchmark_path_returns_true() {
        assert!(is_test_target(Path::new("crates/whisker/benches/walk.rs")));
    }

    #[test]
    fn is_test_target_with_integration_test_path_returns_true() {
        assert!(is_test_target(Path::new(
            "crates/whisker/tests/support/unreadable.rs"
        )));
    }

    #[test]
    fn is_test_target_with_source_path_returns_false() {
        assert!(!is_test_target(Path::new(
            "crates/whisker-testing/src/lib.rs"
        )));
    }

    #[test]
    fn join_traits_with_one_trait_returns_that_trait() {
        let joined = join_traits(&[AutoTrait::Sync]);

        assert_eq!(joined, "`Sync`");
    }

    #[test]
    fn join_traits_with_three_traits_uses_oxford_comma() {
        let joined = join_traits(&AutoTrait::ALL);

        assert_eq!(joined, "`Send`, `Sync`, or `Unpin`");
    }

    #[test]
    fn join_traits_with_two_traits_uses_or() {
        let joined = join_traits(&[AutoTrait::Send, AutoTrait::Unpin]);

        assert_eq!(joined, "`Send` or `Unpin`");
    }

    #[test]
    fn negative_test_with_attribute_arguments_is_not_flagged() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    /// `Foo` holds an `Rc`, so it cannot be `Send`.
    #[tokio::test(flavor = "multi_thread")]
    async fn trait_not_send() {}

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Foo>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_test_module_types_are_not_flagged() {
        let source = r#"
#[cfg(test)]
mod tests {
    mod fixtures {
        pub struct Stub;
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_type_argument_does_not_prove_inner_type() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Vec<Foo>>();
    }
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("`Foo` has no `Send`, `Sync`, or `Unpin` trait test");
    }

    #[test]
    fn nested_type_argument_leaves_the_inner_type_flagged_once() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Vec<Foo>>();
    }
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn partially_tested_type_is_flagged_once() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn partially_tested_type_names_only_missing_traits() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("`Foo` has no `Sync` or `Unpin` trait test");
    }

    #[test]
    fn scoped_helper_call_proves_the_trait() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    mod helpers {
        pub fn assert_auto<T: Send + Sync + Unpin>() {}
    }

    #[test]
    fn trait_auto() {
        helpers::assert_auto::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn shipped_call_does_not_prove_the_trait() {
        let source = r#"
pub struct Foo;

pub fn spawn<T: Send>(value: T) {}

pub fn run(value: Foo) {
    spawn::<Foo>(value);
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("`Foo` has no `Send`, `Sync`, or `Unpin` trait test");
    }

    #[test]
    fn shipped_call_leaves_the_type_flagged_once() {
        let source = r#"
pub struct Foo;

pub fn spawn<T: Send>(value: T) {}

pub fn run(value: Foo) {
    spawn::<Foo>(value);
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn struct_in_test_module_is_not_flagged() {
        let source = r#"
#[cfg(test)]
mod tests {
    struct Stub;
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_inside_function_is_not_flagged() {
        let diagnostics = run("pub fn build() { struct Local; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_under_cfg_any_test_is_not_flagged() {
        let diagnostics = run("#[cfg(any(test, unix))]\npub struct Foo;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_under_cfg_feature_named_test_is_flagged() {
        let diagnostics = run("#[cfg(feature = \"test\")]\npub struct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn struct_with_all_three_tests_is_not_flagged() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Foo>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Foo>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_without_tests_is_flagged_as_a_warning_naming_all_three_traits() {
        let diagnostics = run("pub struct Foo;");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.missing-trait-tests")
            .has_severity(Severity::Warn)
            .message_contains("`Foo` has no `Send`, `Sync`, or `Unpin` trait test");
    }

    #[test]
    fn struct_without_tests_is_flagged_once() {
        let diagnostics = run("pub struct Foo;");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn test_for_another_type_does_not_prove_this_one() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Bar>();
    }
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("`Foo`");
    }

    #[test]
    fn test_for_another_type_leaves_this_one_flagged_once() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Bar>();
    }
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<MissingTraitTests>();
    }

    #[test]
    fn trait_send_auto_trait() {
        fn assert_send<T: Send>() {}
        assert_send::<AutoTrait>();
    }

    #[test]
    fn trait_send_call() {
        fn assert_send<T: Send>() {}
        assert_send::<Call<'_>>();
    }

    #[test]
    fn trait_send_candidate() {
        fn assert_send<T: Send>() {}
        assert_send::<Candidate<'_>>();
    }

    #[test]
    fn trait_send_file_scan() {
        fn assert_send<T: Send>() {}
        assert_send::<FileScan<'_>>();
    }

    #[test]
    fn trait_send_scope() {
        fn assert_send<T: Send>() {}
        assert_send::<Scope>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<MissingTraitTests>();
    }

    #[test]
    fn trait_sync_auto_trait() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<AutoTrait>();
    }

    #[test]
    fn trait_sync_call() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Call<'_>>();
    }

    #[test]
    fn trait_sync_candidate() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Candidate<'_>>();
    }

    #[test]
    fn trait_sync_file_scan() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FileScan<'_>>();
    }

    #[test]
    fn trait_sync_scope() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Scope>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<MissingTraitTests>();
    }

    #[test]
    fn trait_unpin_auto_trait() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<AutoTrait>();
    }

    #[test]
    fn trait_unpin_call() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Call<'_>>();
    }

    #[test]
    fn trait_unpin_candidate() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Candidate<'_>>();
    }

    #[test]
    fn trait_unpin_file_scan() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<FileScan<'_>>();
    }

    #[test]
    fn trait_unpin_scope() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Scope>();
    }

    #[test]
    fn two_untested_types_produce_two_diagnostics() {
        let diagnostics = run("pub struct Foo;\npub enum Bar { A }");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn two_untested_types_report_bar_second() {
        let diagnostics = run("pub struct Foo;\npub enum Bar { A }");

        assert_diagnostic(&diagnostics[1]).message_contains("`Bar`");
    }

    #[test]
    fn two_untested_types_report_foo_first() {
        let diagnostics = run("pub struct Foo;\npub enum Bar { A }");

        assert_diagnostic(&diagnostics[0]).message_contains("`Foo`");
    }

    #[test]
    fn type_alias_is_not_flagged() {
        let diagnostics = run("pub type Alias = u32;");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn undocumented_negative_test_is_flagged_for_the_missing_send_test() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_not_send() {}

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Foo>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("`Foo` has no `Send` trait test");
    }

    #[test]
    fn undocumented_negative_test_is_flagged_once() {
        let source = r#"
pub struct Foo;

#[cfg(test)]
mod tests {
    #[test]
    fn trait_not_send() {}

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Foo>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Foo>();
    }
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn union_is_not_flagged() {
        let diagnostics = run("pub union Bits { a: u32, b: f32 }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn untested_type_in_shipped_module_is_flagged_by_name() {
        let source = r#"
pub mod inner {
    pub struct Foo;
}
"#;

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("`Foo`");
    }

    #[test]
    fn untested_type_in_shipped_module_is_flagged_once() {
        let source = r#"
pub mod inner {
    pub struct Foo;
}
"#;

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }
}
