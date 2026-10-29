use std::path::Path;

use stiles_rust::decorations::{FnSignature, TypePathRef};
use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, Severity};

const RULE_ID: RuleId = RuleId::new("lint.pub-anyhow-result");

/// The definition path of `anyhow::Error`
///
/// `anyhow` declares `Error` at its crate root, so the module segment list
/// is empty. The comparison uses the definition path, so a local type named
/// `Error` never matches. `anyhow::Result<T>` matches through any alias or
/// import, because it resolves to `Result<T, anyhow::Error>`.
const ANYHOW_ERROR: TypePathRef<'static> = TypePathRef::builder()
    .krate("anyhow")
    .modules(&[])
    .name("Error")
    .build();

/// Flags a public function whose error type is `anyhow::Error`
///
/// `anyhow` belongs where an error is reported. A public function that
/// returns it hands every caller a type the caller can only print or
/// downcast. The caller cannot match on what went wrong.
///
/// The rule reads the error type from the [`FnSignature`] decoration, and
/// stays silent when the decoration is missing. For an `async fn`, the
/// decoration carries the error type of the future's output, so an
/// `async fn` is reported the same way as a plain one.
///
/// Visibility is the boundary the rule can see. A bare `pub` is reported,
/// in a binary crate too, and `pub(crate)`, `pub(super)`, `pub(in ..)`,
/// and private functions are not. A method in a trait takes the trait's
/// visibility, so a method of a `pub trait` is reported on the trait,
/// where the signature is fixed. A declaration without a body is reported
/// only when the provider decorates it. A method in a trait `impl` is not
/// reported, because the implementation cannot change the signature. A
/// second report there would point at code the author cannot fix.
///
/// The rule skips a function with a `#[command]` attribute, under any path,
/// because the command framework fixes its signature. It skips every
/// function in a file named `main.rs`, because that is where a binary
/// reports its errors. It skips test code. That is a function under
/// `#[test]` or under a `cfg` that holds only in tests, and every file under
/// a `tests` or `benches` directory.
///
/// # Examples
///
/// The rule reads the signature the Rust provider attaches as a decoration.
/// This example attaches it by hand.
///
/// ```
/// use pub_anyhow_result::PubAnyhowResult;
/// use stiles_rust::decorations::{ErrorType, FnSignature, ResolvedType, ResultKind, ReturnMode, TypePath};
/// use stiles_testing::{decorate, execute, parse};
/// use stiles_types::{DecorationMap, Language};
///
/// let mut tree = parse("pub fn load() -> anyhow::Result<()> { Ok(()) }", Language::Rust);
/// let function = tree.root_node().named_child(0).unwrap();
/// let error = TypePath::builder().krate("anyhow").modules([] as [&str; 0]).name("Error").build();
/// let signature = FnSignature::builder()
///     .return_type(ResolvedType::new("Result<()>".into()).with_result_kind(ResultKind::Result))
///     .error_type(ErrorType::Named(error))
///     .return_mode(ReturnMode::Direct)
///     .build();
/// let mut decorations = DecorationMap::new();
/// decorations.insert(function.id(), signature);
/// decorate(&mut tree, decorations);
/// let mut passes = vec![PubAnyhowResult::into_lint_pass()];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
///
/// [`FnSignature`]: stiles_rust::decorations::FnSignature
pub struct PubAnyhowResult;

impl PubAnyhowResult {
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = PubAnyhowResult::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

impl RustLintPass for PubAnyhowResult {
    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_signature(node)
    }

    fn check_function_signature_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        check_signature(node)
    }
}

/// Reports the signature at `node` when it is public and returns `anyhow`
///
/// A definition and a trait declaration are one position for this rule,
/// because both fix the signature every caller reads.
fn check_signature(node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
    if !is_public(node) {
        return Vec::new();
    }

    let Some(signature) = node.decoration::<FnSignature>() else {
        return Vec::new();
    };
    let Some(error_type) = signature.error_type() else {
        return Vec::new();
    };
    if !error_type.is(ANYHOW_ERROR) {
        return Vec::new();
    }

    let span = node.span();
    let file = span.file();
    if is_main_file(file) || is_test_target(file) {
        return Vec::new();
    }
    if outer_attributes(node).iter().any(is_command_attribute) {
        return Vec::new();
    }
    if is_test_code(node) {
        return Vec::new();
    }

    let Some(return_type) = node.child_by_field_name("return_type") else {
        return Vec::new();
    };
    let Some(name) = node.child_by_field_name("name") else {
        return Vec::new();
    };

    vec![
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Error)
            .message(format!(
                "`{}` is public and its error type is `anyhow::Error`; return an error type the \
             caller can match on",
                name.text()
            ))
            .span(return_type.span())
            .build(),
    ]
}

/// Returns whether the signature is reachable under a bare `pub`
///
/// A method in a trait has no visibility of its own and takes the trait's.
/// A method in an `impl` block carries its own, and a method in a trait
/// `impl` carries none, so it is never public here.
fn is_public(node: &DecoratedNode<'_>) -> bool {
    let trait_item = node
        .parent()
        .filter(|parent| parent.kind() == "declaration_list")
        .and_then(|list| list.parent())
        .filter(|grandparent| grandparent.kind() == "trait_item");

    match trait_item {
        Some(trait_item) => has_bare_pub(&trait_item),
        None => has_bare_pub(node),
    }
}

/// Returns whether the item carries a bare `pub` visibility modifier
///
/// A restricted form such as `pub(crate)` holds its scope as a named child,
/// so a bare `pub` is the one with no named children.
fn has_bare_pub(item: &DecoratedNode<'_>) -> bool {
    item.named_children()
        .iter()
        .any(|child| child.kind() == "visibility_modifier" && child.named_child_count() == 0)
}

/// Returns whether the file is a crate's `main.rs`
fn is_main_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "main.rs")
}

/// Returns whether any directory in a path is `tests` or `benches`
///
/// Cargo builds those directories only for test and benchmark targets, so
/// the code there is test code. The file name itself does not count.
fn is_test_target(path: &Path) -> bool {
    let Some(directory) = path.parent() else {
        return false;
    };
    directory
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .any(|name| name == "tests" || name == "benches")
}

/// Returns whether the node sits in code that only a test build compiles
///
/// The walk visits the node and every ancestor. An item writes its gate in
/// front of itself, and a module or a file writes it inside, so both
/// spellings count at every level. A `#[test]` on an enclosing function
/// counts too, for a helper declared inside a test.
fn is_test_code(node: &DecoratedNode<'_>) -> bool {
    let mut current = Some(*node);
    while let Some(item) = current {
        let outer = outer_attributes(&item);
        if outer.iter().any(is_test_attribute) || outer.iter().any(is_test_cfg) {
            return true;
        }
        if inner_attributes(&item).iter().any(is_test_cfg) {
            return true;
        }
        current = item.parent();
    }
    false
}

/// Returns the `attribute` nodes written in front of the node
///
/// Tree-sitter makes each attribute a sibling of the item it decorates. The
/// walk reads backward from the node over attributes and comments. It stops
/// at the first sibling that is neither, so an attribute on the item before
/// this one never reaches it.
fn outer_attributes<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    let siblings = parent.named_children();
    let Some(position) = siblings
        .iter()
        .position(|sibling| sibling.id() == node.id())
    else {
        return Vec::new();
    };

    let mut attributes = Vec::new();
    for sibling in siblings[..position].iter().rev() {
        match sibling.kind() {
            "attribute_item" => attributes.extend(sibling.named_child(0)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    attributes
}

/// Returns the `attribute` nodes of the `#![..]` items that open the node
///
/// A file and a module body hold their inner attributes as leading
/// children. The walk stops at the first child that is neither an inner
/// attribute nor a comment.
fn inner_attributes<'a>(node: &DecoratedNode<'a>) -> Vec<DecoratedNode<'a>> {
    let mut attributes = Vec::new();
    for child in node.named_children() {
        match child.kind() {
            "inner_attribute_item" => attributes.extend(child.named_child(0)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
    }
    attributes
}

/// Returns the last segment of an attribute's path
///
/// `#[tauri::command]` and `#[command]` name one macro, and which one a
/// file writes depends on its imports.
fn attribute_name<'a>(attribute: &DecoratedNode<'a>) -> Option<&'a str> {
    let path = attribute.named_child(0)?;
    let name = match path.kind() {
        "scoped_identifier" => path.child_by_field_name("name")?,
        _ => path,
    };
    Some(name.text())
}

/// Returns whether the attribute is a `#[command]` under any path
fn is_command_attribute(attribute: &DecoratedNode<'_>) -> bool {
    attribute_name(attribute) == Some("command")
}

/// Returns whether the attribute marks a test, such as `#[tokio::test]`
fn is_test_attribute(attribute: &DecoratedNode<'_>) -> bool {
    attribute_name(attribute) == Some("test")
}

/// Returns whether the attribute is a `cfg` that holds only in tests
///
/// `cfg_attr` does not count, because it applies another attribute rather
/// than removing the item.
fn is_test_cfg(attribute: &DecoratedNode<'_>) -> bool {
    let Some(path) = attribute.named_child(0) else {
        return false;
    };
    if path.kind() != "identifier" || path.text() != "cfg" {
        return false;
    }
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return false;
    };

    let predicates = predicates(&arguments);
    match predicates.as_slice() {
        [predicate] => implies_test(predicate),
        _ => false,
    }
}

/// Splits a `cfg` token tree into its comma-separated predicates
///
/// Each predicate is the list of tokens between two commas, with the
/// tree's own parentheses left out.
fn predicates<'a>(tree: &DecoratedNode<'a>) -> Vec<Vec<DecoratedNode<'a>>> {
    let mut predicates = Vec::new();
    let mut current = Vec::new();
    for position in 0..tree.child_count() as u32 {
        let Some(token) = tree.child(position) else {
            continue;
        };
        match token.kind() {
            "(" | ")" => {}
            "," => predicates.push(std::mem::take(&mut current)),
            _ => current.push(token),
        }
    }
    if !current.is_empty() {
        predicates.push(current);
    }
    predicates
}

/// Returns whether a `cfg` predicate can hold only when `test` holds
///
/// `all(..)` needs one operand that implies `test`, and `any(..)` needs every
/// operand to. `not(..)` never does, and neither does a key-value predicate,
/// so `cfg(feature = "test")` and `cfg(not(test))` are not test code.
fn implies_test(predicate: &[DecoratedNode<'_>]) -> bool {
    match predicate {
        [word] => word.kind() == "identifier" && word.text() == "test",
        [operator, operands] if operands.kind() == "token_tree" => {
            let operands = predicates(operands);
            match operator.text() {
                "all" => operands.iter().any(|operand| implies_test(operand)),
                "any" => {
                    !operands.is_empty() && operands.iter().all(|operand| implies_test(operand))
                }
                _ => false,
            }
        }
        _ => false,
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for PubAnyhowResult {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

stiles_plugin::export_lints![rust: [PubAnyhowResult]];

#[cfg(test)]
mod tests {
    use stiles_rust::decorations::{ErrorType, ResolvedType, ResultKind, ReturnMode, TypePath};
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use stiles_types::{DecoratedTree, DecorationMap, Language};

    use super::*;

    /// Returns the signature the provider records for `anyhow::Result<()>`
    fn anyhow_signature() -> FnSignature {
        let ret = ResolvedType::new("Result<()>".into()).with_result_kind(ResultKind::Result);
        let error = ErrorType::Named(
            TypePath::builder()
                .krate("anyhow")
                .modules([] as [&str; 0])
                .name("Error")
                .build(),
        );
        FnSignature::builder()
            .return_type(ret)
            .error_type(error)
            .return_mode(ReturnMode::Direct)
            .build()
    }

    /// Returns the signature the provider records for an `async fn` whose
    /// future yields `anyhow::Result<()>`
    fn anyhow_async_signature() -> FnSignature {
        let ret = ResolvedType::new("Result<()>".into()).with_result_kind(ResultKind::Result);
        let error = ErrorType::Named(
            TypePath::builder()
                .krate("anyhow")
                .modules([] as [&str; 0])
                .name("Error")
                .build(),
        );
        FnSignature::builder()
            .return_type(ret)
            .error_type(error)
            .return_mode(ReturnMode::Awaited)
            .build()
    }

    /// Returns the signature the provider records for `Result<(), Error>`
    /// where `Error` is the crate's own type
    ///
    /// The name renders as `Error` like `anyhow`'s, so only the definition
    /// path tells the two apart.
    fn local_error_signature() -> FnSignature {
        let ret =
            ResolvedType::new("Result<(), Error>".into()).with_result_kind(ResultKind::Result);
        let error = ErrorType::Named(
            TypePath::builder()
                .krate("my_crate")
                .modules(["error"])
                .name("Error")
                .build(),
        );
        FnSignature::builder()
            .return_type(ret)
            .error_type(error)
            .return_mode(ReturnMode::Direct)
            .build()
    }

    /// Runs the rule with `signature` on every function in `source`
    ///
    /// The rule reads a decoration the Rust provider makes, and these tests
    /// run no provider, so they attach it by hand.
    fn run_with(source: &str, signature: FnSignature) -> Vec<Diagnostic> {
        run_decorated(parse(source, Language::Rust), signature)
    }

    /// Runs the rule on `source` as if it were the file at `path`
    ///
    /// `parse` names every file `<test>`, so this parses again with the
    /// grammar it used and a path of the test's own.
    fn run_in_file(source: &str, path: &Path) -> Vec<Diagnostic> {
        let parsed = parse(source, Language::Rust);
        let grammar = parsed.root_node().raw().language().to_owned();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .expect("the grammar should load");
        let tree = parser.parse(source, None).expect("the source should parse");
        let tree = DecoratedTree::builder()
            .tree(tree)
            .language(Language::Rust)
            .source(source.to_owned())
            .file(path.to_path_buf())
            .build();

        run_decorated(tree, anyhow_signature())
    }

    /// Attaches `signature` to every function in `tree` and runs the rule
    fn run_decorated(mut tree: DecoratedTree, signature: FnSignature) -> Vec<Diagnostic> {
        let mut ids = Vec::new();
        collect_signature_ids(&tree.root_node(), &mut ids);

        let mut map = DecorationMap::new();
        for id in ids {
            map.insert(id, signature.clone());
        }
        decorate(&mut tree, map);

        let mut passes = vec![PubAnyhowResult::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    /// Runs the rule with every function decorated as `anyhow::Result<()>`
    fn run(source: &str) -> Vec<Diagnostic> {
        run_with(source, anyhow_signature())
    }

    fn collect_signature_ids(node: &DecoratedNode<'_>, ids: &mut Vec<usize>) {
        match node.kind() {
            "function_item" | "function_signature_item" => ids.push(node.id()),
            _ => {}
        }
        for child in node.named_children() {
            collect_signature_ids(&child, ids);
        }
    }

    #[test]
    fn async_pub_fn_flagged() {
        let diagnostics = run_with(
            "pub async fn load() -> anyhow::Result<()> {}",
            anyhow_async_signature(),
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn attribute_on_earlier_item_does_not_exempt() {
        let diagnostics = run("#[command]\nfn a() {}\npub fn b() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn attribute_on_earlier_item_does_not_exempt_the_later_function() {
        let diagnostics = run("#[command]\nfn a() {}\npub fn b() -> Result<()> {}");

        assert_diagnostic(&diagnostics[0]).message_contains("`b`");
    }

    #[test]
    fn boxed_error_type_not_flagged() {
        let ret = ResolvedType::new("Result<(), Box<dyn Error>>".into())
            .with_result_kind(ResultKind::Result);
        let error = ErrorType::Named(
            TypePath::builder()
                .krate("alloc")
                .modules(["boxed"])
                .name("Box")
                .build(),
        );
        let signature = FnSignature::builder()
            .return_type(ret)
            .error_type(error)
            .return_mode(ReturnMode::Direct)
            .build();

        let diagnostics = run_with("pub fn f() -> Result<(), Box<dyn Error>> {}", signature);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_all_with_test_not_flagged() {
        let diagnostics = run("#[cfg(all(test, unix))]\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_any_of_only_tests_not_flagged() {
        let diagnostics = run("#[cfg(any(test, all(test, unix)))]\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    /// The function also builds whenever the feature is on, so it ships
    #[test]
    fn cfg_any_with_test_and_feature_flagged() {
        let diagnostics = run("#[cfg(any(test, feature = \"x\"))]\npub fn f() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_attr_test_flagged() {
        let diagnostics = run("#[cfg_attr(test, allow(dead_code))]\npub fn f() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_empty_any_flagged() {
        let diagnostics = run("#[cfg(any())]\npub fn f() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_feature_named_test_flagged() {
        let diagnostics = run("#[cfg(feature = \"test\")]\npub fn f() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_not_test_flagged() {
        let diagnostics = run("#[cfg(not(test))]\npub fn f() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_test_behind_doc_comment_not_flagged() {
        let diagnostics = run("#[cfg(test)]\n/// Helps\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_module_not_flagged() {
        let diagnostics = run("#[cfg(test)]\nmod tests {\n    pub fn f() -> Result<()> {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_on_function_not_flagged() {
        let diagnostics = run("#[cfg(test)]\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_on_impl_not_flagged() {
        let diagnostics = run("#[cfg(test)]\nimpl S {\n    pub fn f() -> Result<()> {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn command_attribute_after_other_attribute_not_flagged() {
        let diagnostics = run("#[command]\n/// Runs\n#[inline]\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn command_attribute_exempts_only_its_function() {
        let diagnostics =
            run("#[command]\npub fn a() -> Result<()> {}\n\npub fn b() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn command_attribute_exempts_only_its_function_and_flags_the_other() {
        let diagnostics =
            run("#[command]\npub fn a() -> Result<()> {}\n\npub fn b() -> Result<()> {}");

        assert_diagnostic(&diagnostics[0]).message_contains("`b`");
    }

    #[test]
    fn command_attribute_not_flagged() {
        let diagnostics = run("#[command]\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn command_attribute_with_arguments_not_flagged() {
        let diagnostics =
            run("#[tauri::command(rename_all = \"snake_case\")]\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn command_attribute_with_path_not_flagged() {
        let diagnostics = run("#[tauri::command]\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn diagnostic_is_reported_once_for_one_pub_fn() {
        let diagnostics = run("pub fn f() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn diagnostic_spans_the_return_type() {
        let diagnostics = run("pub fn f() -> Result<()> {}");

        assert_diagnostic(&diagnostics[0]).has_span("<test>", 14..24);
    }

    /// Only the crate root's `Error` is `anyhow::Error`
    #[test]
    fn error_in_anyhow_submodule_not_flagged() {
        let ret =
            ResolvedType::new("Result<(), Error>".into()).with_result_kind(ResultKind::Result);
        let error = ErrorType::Named(
            TypePath::builder()
                .krate("anyhow")
                .modules(["kind"])
                .name("Error")
                .build(),
        );
        let signature = FnSignature::builder()
            .return_type(ret)
            .error_type(error)
            .return_mode(ReturnMode::Direct)
            .build();

        let diagnostics = run_with("pub fn f() -> Result<(), Error> {}", signature);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn generic_error_type_not_flagged() {
        let ret = ResolvedType::new("Result<(), E>".into()).with_result_kind(ResultKind::Result);
        let signature = FnSignature::builder()
            .return_type(ret)
            .error_type(ErrorType::Generic)
            .return_mode(ReturnMode::Direct)
            .build();

        let diagnostics = run_with("pub fn f<E>() -> Result<(), E> {}", signature);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn helper_inside_test_function_not_flagged() {
        let diagnostics = run("#[test]\nfn t() {\n    pub fn helper() -> Result<()> {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inherent_private_method_not_flagged() {
        let diagnostics = run("impl S {\n    fn f(&self) -> Result<()> {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inherent_pub_method_flagged() {
        let diagnostics = run("impl S {\n    pub fn f(&self) -> Result<()> {}\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inherent_pub_method_flagged_by_name() {
        let diagnostics = run("impl S {\n    pub fn f(&self) -> Result<()> {}\n}");

        assert_diagnostic(&diagnostics[0]).message_contains("`f`");
    }

    #[test]
    fn inner_cfg_test_in_file_not_flagged() {
        let diagnostics = run("#![cfg(test)]\n\npub fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_cfg_test_in_module_not_flagged() {
        let diagnostics = run("mod m {\n    #![cfg(test)]\n    pub fn f() -> Result<()> {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn integration_test_file_not_flagged() {
        let diagnostics = run_in_file(
            "pub fn setup() -> Result<()> {}",
            Path::new("crates/app/tests/common.rs"),
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn is_main_file_with_bin_main_returns_true() {
        assert!(is_main_file(Path::new("src/bin/tool/main.rs")));
    }

    #[test]
    fn is_main_file_with_crate_main_returns_true() {
        assert!(is_main_file(Path::new("crates/app/src/main.rs")));
    }

    #[test]
    fn is_main_file_with_library_file_returns_false() {
        assert!(!is_main_file(Path::new("src/lib.rs")));
    }

    #[test]
    fn is_main_file_with_main_directory_returns_false() {
        assert!(!is_main_file(Path::new("src/main.rs/run.rs")));
    }

    #[test]
    fn is_main_file_with_suffixed_name_returns_false() {
        assert!(!is_main_file(Path::new("src/domain_main.rs")));
    }

    #[test]
    fn is_test_target_with_benchmark_path_returns_true() {
        assert!(is_test_target(Path::new("benches/throughput.rs")));
    }

    #[test]
    fn is_test_target_with_integration_test_path_returns_true() {
        assert!(is_test_target(Path::new("crates/core/tests/common/mod.rs")));
    }

    #[test]
    fn is_test_target_with_source_path_returns_false() {
        assert!(!is_test_target(Path::new("crates/core/src/tests.rs")));
    }

    #[test]
    fn library_file_flagged() {
        let diagnostics = run_in_file(
            "pub fn run() -> Result<()> {}",
            Path::new("crates/app/src/lib.rs"),
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn local_error_type_not_flagged() {
        let diagnostics = run_with(
            "pub fn f() -> Result<(), Error> {}",
            local_error_signature(),
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn main_file_not_flagged() {
        let diagnostics = run_in_file(
            "pub fn run() -> Result<()> {}",
            Path::new("crates/app/src/main.rs"),
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn missing_decoration_not_flagged() {
        let tree = parse("pub fn f() -> anyhow::Result<()> {}", Language::Rust);
        let mut passes = vec![PubAnyhowResult::into_lint_pass()];

        let diagnostics = execute(&tree, &mut passes);

        assert_no_diagnostics(&diagnostics);
    }

    /// A function nested in a body is reported like any other, because the
    /// rule reads the visibility the author wrote
    #[test]
    fn nested_pub_fn_flagged() {
        let diagnostics = run("fn outer() {\n    pub fn inner() -> Result<()> {}\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    /// A function nested in a body is reported like any other, because the
    /// rule reads the visibility the author wrote
    #[test]
    fn nested_pub_fn_flagged_by_name() {
        let diagnostics = run("fn outer() {\n    pub fn inner() -> Result<()> {}\n}");

        assert_diagnostic(&diagnostics[0]).message_contains("`inner`");
    }

    #[test]
    fn non_result_return_not_flagged() {
        let ret = ResolvedType::new("u32".into());
        let signature = FnSignature::builder()
            .return_type(ret)
            .return_mode(ReturnMode::Direct)
            .build();

        let diagnostics = run_with("pub fn f() -> u32 {}", signature);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn opaque_async_signature_not_flagged() {
        let signature = FnSignature::builder()
            .return_mode(ReturnMode::Opaque)
            .build();

        let diagnostics = run_with("pub async fn f() -> anyhow::Result<()> {}", signature);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn private_fn_not_flagged() {
        let diagnostics = run("fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn private_trait_method_not_flagged() {
        let diagnostics = run("trait Port {\n    fn f(&self) -> Result<()>;\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_crate_fn_not_flagged() {
        let diagnostics = run("pub(crate) fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_crate_trait_method_not_flagged() {
        let diagnostics = run("pub(crate) trait Port {\n    fn f(&self) -> Result<()>;\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_fn_flagged() {
        let diagnostics = run("pub fn run() -> anyhow::Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn pub_fn_flagged_with_the_anyhow_message() {
        let diagnostics = run("pub fn run() -> anyhow::Result<()> {}");

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.pub-anyhow-result")
            .has_severity(Severity::Error)
            .message_contains("`run` is public and its error type is `anyhow::Error`");
    }

    #[test]
    fn pub_fn_in_private_module_flagged() {
        let diagnostics = run("mod m {\n    pub fn f() -> Result<()> {}\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn pub_in_path_fn_not_flagged() {
        let diagnostics = run("pub(in crate::a) fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_method_on_private_type_flagged() {
        let diagnostics = run("struct S;\nimpl S {\n    pub fn f(&self) -> Result<()> {}\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn pub_self_fn_not_flagged() {
        let diagnostics = run("pub(self) fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_super_fn_not_flagged() {
        let diagnostics = run("pub(super) fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_trait_async_method_flagged() {
        let diagnostics = run_with(
            "pub trait Port {\n    async fn f(&self) -> Result<()> {}\n}",
            anyhow_async_signature(),
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn pub_trait_declaration_flagged() {
        let diagnostics = run("pub trait Port {\n    fn f(&self) -> Result<()>;\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn pub_trait_declaration_flagged_by_method_name() {
        let diagnostics = run("pub trait Port {\n    fn f(&self) -> Result<()>;\n}");

        assert_diagnostic(&diagnostics[0]).message_contains("`f`");
    }

    #[test]
    fn pub_trait_default_method_flagged() {
        let diagnostics = run("pub trait Port {\n    fn f(&self) -> Result<()> {}\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn pub_trait_in_cfg_test_module_not_flagged() {
        let diagnostics = run(
            "#[cfg(test)]\nmod tests {\n    pub trait T {\n        fn f() -> Result<()>;\n    }\n}",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pub_trait_reports_each_method() {
        let diagnostics = run(
            "pub trait Port {\n    fn a(&self) -> Result<()>;\n    fn b(&self) -> Result<()>;\n}",
        );

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn similarly_named_attribute_flagged() {
        let diagnostics = run("#[commands]\npub fn f() -> Result<()> {}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn test_attribute_not_flagged() {
        let diagnostics = run("#[tokio::test]\npub async fn f() -> Result<()> {}");

        assert_no_diagnostics(&diagnostics);
    }

    /// The implementation cannot change the trait's signature, so the
    /// report belongs on the trait
    #[test]
    fn trait_impl_method_not_flagged() {
        let diagnostics = run("impl Port for S {\n    fn f(&self) -> Result<()> {}\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<PubAnyhowResult>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<PubAnyhowResult>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<PubAnyhowResult>();
    }
}
