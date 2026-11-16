#[cfg(feature = "plugin")]
use stiles_rust::ReadsTestCode as _;
use stiles_rust::decorations::{DefinitionKind, Resolution, Target, TargetKind};
use stiles_rust::{RuleId, RustLintPass, is_test_code};
use stiles_types::{DecoratedNode, Diagnostic, Severity};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(terminal_output::RULE_ID.as_str(), "lint.terminal-output");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.terminal-output");

/// The macros that write to standard output or standard error
const PRINT_MACROS: [&str; 4] = ["print", "println", "eprint", "eprintln"];

/// Flags code that writes to the terminal or ends the process itself
///
/// Code that produces a result does not decide how the result looks. A
/// `println!` in the code that found a diagnostic fixes its rendering, its
/// stream, and its timing in one line. Every other consumer of that result,
/// such as a JSON mode, a test, or an editor, then has to work around it.
///
/// The rule reports the `print!`, `println!`, `eprint!`, and `eprintln!`
/// macros. It reports a call to `std::io::stdout` or `std::io::stderr`,
/// because a function that takes the handle itself makes the same decision.
/// A renderer that writes to a handle its caller passes in is the form the
/// rule leaves alone. It also reports a call to `std::process::exit`. That
/// call ends the process instead of returning the result, and output behind
/// it in a buffer is lost.
///
/// A name counts when its `Resolution` decoration names the definition in
/// the standard library. Rust-analyzer resolves it, so a glob import, a
/// rename, and a re-export reach the definition. A local item, a binding,
/// or a `macro_rules!` with the same name resolves to itself instead. A
/// name without the decoration keeps the rule silent. The rule reads
/// `dbg!` as a debugging aid that Clippy already covers, so it leaves that
/// macro alone.
///
/// The rule skips the root of a binary, where the program meets the
/// terminal. The kinds of the `Target` decoration of that file are only
/// binaries, and the file declares `fn main`. The other modules of a binary
/// are checked. The rule skips a file whose kinds are only build scripts
/// and examples. A build script talks to cargo through standard output, and
/// an example is a program that shows its output to a person.
///
/// The rule skips the body of an `impl Termination`. Std calls `report` with
/// no argument when `main` returns the value, so no caller can pass a handle
/// in. Such an impl hands the handles of the process to a function that
/// takes them, and that function is where the rule looks.
///
/// The rule skips test code, which the Rust provider marks with the
/// `TestCode` decoration, such as a test function or a `cfg(test)` module.
/// A file is test code too when its kinds are only integration tests and
/// benches. A `cfg(test)` on a statement marks no item, so the rule checks
/// the statement. Code inside a macro argument or a macro body is invisible
/// to the rule. Tree-sitter does not parse it as code, and the provider
/// resolves no name in it.
///
/// # Examples
///
/// ```text
/// // DON'T
/// fn report(count: usize) {
///     println!("found {count} issues");
/// }
///
/// // DO
/// fn report(out: &mut impl Write, count: usize) -> io::Result<()> {
///     writeln!(out, "found {count} issues")
/// }
/// ```
///
/// The rule reads what each name resolves to, which the Rust provider
/// attaches as decorations. This example attaches it by hand.
///
/// ```
/// use stiles_rust::RustLintPassAdapter;
/// use stiles_rust::decorations::{DefinitionKind, DefinitionOrigin, Resolution, TypePath};
/// use stiles_testing::{execute, parse};
/// use stiles_types::{Language, LintPass};
/// use terminal_output::TerminalOutput;
///
/// let mut tree = parse("fn run() { println!(\"building\"); }", Language::Rust);
/// let function = tree.root_node().named_child(0).unwrap();
/// let statement = function.child_by_field_name("body").unwrap().named_child(0).unwrap();
/// let call = statement.named_child(0).unwrap();
/// let name = call.child_by_field_name("macro").unwrap().id();
/// let println = Resolution::builder()
///     .kind(DefinitionKind::Macro)
///     .origin(DefinitionOrigin::OtherCrate)
///     .krate("std")
///     .path(TypePath::builder().krate("std").modules(["macros"]).name("println").build())
///     .build();
/// tree.decorations_mut().insert(name, println);
/// let pass: Box<dyn LintPass> = Box::new(RustLintPassAdapter::new(TerminalOutput));
/// let mut passes = vec![pass];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct TerminalOutput;

impl RustLintPass for TerminalOutput {
    fn check_macro_invocation(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let resolution = node
            .child_by_field_name("macro")
            .and_then(|path| path.decoration::<Resolution>().and_then(std_item));
        let Some(name) = (match resolution {
            Some(StdItem::Macro { name }) => PRINT_MACROS
                .into_iter()
                .find(|candidate| *candidate == name),
            Some(StdItem::Function { .. }) | None => None,
        }) else {
            return Vec::new();
        };

        report(
            node,
            format!("`{name}!` writes to the terminal; {PRINT_ADVICE}"),
        )
    }

    fn check_call_expression(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let resolution = node
            .child_by_field_name("function")
            .and_then(|function| function.decoration::<Resolution>().and_then(std_item));
        let function = match resolution {
            Some(StdItem::Function { module, name }) => (module, name),
            Some(StdItem::Macro { .. }) | None => return Vec::new(),
        };

        let message = match function {
            ("process", "exit") => {
                "`std::process::exit` ends the process; return the result to `main` instead"
                    .to_owned()
            }
            ("io", handle @ ("stdout" | "stderr")) => {
                format!("`std::io::{handle}()` reaches for the terminal; {HANDLE_ADVICE}")
            }
            _ => return Vec::new(),
        };

        report(node, message)
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for TerminalOutput {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::of::<Resolution>().and_test_code()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [TerminalOutput]];

/// The fix a print macro's finding asks for
const PRINT_ADVICE: &str = "return the result, or write to a handle the caller passes in";

/// The fix a standard stream handle's finding asks for
const HANDLE_ADVICE: &str = "write to a handle the caller passes in instead";

/// A macro or a function of the standard library, which the rule may report
enum StdItem<'a> {
    /// A macro, by its name alone, because the module that defines it is private
    Macro { name: &'a str },
    /// A function, by its public module and its name
    ///
    /// The definition can sit in a private module below the public one, as
    /// `stdout` sits in `std::io::stdio`. The first module is the public one.
    Function { module: &'a str, name: &'a str },
}

/// Returns the macro or the function of `std` that `resolution` names, if it names one
fn std_item(resolution: &Resolution) -> Option<StdItem<'_>> {
    let path = resolution.path()?;
    if path.krate() != "std" {
        return None;
    }

    match resolution.kind() {
        DefinitionKind::Macro => Some(StdItem::Macro { name: path.name() }),
        DefinitionKind::Function => Some(StdItem::Function {
            module: path.modules().next()?,
            name: path.name(),
        }),
        DefinitionKind::Crate
        | DefinitionKind::Module
        | DefinitionKind::Struct
        | DefinitionKind::Enum
        | DefinitionKind::Union
        | DefinitionKind::EnumVariant
        | DefinitionKind::Trait
        | DefinitionKind::TypeAlias
        | DefinitionKind::AssociatedFunction
        | DefinitionKind::Method
        | DefinitionKind::Const
        | DefinitionKind::Static
        | DefinitionKind::LocalBinding
        | DefinitionKind::TypeParameter
        | DefinitionKind::ConstParameter
        | DefinitionKind::SelfType
        | DefinitionKind::BuiltinType
        | DefinitionKind::BuiltinAttribute
        | DefinitionKind::ToolModule
        | DefinitionKind::DeriveHelper => None,
    }
}

/// Returns the finding for `node`, unless its file or its scope is exempt
fn report(node: &DecoratedNode<'_>, message: String) -> Vec<Diagnostic> {
    if is_exempt_file(node) {
        return Vec::new();
    }
    if is_test_code(node) {
        return Vec::new();
    }
    if is_in_termination_impl(node) {
        return Vec::new();
    }

    vec![
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Error)
            .message(message)
            .span(node.span())
            .build(),
    ]
}

/// Returns whether `node` sits in an impl of the trait `Termination`
///
/// The trait counts by the last segment of its path, so `Termination` and
/// `std::process::Termination` both count.
fn is_in_termination_impl(node: &DecoratedNode<'_>) -> bool {
    let mut current = node.parent();
    while let Some(scope) = current {
        if scope.kind() == "impl_item" {
            return scope.child_by_field_name("trait").is_some_and(|name| {
                let path = name.text();
                path.rsplit("::").next().unwrap_or(path).trim() == "Termination"
            });
        }
        current = scope.parent();
    }
    false
}

/// Returns whether the rule skips every node of the file that holds `node`
///
/// The root of a binary meets the terminal, as [`program_root`] decides. A
/// file that only build scripts and examples compile talks to its reader
/// through the terminal too. The kinds come from the `Target` decoration of
/// the file, and a file without one is checked, because an exemption needs
/// proof.
fn is_exempt_file(node: &DecoratedNode<'_>) -> bool {
    if program_root::is_binary_root(node) {
        return true;
    }

    let mut file = *node;
    while let Some(parent) = file.parent() {
        file = parent;
    }
    file.decoration::<Target>().is_some_and(|target| {
        let kinds = target.kinds();
        !kinds.is_empty() && kinds.iter().all(talks_to_its_reader)
    })
}

/// Returns whether every file of a target of `kind` may write to the terminal
fn talks_to_its_reader(kind: TargetKind) -> bool {
    match kind {
        TargetKind::BuildScript | TargetKind::Example => true,
        TargetKind::Library
        | TargetKind::Binary
        | TargetKind::IntegrationTest
        | TargetKind::Bench => false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use stiles_rust::RustLintPassAdapter;
    use stiles_rust::decorations::{DefinitionOrigin, TypePath};
    use stiles_testing::{assert_diagnostic, execute};
    use stiles_types::{DecoratedTree, Language, LintPass};

    use super::*;

    /// The path the tests give a file the rule checks
    const LIBRARY: &str = "/work/project/crates/report/src/render.rs";

    /// Parses `source` as the file at `path`
    fn parse_at(path: &Path, source: &str) -> DecoratedTree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&stiles_rust::language())
            .expect("should load the Rust grammar");
        let tree = parser.parse(source, None).expect("should parse");
        DecoratedTree::builder()
            .tree(tree)
            .language(Language::Rust)
            .source(source.to_owned())
            .file(path.to_path_buf())
            .build()
    }

    /// Returns what the provider attaches to a name of the item that `path` spells
    ///
    /// The path spells the crate, the modules, and the name, as `std::process::exit`.
    fn defined(kind: DefinitionKind, path: &str) -> Resolution {
        let segments: Vec<&str> = path.split("::").collect();
        let [krate, modules @ .., name] = segments.as_slice() else {
            panic!("the path `{path}` should name a crate and an item");
        };
        let origin = match *krate {
            "sample" => DefinitionOrigin::SameCrate,
            _ => DefinitionOrigin::OtherCrate,
        };
        Resolution::builder()
            .kind(kind)
            .origin(origin)
            .krate(krate)
            .path(
                TypePath::builder()
                    .krate(krate)
                    .modules(modules.iter().copied())
                    .name(name)
                    .build(),
            )
            .build()
    }

    /// Returns what the provider attaches to a name of the standard macro `name`
    fn std_macro(name: &str) -> Resolution {
        defined(DefinitionKind::Macro, &format!("std::macros::{name}"))
    }

    /// Returns what the provider attaches to a name of `std::process::exit`
    fn std_exit() -> Resolution {
        defined(DefinitionKind::Function, "std::process::exit")
    }

    /// Returns what the provider attaches to a name of the standard stream function `name`
    fn std_stream(name: &str) -> Resolution {
        defined(DefinitionKind::Function, &format!("std::io::stdio::{name}"))
    }

    /// Returns what the provider attaches to a local binding
    fn local_binding() -> Resolution {
        Resolution::builder()
            .kind(DefinitionKind::LocalBinding)
            .origin(DefinitionOrigin::SameCrate)
            .krate("sample")
            .build()
    }

    /// Returns the spellings of standard names that every test resolves
    fn standard_names() -> Vec<(&'static str, Resolution)> {
        let mut names: Vec<(&str, Resolution)> = PRINT_MACROS
            .into_iter()
            .map(|name| (name, std_macro(name)))
            .collect();
        names.extend([
            ("std::println", std_macro("println")),
            ("::std::eprintln", std_macro("eprintln")),
            ("dbg", std_macro("dbg")),
            ("vec", defined(DefinitionKind::Macro, "alloc::macros::vec")),
            ("std::process::exit", std_exit()),
            ("std::io::stdout", std_stream("stdout")),
            ("std::io::stderr", std_stream("stderr")),
            ("std::io::stdin", std_stream("stdin")),
        ]);
        names
    }

    /// Attaches each resolution to every name in `tree` that `names` spells
    ///
    /// The provider resolves no token inside a macro call, so the walk
    /// skips token trees.
    fn resolve(tree: &mut DecoratedTree, names: &[(&str, Resolution)]) {
        let mut found = Vec::new();
        collect_names(&tree.root_node(), names, &mut found);
        for (id, resolution) in found {
            tree.decorations_mut().insert(id, resolution);
        }
    }

    /// Collects the id and the resolution of each name below `node` that `names` spells
    fn collect_names(
        node: &DecoratedNode<'_>,
        names: &[(&str, Resolution)],
        found: &mut Vec<(usize, Resolution)>,
    ) {
        if node.kind() == "token_tree" {
            return;
        }
        if ["identifier", "scoped_identifier"].contains(&node.kind())
            && let Some((_, resolution)) = names
                .iter()
                .rev()
                .find(|(spelling, _)| *spelling == node.text())
        {
            found.push((node.id(), resolution.clone()));
        }
        for child in node.named_children() {
            collect_names(&child, names, found);
        }
    }

    /// Returns the messages the rule reports for `source`, after `attach`
    /// adds what the provider knows of test code and targets
    fn messages_attaching(source: &str, attach: impl FnOnce(&mut DecoratedTree)) -> Vec<String> {
        messages_with(source, &[], attach)
    }

    /// Returns the messages the rule reports for `source`, with `names`
    /// resolved on top of the standard names and `attach` applied
    ///
    /// A spelling in `names` replaces the standard resolution of that spelling.
    fn messages_with(
        source: &str,
        names: &[(&str, Resolution)],
        attach: impl FnOnce(&mut DecoratedTree),
    ) -> Vec<String> {
        let mut tree = parse_at(Path::new(LIBRARY), source);
        let mut all = standard_names();
        all.extend(names.iter().cloned());
        resolve(&mut tree, &all);
        attach(&mut tree);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(TerminalOutput))];
        execute(&tree, &mut passes)
            .iter()
            .map(|diagnostic| diagnostic.message().to_owned())
            .collect()
    }

    /// Returns the messages the rule reports for `source`, which resolves
    /// only the standard names
    fn messages(source: &str) -> Vec<String> {
        messages_attaching(source, |_| {})
    }

    /// Returns the messages the rule reports for `source`, with `names`
    /// resolved on top of the standard names
    fn messages_resolving(source: &str, names: &[(&str, Resolution)]) -> Vec<String> {
        messages_with(source, names, |_| {})
    }

    /// Returns the messages the rule reports for `source` in a file that
    /// only Cargo targets of `kinds` compile
    fn messages_in_target(
        source: &str,
        kinds: impl IntoIterator<Item = TargetKind>,
    ) -> Vec<String> {
        messages_attaching(source, |tree| test_decorations::target(tree, kinds))
    }

    /// Returns whether the rule skips the file `source` whose targets are `kinds`
    fn exempt(source: &str, kinds: impl IntoIterator<Item = TargetKind>) -> bool {
        let mut tree = parse_at(Path::new(LIBRARY), source);
        test_decorations::target(&mut tree, kinds);
        is_exempt_file(&tree.root_node())
    }

    /// Returns the finding text for a print macro
    fn print_message(name: &str) -> String {
        format!("`{name}!` writes to the terminal; {PRINT_ADVICE}")
    }

    /// Returns the finding text for a standard stream handle
    fn handle_message(handle: &str) -> String {
        format!("`std::io::{handle}()` reaches for the terminal; {HANDLE_ADVICE}")
    }

    /// The finding text for `std::process::exit`
    const EXIT_MESSAGE: &str =
        "`std::process::exit` ends the process; return the result to `main` instead";

    #[test]
    fn absolute_std_print_is_flagged() {
        let messages = messages("fn f() { ::std::eprintln!(\"x\"); }");

        assert_eq!(messages, vec![print_message("eprintln")]);
    }

    #[test]
    fn aliased_print_import_is_flagged() {
        let messages = messages_resolving(
            "use std::println as say;\nfn f() { say!(\"x\"); }",
            &[("say", std_macro("println"))],
        );

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn bare_exit_after_glob_import_is_flagged() {
        let messages = messages_resolving(
            "use std::process::*;\nfn f() { exit(1); }",
            &[("exit", std_exit())],
        );

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_after_renamed_import_is_flagged() {
        let messages = messages_resolving(
            "use std::process::exit as quit;\nfn f() { quit(2); }",
            &[("quit", std_exit())],
        );

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bench_function_is_not_flagged() {
        let source = "#[bench]\nfn b(bencher: &mut Bencher) { println!(\"x\"); }";

        let messages = messages_attaching(source, |tree| test_decorations::mark(tree, "fn b"));

        assert!(messages.is_empty());
    }

    #[test]
    fn binary_root_is_not_flagged() {
        let source = "fn main() { eprintln!(\"x\"); std::process::exit(2); }";

        let messages = messages_in_target(source, [TargetKind::Binary]);

        assert!(messages.is_empty());
    }

    #[test]
    fn build_module_of_a_library_is_flagged() {
        let source = "pub fn run() { println!(\"building\"); }";

        let messages = messages_in_target(source, [TargetKind::Library]);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn build_script_is_not_flagged() {
        let source = "fn main() { println!(\"cargo::rerun-if-changed=build.rs\"); }";

        let messages = messages_in_target(source, [TargetKind::BuildScript]);

        assert!(messages.is_empty());
    }

    #[test]
    fn cfg_all_with_test_is_not_flagged() {
        let source = "#[cfg(all(test, unix))]\nfn f() { println!(\"x\"); }";

        let messages = messages_attaching(source, |tree| test_decorations::mark(tree, "fn f"));

        assert!(messages.is_empty());
    }

    #[test]
    fn cfg_any_with_other_predicate_is_flagged() {
        let source = "#[cfg(any(test, feature = \"debug\"))]\nfn f() { println!(\"x\"); }";

        let messages = messages(source);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn cfg_attr_test_is_flagged() {
        let source = "#[cfg_attr(test, derive(Debug))]\nfn f() { println!(\"x\"); }";

        let messages = messages(source);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn cfg_feature_named_test_is_flagged() {
        let source = "#[cfg(feature = \"test\")]\nfn f() { println!(\"x\"); }";

        let messages = messages(source);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn cfg_not_test_is_flagged() {
        let source = "#[cfg(not(test))]\nfn f() { println!(\"x\"); }";

        let messages = messages(source);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn cfg_test_module_is_not_flagged() {
        let source = "#[cfg(test)]\nmod tests {\n    fn helper() { println!(\"x\"); }\n}";

        let messages = messages_attaching(source, |tree| test_decorations::mark(tree, "mod tests"));

        assert!(messages.is_empty());
    }

    #[test]
    fn cfg_test_on_statement_is_flagged() {
        let source = "fn f() {\n    #[cfg(test)]\n    println!(\"x\");\n    g();\n}";

        let messages = messages(source);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn cfg_test_with_comment_between_is_not_flagged() {
        let source = "#[cfg(test)]\n// Fixtures\nmod tests { fn f() { println!(\"x\"); } }";

        let messages = messages_attaching(source, |tree| test_decorations::mark(tree, "mod tests"));

        assert!(messages.is_empty());
    }

    #[test]
    fn dbg_is_not_flagged() {
        let messages = messages("fn f(x: u8) -> u8 { dbg!(x) }");

        assert!(messages.is_empty());
    }

    #[test]
    fn diagnostic_has_rule_id_severity_and_span() {
        let source = "fn f() { println!(\"x\"); }";
        let mut tree = parse_at(Path::new(LIBRARY), source);
        resolve(&mut tree, &standard_names());
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(TerminalOutput))];

        let diagnostics = execute(&tree, &mut passes);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.terminal-output")
            .has_severity(Severity::Error)
            .has_span(LIBRARY, 9..22);
    }

    #[test]
    fn diagnostic_is_reported_once() {
        let source = "fn f() { println!(\"x\"); }";
        let mut tree = parse_at(Path::new(LIBRARY), source);
        resolve(&mut tree, &standard_names());
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(TerminalOutput))];

        let diagnostics = execute(&tree, &mut passes);

        assert_eq!(diagnostics.len(), 1);
    }

    fn run_documented(source: &str) -> Vec<stiles_types::Diagnostic> {
        let mut tree = stiles_testing::parse(source, stiles_types::Language::Rust);
        resolve(&mut tree, &standard_names());
        stiles_testing::execute(
            &tree,
            &mut [Box::new(stiles_rust::RustLintPassAdapter::new(
                TerminalOutput,
            ))],
        )
    }

    #[test]
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        pair.assert_example(run_documented);
    }

    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_documented(pair.fix());

        stiles_testing::assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn example_file_is_not_flagged() {
        let source = "fn main() { run(); }\nfn run() { println!(\"x\"); }";

        let messages = messages_in_target(source, [TargetKind::Example]);

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_after_module_import_is_flagged() {
        let messages = messages_resolving(
            "use std::process;\nfn f() { process::exit(1); }",
            &[("process::exit", std_exit())],
        );

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_from_other_crate_is_not_flagged() {
        let messages = messages_resolving(
            "use nix::process;\nfn f() { process::exit(1); }",
            &[(
                "process::exit",
                defined(DefinitionKind::Function, "nix::unistd::exit"),
            )],
        );

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_in_cfg_test_impl_is_not_flagged() {
        let source = "#[cfg(test)]\nimpl App { fn quit(&self) { std::process::exit(0); } }";

        let messages = messages_attaching(source, |tree| test_decorations::mark(tree, "impl App"));

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_in_method_body_is_flagged() {
        let source = "use std::process;\nimpl App { fn quit(&self) { process::exit(0); } }";

        let messages = messages_resolving(source, &[("process::exit", std_exit())]);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_on_value_is_not_flagged() {
        let messages = messages("fn f(app: App) { app.exit(1); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_through_reexport_is_flagged() {
        let source = "use facade::quit;\nfn f() { quit(1); }";

        let messages = messages_resolving(source, &[("quit", std_exit())]);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_without_resolution_is_not_flagged() {
        let messages = messages("use std::process::exit;\nfn f() { exit(1); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn file_that_a_library_shares_with_a_test_is_flagged() {
        let source = "fn f() { println!(\"x\"); }";

        let messages =
            messages_in_target(source, [TargetKind::IntegrationTest, TargetKind::Library]);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn full_exit_path_is_flagged() {
        let messages = messages("fn f() { std::process::exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn inner_cfg_test_on_file_is_not_flagged() {
        let source = "#![cfg(test)]\n\nfn helper() { println!(\"x\"); }";

        let messages = messages_attaching(source, test_decorations::enclose_file);

        assert!(messages.is_empty());
    }

    #[test]
    fn inner_cfg_test_on_module_is_not_flagged() {
        let source = "mod fixtures {\n    #![cfg(test)]\n    fn f() { println!(\"x\"); }\n}";

        let messages =
            messages_attaching(source, |tree| test_decorations::mark(tree, "mod fixtures"));

        assert!(messages.is_empty());
    }

    #[test]
    fn integration_test_file_is_not_flagged() {
        let source = "fn helper() { println!(\"x\"); }";

        let messages = messages_in_target(source, [TargetKind::IntegrationTest]);

        assert!(messages.is_empty());
    }

    #[test]
    fn is_exempt_file_with_a_binary_module_returns_false() {
        let exempt = exempt("pub fn render() {}", [TargetKind::Binary]);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_a_binary_root_returns_true() {
        let exempt = exempt("fn main() {}", [TargetKind::Binary]);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_a_build_script_returns_true() {
        let exempt = exempt("fn main() {}", [TargetKind::BuildScript]);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_a_library_returns_false() {
        let exempt = exempt("pub fn render() {}", [TargetKind::Library]);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_an_example_a_library_shares_returns_false() {
        let exempt = exempt(
            "pub fn render() {}",
            [TargetKind::Example, TargetKind::Library],
        );

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_an_example_module_returns_true() {
        let exempt = exempt("pub fn render() {}", [TargetKind::Example]);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_an_integration_test_returns_false() {
        let exempt = exempt("fn helper() {}", [TargetKind::IntegrationTest]);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_no_kinds_returns_false() {
        let exempt = exempt("fn main() {}", []);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_without_a_target_returns_false() {
        let tree = parse_at(Path::new(LIBRARY), "fn main() {}");

        let exempt = is_exempt_file(&tree.root_node());

        assert!(!exempt);
    }

    #[test]
    fn let_binding_named_exit_is_not_flagged() {
        let source = "use std::process::exit;\nfn f() { let exit = |code| code; exit(1); }";

        let messages = messages_resolving(source, &[("exit", local_binding())]);

        assert!(messages.is_empty());
    }

    #[test]
    fn local_function_named_exit_is_not_flagged() {
        let messages = messages_resolving(
            "fn exit(code: i32) {}\nfn f() { exit(1); }",
            &[("exit", defined(DefinitionKind::Function, "sample::exit"))],
        );

        assert!(messages.is_empty());
    }

    #[test]
    fn local_function_named_println_is_flagged() {
        let messages = messages("fn println() {}\nfn f() { println!(\"x\"); }");

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn local_macro_named_println_is_not_flagged() {
        let source = "macro_rules! println { ($($t:tt)*) => {}; }\nfn f() { println!(\"x\"); }";
        let local = defined(DefinitionKind::Macro, "sample::println");

        let messages = messages_resolving(source, &[("println", local)]);

        assert!(messages.is_empty());
    }

    #[test]
    fn local_module_named_process_is_not_flagged() {
        let source = "mod process { pub fn exit(_: i32) {} }\nfn f() { process::exit(1); }";
        let local = defined(DefinitionKind::Function, "sample::process::exit");

        let messages = messages_resolving(source, &[("process::exit", local)]);

        assert!(messages.is_empty());
    }

    #[test]
    fn main_function_of_a_binary_module_is_flagged() {
        let source = "mod cli {\n    fn main() { println!(\"x\"); }\n}";

        let messages = messages_in_target(source, [TargetKind::Binary]);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn nested_print_macros_are_each_flagged() {
        let source = "fn f() { if a { print!(\"a\"); } else { eprint!(\"b\"); } }";

        let messages = messages(source);

        assert_eq!(
            messages,
            vec![print_message("print"), print_message("eprint")]
        );
    }

    #[test]
    fn parameter_named_exit_is_not_flagged() {
        let source = "use std::process::exit;\nfn f(exit: impl Fn(i32)) { exit(1); }";

        let messages = messages_resolving(source, &[("exit", local_binding())]);

        assert!(messages.is_empty());
    }

    #[test]
    fn print_from_other_crate_is_not_flagged() {
        let messages = messages_resolving(
            "use defmt::println;\nfn f() { println!(\"x\"); }",
            &[("println", defined(DefinitionKind::Macro, "defmt::println"))],
        );

        assert!(messages.is_empty());
    }

    #[test]
    fn print_in_closure_in_library_is_flagged() {
        let messages = messages("fn f() { items.iter().for_each(|i| println!(\"{i}\")); }");

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn print_in_macro_argument_is_not_flagged() {
        let messages = messages("fn f() { let v = vec![println!(\"x\")]; }");

        assert!(messages.is_empty());
    }

    #[test]
    fn print_in_macro_body_is_not_flagged() {
        let source = "macro_rules! say { ($e:expr) => { println!(\"{}\", $e) }; }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn print_in_main_function_of_library_file_is_flagged() {
        let messages = messages_in_target("fn main() { println!(\"x\"); }", [TargetKind::Library]);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn print_macros_are_flagged() {
        let source = "fn f() { print!(\"a\"); println!(); eprint!(\"b\"); eprintln!(); }";

        let messages = messages(source);

        assert_eq!(
            messages,
            vec![
                print_message("print"),
                print_message("println"),
                print_message("eprint"),
                print_message("eprintln"),
            ]
        );
    }

    #[test]
    fn print_named_by_a_function_is_not_flagged() {
        let source = "fn f() { println!(\"x\"); }";
        let function = defined(DefinitionKind::Function, "std::println");

        let messages = messages_resolving(source, &[("println", function)]);

        assert!(messages.is_empty());
    }

    #[test]
    fn print_through_reexport_is_flagged() {
        let source = "fn f() { facade::say!(\"x\"); }";

        let messages = messages_resolving(source, &[("facade::say", std_macro("println"))]);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn print_without_resolution_is_not_flagged() {
        let source = "fn f() { say!(\"x\"); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn qualified_print_is_flagged() {
        let messages = messages("fn f() { std::println!(\"x\"); }");

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[cfg(feature = "plugin")]
    #[test]
    fn reads_returns_the_resolution_the_target_and_the_test_code() {
        let reads = stiles_plugin::DeclaresRules::reads(&TerminalOutput);

        assert_eq!(
            reads,
            stiles_plugin::Reads::of::<Resolution>()
                .and::<Target>()
                .and::<stiles_rust::decorations::TestCode>()
        );
    }

    #[test]
    fn std_function_outside_its_module_is_not_flagged() {
        let source = "fn f() { sys::exit(1); }";
        let private = defined(DefinitionKind::Function, "std::sys::exit::exit");

        let messages = messages_resolving(source, &[("sys::exit", private)]);

        assert!(messages.is_empty());
    }

    #[test]
    fn stderr_after_import_is_flagged() {
        let messages = messages_resolving(
            "use std::io::stderr;\nfn f() { let e = stderr(); }",
            &[("stderr", std_stream("stderr"))],
        );

        assert_eq!(messages, vec![handle_message("stderr")]);
    }

    #[test]
    fn stdin_is_not_flagged() {
        let messages = messages("fn f() { let input = std::io::stdin(); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn stdout_after_glob_import_is_flagged() {
        let source = "use std::io::*;\nfn f() { let out = stdout(); }";

        let messages = messages_resolving(source, &[("stdout", std_stream("stdout"))]);

        assert_eq!(messages, vec![handle_message("stdout")]);
    }

    #[test]
    fn stdout_after_module_import_is_flagged() {
        let source = "use std::io::{self, Write};\nfn f() { io::stdout().flush(); }";

        let messages = messages_resolving(source, &[("io::stdout", std_stream("stdout"))]);

        assert_eq!(messages, vec![handle_message("stdout")]);
    }

    #[test]
    fn stdout_full_path_is_flagged() {
        let messages = messages("fn f() { let out = std::io::stdout().lock(); }");

        assert_eq!(messages, vec![handle_message("stdout")]);
    }

    #[test]
    fn stdout_in_other_trait_impl_is_flagged() {
        let source =
            "impl Render for Exit { fn render(self) { report_to(&mut std::io::stdout()); } }";

        let messages = messages(source);

        assert_eq!(messages, vec![handle_message("stdout")]);
    }

    #[test]
    fn stdout_in_termination_impl_is_not_flagged() {
        let source = "impl std::process::Termination for Exit {\n    fn report(self) -> ExitCode { report_to(&mut std::io::stdout()) }\n}";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn stdout_passed_without_call_is_not_flagged() {
        let messages = messages("fn f() { render(std::io::stdout); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn stdout_through_reexport_is_flagged() {
        let source = "fn f() { let out = facade::out(); }";

        let messages = messages_resolving(source, &[("facade::out", std_stream("stdout"))]);

        assert_eq!(messages, vec![handle_message("stdout")]);
    }

    #[test]
    fn test_attribute_after_doc_comment_is_not_flagged() {
        let source = "/// Checks the output\n#[test]\nfn t() { println!(\"x\"); }";

        let messages = messages_attaching(source, |tree| test_decorations::mark(tree, "fn t"));

        assert!(messages.is_empty());
    }

    #[test]
    fn test_attribute_with_path_and_arguments_is_not_flagged() {
        let source = "#[tokio::test(flavor = \"multi_thread\")]\nasync fn t() { println!(\"x\"); }";

        let messages =
            messages_attaching(source, |tree| test_decorations::mark(tree, "async fn t"));

        assert!(messages.is_empty());
    }

    #[test]
    fn test_function_is_not_flagged() {
        let source =
            "#[test]\n#[should_panic]\nfn t() { eprintln!(\"x\"); std::process::exit(1); }";

        let messages = messages_attaching(source, |tree| test_decorations::mark(tree, "fn t"));

        assert!(messages.is_empty());
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<TerminalOutput>();
    }

    #[test]
    fn trait_send_std_item() {
        fn assert_send<T: Send>() {}
        assert_send::<StdItem<'static>>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<TerminalOutput>();
    }

    #[test]
    fn trait_sync_std_item() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<StdItem<'static>>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<TerminalOutput>();
    }

    #[test]
    fn trait_unpin_std_item() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<StdItem<'static>>();
    }

    #[test]
    fn writer_parameter_is_not_flagged() {
        let source =
            "fn render(out: &mut impl Write, n: u8) -> io::Result<()> { out.write_all(b\"x\") }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }
}
