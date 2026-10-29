use stiles_rust::decorations::{FnSignature, ResolvedType, TypePathRef};
use stiles_rust::{RuleId, RustLintPass};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, Severity};

const RULE_ID: RuleId = RuleId::new("lint.anyhow-missing-context");

/// The definition path of `anyhow::Error`
///
/// `anyhow` declares `Error` at its crate root, so the module segment list
/// is empty. The comparison uses the definition path, so another crate's
/// `Error` never matches, whatever its rendered name.
const ANYHOW_ERROR: TypePathRef<'static> = TypePathRef::builder()
    .krate("anyhow")
    .modules(&[])
    .name("Error")
    .build();

/// Flags uses of `?` on [`Result`] types without a preceding `.context()`
/// or `.with_context()` call, but only when the enclosing function returns
/// `Result<T, anyhow::Error>`
///
/// A `?` without `.context()` propagates an error with no information
/// about the operation that failed. Context calls build a chain of
/// explanations for how the error occurred, which makes debugging easier.
///
/// A `?` inside a closure, an `async` block, or a `try` block converts
/// into that body's error type, not the enclosing function's. The rule
/// leaves those alone, because `.context(..)` would not compile there.
///
/// # Examples
///
/// The rule reads the types the Rust provider attaches as decorations. This
/// example attaches them by hand.
///
/// ```
/// use anyhow_missing_context::AnyhowMissingContext;
/// use stiles_rust::decorations::{ErrorType, FnSignature, ResolvedType, ResultKind, ReturnMode, TypePath};
/// use stiles_testing::{decorate, execute, parse};
/// use stiles_types::{DecorationMap, Language};
///
/// let mut tree = parse("fn load() -> Result<()> { read()?; }", Language::Rust);
/// let function = tree.root_node().named_child(0).unwrap();
/// let statement = function.child_by_field_name("body").unwrap().named_child(0).unwrap();
/// let operand = statement.named_child(0).unwrap().named_child(0).unwrap();
/// let error = TypePath::builder().krate("anyhow").modules([] as [&str; 0]).name("Error").build();
/// let signature = FnSignature::builder()
///     .return_type(ResolvedType::new("Result<()>".into()).with_result_kind(ResultKind::Result))
///     .error_type(ErrorType::Named(error))
///     .return_mode(ReturnMode::Direct)
///     .build();
/// let mut decorations = DecorationMap::new();
/// decorations.insert(operand.id(), ResolvedType::new("Result<()>".into()).with_result_kind(ResultKind::Result));
/// decorations.insert(function.id(), signature);
/// decorate(&mut tree, decorations);
/// let mut passes = vec![AnyhowMissingContext::into_lint_pass()];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
///
/// [`Result`]: std::result::Result
pub struct AnyhowMissingContext;

impl AnyhowMissingContext {
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = AnyhowMissingContext::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(stiles_rust::RustLintPassAdapter::new(Self))
    }
}

impl RustLintPass for AnyhowMissingContext {
    fn check_try_expression(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(operand) = node.named_child(0) else {
            return Vec::new();
        };

        let Some(resolved) = operand.decoration::<ResolvedType>() else {
            return Vec::new();
        };

        if resolved.is_option() {
            return Vec::new();
        }

        if !resolved.is_result() {
            return Vec::new();
        }

        let Some(fn_sig) = find_enclosing_fn_signature(node) else {
            return Vec::new();
        };
        let Some(error_type) = fn_sig.error_type() else {
            return Vec::new();
        };
        if !error_type.is(ANYHOW_ERROR) {
            return Vec::new();
        }

        if is_context_call(&operand) {
            return Vec::new();
        }

        vec![
            Diagnostic::builder()
                .rule_id(RULE_ID)
                .severity(Severity::Error)
                .message("use of `?` on Result without error context")
                .span(node.span())
                .build(),
        ]
    }
}

/// Node kinds whose bodies own a `?`, so the ancestor walk stops at them
///
/// A closure, an `async` block, a `gen` block, a `const` block, and a
/// `try` block each have their own return type. A `?` inside one converts
/// into that type, not into the enclosing function's error type.
const BODY_BARRIERS: &[&str] = &[
    "async_block",
    "closure_expression",
    "const_block",
    "gen_block",
    "try_block",
];

/// Walks up the tree to find the enclosing `function_item` and returns
/// its [`FnSignature`] decoration, if present
///
/// Returns [`None`] when the walk meets a [`BODY_BARRIERS`] kind first.
/// That accepts a false negative: the rule does not flag a `?` inside a
/// closure that itself returns `anyhow::Result`. A missed diagnostic costs
/// less than a suggestion that does not compile.
fn find_enclosing_fn_signature<'a>(node: &DecoratedNode<'a>) -> Option<&'a FnSignature> {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if BODY_BARRIERS.contains(&ancestor.kind()) {
            return None;
        }
        if ancestor.kind() == "function_item" {
            return ancestor.decoration::<FnSignature>();
        }
        current = ancestor.parent();
    }
    None
}

/// Returns whether the operand is a `.context()` or `.with_context()` call
fn is_context_call(operand: &DecoratedNode<'_>) -> bool {
    if operand.kind() != "call_expression" {
        return false;
    }
    let Some(function) = operand.child_by_field_name("function") else {
        return false;
    };
    if function.kind() != "field_expression" {
        return false;
    }
    let Some(field) = function.child_by_field_name("field") else {
        return false;
    };
    let name = field.text();
    name == "context" || name == "with_context"
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for AnyhowMissingContext {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.anyhow-missing-context")]
    }
}

stiles_plugin::export_lints![rust: [AnyhowMissingContext]];

#[cfg(test)]
mod tests {
    use stiles_rust::decorations::{
        ErrorType, FnSignature, OptionKind, ResolvedType, ResultKind, ReturnMode, TypePath,
    };
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use stiles_types::{DecorationMap, Language, LintPass, Severity};

    use super::*;

    fn passes() -> Vec<Box<dyn LintPass>> {
        vec![AnyhowMissingContext::into_lint_pass()]
    }

    fn result_type() -> ResolvedType {
        ResolvedType::new("Result<(), Error>".into()).with_result_kind(ResultKind::Result)
    }

    fn option_type() -> ResolvedType {
        ResolvedType::new("Option<String>".into()).with_option_kind(OptionKind::Option)
    }

    /// Returns the signature the provider reports for `anyhow::Result<()>`
    fn anyhow_fn_sig() -> FnSignature {
        let ret =
            ResolvedType::new("Result<(), Error>".into()).with_result_kind(ResultKind::Result);
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

    /// Returns the signature the provider reports for `std::io::Result<()>`
    ///
    /// The rendering matches `anyhow_fn_sig`, so only the definition path
    /// tells the two apart. The path names `core`, because `std::io::Error`
    /// is a re-export of `core::io::error::Error`.
    fn non_anyhow_fn_sig() -> FnSignature {
        let ret =
            ResolvedType::new("Result<(), Error>".into()).with_result_kind(ResultKind::Result);
        let error = ErrorType::Named(
            TypePath::builder()
                .krate("core")
                .modules(["io", "error"])
                .name("Error")
                .build(),
        );
        FnSignature::builder()
            .return_type(ret)
            .error_type(error)
            .return_mode(ReturnMode::Direct)
            .build()
    }

    /// Walks the tree to find the first node with the given kind
    fn find_node_by_kind<'a>(node: &stiles_types::DecoratedNode<'a>, kind: &str) -> Option<usize> {
        if node.kind() == kind {
            return Some(node.id());
        }
        for child in node.named_children() {
            if let Some(id) = find_node_by_kind(&child, kind) {
                return Some(id);
            }
        }
        None
    }

    /// Walks the tree to find the first `try_expression` and returns the
    /// node ID of its operand (first named child)
    fn find_try_operand_id(tree: &stiles_types::DecoratedTree) -> usize {
        fn walk(node: &stiles_types::DecoratedNode<'_>) -> Option<usize> {
            if node.kind() == "try_expression" {
                return node.named_child(0).map(|c| c.id());
            }
            for child in node.named_children() {
                if let Some(id) = walk(&child) {
                    return Some(id);
                }
            }
            None
        }
        walk(&tree.root_node()).expect("should find try_expression operand")
    }

    #[test]
    fn in_anyhow_fn_without_context_is_flagged() {
        let source = "fn foo() -> Result<(), Error> { something()?; }";
        let mut tree = parse(source, Language::Rust);
        let operand_id = find_try_operand_id(&tree);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(operand_id, result_type());
        map.insert(fn_id, anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn in_anyhow_fn_without_context_is_flagged_with_the_full_diagnostic() {
        let source = "fn foo() -> Result<(), Error> { something()?; }";
        let mut tree = parse(source, Language::Rust);
        let operand_id = find_try_operand_id(&tree);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(operand_id, result_type());
        map.insert(fn_id, anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.anyhow-missing-context")
            .has_severity(Severity::Error)
            .message_contains("without error context");
    }

    /// A `?` inside an `async` block converts into that block's error type
    ///
    /// The block is its own body, so the enclosing `fn`'s signature says
    /// nothing about what the `?` there produces.
    #[test]
    fn in_async_block_inside_anyhow_fn_is_not_flagged() {
        let source = "fn foo() -> Result<(), Error> { async { something()?; } }";
        let mut tree = parse(source, Language::Rust);
        let operand_id = find_try_operand_id(&tree);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(operand_id, result_type());
        map.insert(fn_id, anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    /// A `?` inside a closure converts into the closure's error type
    ///
    /// The enclosing function's signature does not apply inside the
    /// closure's body.
    #[test]
    fn in_closure_inside_anyhow_fn_is_not_flagged() {
        let source = "fn foo() -> Result<(), Error> { let f = || { something()?; }; }";
        let mut tree = parse(source, Language::Rust);
        let operand_id = find_try_operand_id(&tree);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(operand_id, result_type());
        map.insert(fn_id, anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn in_non_anyhow_fn_is_not_flagged() {
        let source = "fn foo() -> Result<(), IoError> { something()?; }";
        let mut tree = parse(source, Language::Rust);
        let operand_id = find_try_operand_id(&tree);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(operand_id, result_type());
        map.insert(fn_id, non_anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn no_resolved_type_decoration_is_not_flagged() {
        let source = "fn foo() -> Result<(), Error> { something()?; }";
        let mut tree = parse(source, Language::Rust);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(fn_id, anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn on_option_is_not_flagged() {
        let source = "fn foo() -> Result<(), Error> { something()?; }";
        let mut tree = parse(source, Language::Rust);
        let operand_id = find_try_operand_id(&tree);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(operand_id, option_type());
        map.insert(fn_id, anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<AnyhowMissingContext>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<AnyhowMissingContext>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<AnyhowMissingContext>();
    }

    #[test]
    fn with_context_call_is_not_flagged() {
        let source = "fn foo() -> Result<(), Error> { something().with_context(|| \"msg\")?; }";
        let mut tree = parse(source, Language::Rust);
        let operand_id = find_try_operand_id(&tree);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(operand_id, result_type());
        map.insert(fn_id, anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn with_dot_context_call_is_not_flagged() {
        let source = "fn foo() -> Result<(), Error> { something().context(\"reading file\")?; }";
        let mut tree = parse(source, Language::Rust);
        let operand_id = find_try_operand_id(&tree);
        let fn_id = find_node_by_kind(&tree.root_node(), "function_item").expect("should find fn");

        let mut map = DecorationMap::new();
        map.insert(operand_id, result_type());
        map.insert(fn_id, anyhow_fn_sig());
        decorate(&mut tree, map);

        let diagnostics = execute(&tree, &mut passes());

        assert_no_diagnostics(&diagnostics);
    }
}
