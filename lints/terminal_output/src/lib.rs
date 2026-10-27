mod resolve;
mod test_code;

use std::path::Path;

use whisker_rust::{RuleId, RustLintPass};
use whisker_types::{DecoratedNode, Diagnostic, Severity};

use self::resolve::Namespace;

const RULE_ID: RuleId = RuleId::new("lint.terminal-output");

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
/// A path counts when it resolves to the standard library through the `use`
/// declarations of the file. A local item, binding, or `macro_rules!` with
/// the same name silences the rule, and so does an import it cannot follow.
/// The rule reads `dbg!` as a debugging aid that Clippy already covers, so
/// it leaves that macro alone.
///
/// The rule skips the root of a program, where the program meets the
/// terminal. That is a file named `main.rs` and a file directly in
/// `src/bin`. It skips a build script, which talks to cargo through
/// standard output. That is a file named `build.rs` outside `src`, so a
/// module named `build` is still checked. It skips test code, which is an item with a test
/// attribute or a `cfg(test)` scope, and the files under `tests`, `benches`,
/// and `examples`. An example is a program that shows its output to a
/// person, like `main.rs`. Code inside a macro argument or a macro body is
/// invisible to the rule, because tree-sitter does not parse it as code.
pub struct TerminalOutput;

impl RustLintPass for TerminalOutput {
    fn check_macro_invocation(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(path) = node.child_by_field_name("macro") else {
            return Vec::new();
        };
        let Some(segments) = self::resolve::resolve(node, path.text(), Namespace::Macro) else {
            return Vec::new();
        };
        let Some(name) = print_macro(&segments) else {
            return Vec::new();
        };

        report(
            node,
            format!("`{name}!` writes to the terminal; {PRINT_ADVICE}"),
        )
    }

    fn check_call_expression(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(function) = node.child_by_field_name("function") else {
            return Vec::new();
        };
        let namespace = match function.kind() {
            "identifier" => Namespace::Value,
            "scoped_identifier" => Namespace::Type,
            _ => return Vec::new(),
        };
        let Some(segments) = self::resolve::resolve(node, function.text(), namespace) else {
            return Vec::new();
        };

        let message = match segments_as_str(&segments).as_slice() {
            ["std", "process", "exit"] => {
                "`std::process::exit` ends the process; return the result to `main` instead"
                    .to_owned()
            }
            ["std", "io", handle @ ("stdout" | "stderr")] => {
                format!("`std::io::{handle}()` reaches for the terminal; {HANDLE_ADVICE}")
            }
            _ => return Vec::new(),
        };

        report(node, message)
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for TerminalOutput {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

whisker_plugin::export_lints![rust: [TerminalOutput]];

/// The fix a print macro's finding asks for
const PRINT_ADVICE: &str = "return the result, or write to a handle the caller passes in";

/// The fix a standard stream handle's finding asks for
const HANDLE_ADVICE: &str = "write to a handle the caller passes in instead";

/// Returns the name of the print macro a resolved path names, if it names one
fn print_macro(segments: &[String]) -> Option<&'static str> {
    let [krate, name] = segments_as_str(segments)[..] else {
        return None;
    };
    if krate != "std" {
        return None;
    }
    PRINT_MACROS
        .into_iter()
        .find(|candidate| *candidate == name)
}

/// Borrows each segment of a resolved path, so a slice pattern can match it
fn segments_as_str(segments: &[String]) -> Vec<&str> {
    segments.iter().map(String::as_str).collect()
}

/// Returns the finding for `node`, unless its file or its scope is exempt
fn report(node: &DecoratedNode<'_>, message: String) -> Vec<Diagnostic> {
    if is_exempt_file(node.span().file()) {
        return Vec::new();
    }
    if self::test_code::is_test_code(node) {
        return Vec::new();
    }

    vec![
        Diagnostic::builder()
            .rule_id(RULE_ID)
            .severity(Severity::Warn)
            .message(message)
            .span(node.span())
            .build(),
    ]
}

/// Returns whether the rule skips every node of the file at `path`
///
/// The nearest enclosing directory named `src`, `tests`, `benches`, or
/// `examples` places the file in its package. Whisker can hand a rule an
/// absolute path, so a `tests` directory above the project must not count.
/// The nearest of the four names is the one inside the package.
///
/// A file named `main.rs` is a program's root wherever it sits, and so is a
/// file directly in `src/bin`. A file named `build.rs` is a build script
/// only outside `src`, because a module under `src` can carry that name.
/// Cargo builds `tests`, `benches`, and `examples` only for test,
/// benchmark, and example targets.
fn is_exempt_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name == "main.rs" {
        return true;
    }
    let Some(parent) = path.parent() else {
        return name == "build.rs";
    };
    if is_binary_directory(parent) {
        return true;
    }

    let nearest = parent
        .components()
        .rev()
        .filter_map(|component| component.as_os_str().to_str())
        .find(|directory| is_cargo_directory(directory));

    match nearest {
        Some("tests" | "benches" | "examples") => true,
        Some(_) => false,
        None => name == "build.rs",
    }
}

/// Returns whether a directory is a package's `src/bin`
fn is_binary_directory(directory: &Path) -> bool {
    let mut names = directory
        .components()
        .rev()
        .map(|component| component.as_os_str().to_str());
    names.next() == Some(Some("bin")) && names.next() == Some(Some("src"))
}

/// Returns whether a directory name is one that Cargo gives a meaning
fn is_cargo_directory(directory: &str) -> bool {
    match directory {
        "src" | "tests" | "benches" | "examples" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use whisker_rust::RustLintPassAdapter;
    use whisker_testing::{assert_diagnostic, execute};
    use whisker_types::{DecoratedTree, Language, LintPass};

    use super::*;

    /// The path the tests give a file the rule checks
    const LIBRARY: &str = "/work/project/crates/report/src/render.rs";

    /// Parses `source` as the file at `path`
    fn parse_at(path: &Path, source: &str) -> DecoratedTree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&whisker_rust::language())
            .expect("should load the Rust grammar");
        let tree = parser.parse(source, None).expect("should parse");
        DecoratedTree::builder()
            .tree(tree)
            .language(Language::Rust)
            .source(source.to_owned())
            .file(path.to_path_buf())
            .build()
    }

    /// Returns the messages the rule reports for `source` at `path`
    fn messages_at(path: &Path, source: &str) -> Vec<String> {
        let tree = parse_at(path, source);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(TerminalOutput))];
        execute(&tree, &mut passes)
            .iter()
            .map(|diagnostic| diagnostic.message().to_owned())
            .collect()
    }

    /// Returns the messages the rule reports for `source` in a library file
    fn messages(source: &str) -> Vec<String> {
        messages_at(Path::new(LIBRARY), source)
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
        let messages = messages("use std::println as say;\nfn f() { say!(\"x\"); }");

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn bare_exit_after_glob_import_is_flagged() {
        let messages = messages("use std::process::*;\nfn f() { exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_after_glob_in_list_is_flagged() {
        let messages = messages("use std::process::{*};\nfn f() { exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_after_import_in_list_is_flagged() {
        let messages = messages("use std::{io, process::{self, exit}};\nfn f() { exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_after_import_is_flagged() {
        let messages = messages("use std::process::exit;\nfn f() { exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_after_renamed_import_is_flagged() {
        let messages = messages("use std::process::exit as quit;\nfn f() { quit(2); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_after_two_glob_imports_is_not_flagged() {
        let source = "use std::process::*;\nuse std::io::*;\nfn f() { exit(1); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn bare_exit_imported_in_enclosing_block_is_flagged() {
        let source = "fn f() { use std::process::exit; if done { exit(0); } }";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_imported_in_parent_module_is_not_flagged() {
        let source = "use std::process::exit;\nmod inner { fn f() { exit(1); } }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn bare_exit_imported_in_same_inline_module_is_flagged() {
        let source = "mod inner { use std::process::exit; fn f() { exit(1); } }";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_imported_outside_nested_function_is_flagged() {
        let source = "use std::process::exit;\nfn f() { fn g() { exit(1); } }";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn bare_exit_without_import_is_not_flagged() {
        let messages = messages("fn f() { exit(1); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn bench_function_is_not_flagged() {
        let messages = messages("#[bench]\nfn b(bencher: &mut Bencher) { println!(\"x\"); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn binary_root_is_not_flagged() {
        let source = "fn main() { eprintln!(\"x\"); std::process::exit(2); }";

        let messages = messages_at(Path::new("/work/project/src/bin/tool.rs"), source);

        assert!(messages.is_empty());
    }

    #[test]
    fn build_module_in_src_is_flagged() {
        let source = "pub fn run() { println!(\"building\"); }";

        let messages = messages_at(Path::new("/work/project/src/commands/build.rs"), source);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn build_script_is_not_flagged() {
        let source = "fn main() { println!(\"cargo::rerun-if-changed=build.rs\"); }";

        let messages = messages_at(Path::new("/work/project/crates/report/build.rs"), source);

        assert!(messages.is_empty());
    }

    #[test]
    fn cfg_all_with_test_is_not_flagged() {
        let source = "#[cfg(all(test, unix))]\nfn f() { println!(\"x\"); }";

        let messages = messages(source);

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

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn cfg_test_on_statement_is_not_flagged() {
        let source = "fn f() {\n    #[cfg(test)]\n    println!(\"x\");\n    g();\n}";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn cfg_test_with_comment_between_is_not_flagged() {
        let source = "#[cfg(test)]\n// Fixtures\nmod tests { fn f() { println!(\"x\"); } }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn closure_parameter_named_exit_is_not_flagged() {
        let source = "use std::process::exit;\nfn f() { run(|exit: fn(i32)| exit(1)); }";

        let messages = messages(source);

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
        let tree = parse_at(Path::new(LIBRARY), source);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(TerminalOutput))];

        let diagnostics = execute(&tree, &mut passes);

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.terminal-output")
            .has_severity(Severity::Warn)
            .has_span(LIBRARY, 9, 22);
    }

    #[test]
    fn diagnostic_is_reported_once() {
        let source = "fn f() { println!(\"x\"); }";
        let tree = parse_at(Path::new(LIBRARY), source);
        let mut passes: Vec<Box<dyn LintPass>> =
            vec![Box::new(RustLintPassAdapter::new(TerminalOutput))];

        let diagnostics = execute(&tree, &mut passes);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn example_file_is_not_flagged() {
        let source = "fn main() { run(); }\nfn run() { println!(\"x\"); }";

        let messages = messages_at(Path::new("/work/project/examples/demo.rs"), source);

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_after_absolute_import_is_flagged() {
        let messages = messages("use ::std::process;\nfn f() { process::exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_after_import_in_bare_list_is_flagged() {
        let messages = messages("use {std::process::exit};\nfn f() { exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_after_import_in_rooted_list_is_flagged() {
        let messages = messages("use ::{std::process};\nfn f() { process::exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_after_module_import_is_flagged() {
        let messages = messages("use std::process;\nfn f() { process::exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_after_renamed_module_import_is_flagged() {
        let source = "use std::process::{self as proc};\nfn f() { proc::exit(1); }";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_after_std_alias_is_flagged() {
        let messages = messages("use std as standard;\nfn f() { standard::process::exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_from_crate_module_is_not_flagged() {
        let messages = messages("use crate::process;\nfn f() { process::exit(1); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_from_other_crate_is_not_flagged() {
        let messages = messages("use nix::process;\nfn f() { process::exit(1); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_in_cfg_test_impl_is_not_flagged() {
        let source = "#[cfg(test)]\nimpl App { fn quit(&self) { std::process::exit(0); } }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_in_method_body_is_flagged() {
        let source = "use std::process;\nimpl App { fn quit(&self) { process::exit(0); } }";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_on_value_is_not_flagged() {
        let messages = messages("fn f(app: App) { app.exit(1); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_through_chained_import_is_flagged() {
        let source = "use std::process;\nuse process::exit;\nfn f() { exit(1); }";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn exit_with_cyclic_imports_is_not_flagged() {
        let source = "use a::b;\nuse b::a;\nfn f() { a::exit(1); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn exit_without_import_is_not_flagged() {
        let messages = messages("fn f() { process::exit(1); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn file_in_tests_directory_is_not_flagged() {
        let source = "fn helper() { println!(\"x\"); }";

        let messages = messages_at(
            Path::new("/work/project/crates/report/tests/support/mod.rs"),
            source,
        );

        assert!(messages.is_empty());
    }

    #[test]
    fn file_inside_src_below_tests_directory_is_flagged() {
        let source = "fn f() { println!(\"x\"); }";

        let messages = messages_at(Path::new("/home/tests/project/src/lib.rs"), source);

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn for_loop_binding_named_exit_is_not_flagged() {
        let source = "use std::process::exit;\nfn f() { for exit in hooks { exit(1); } }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn full_exit_path_is_flagged() {
        let messages = messages("fn f() { std::process::exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn glob_import_from_other_crate_hides_bare_exit() {
        let source = "fn f() { use std::process::exit; { use other::*; exit(1); } }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn glob_import_from_other_crate_keeps_full_path() {
        let messages = messages("use super::*;\nfn f() { std::process::exit(1); }");

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn glob_import_from_other_crate_keeps_print() {
        let messages = messages("use super::*;\nfn f() { println!(\"x\"); }");

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn if_let_binding_named_exit_is_not_flagged() {
        let source = "use std::process::exit;\nfn f() { if let Some(exit) = hook { exit(1); } }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn inner_cfg_test_on_file_is_not_flagged() {
        let source = "#![cfg(test)]\n\nfn helper() { println!(\"x\"); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn inner_cfg_test_on_module_is_not_flagged() {
        let source = "mod fixtures {\n    #![cfg(test)]\n    fn f() { println!(\"x\"); }\n}";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn is_exempt_file_with_bench_directory_returns_true() {
        let path = Path::new("/work/project/benches/walk.rs");

        let exempt = is_exempt_file(path);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_bin_directory_outside_src_returns_false() {
        let path = Path::new("/work/project/tools/bin/tool.rs");

        let exempt = is_exempt_file(path);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_bin_directory_returns_true() {
        let path = Path::new("/work/project/src/bin/tool.rs");

        let exempt = is_exempt_file(path);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_build_module_in_src_returns_false() {
        let path = Path::new("/work/project/src/commands/build.rs");

        let exempt = is_exempt_file(path);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_build_script_in_relative_root_returns_true() {
        let path = Path::new("build.rs");

        let exempt = is_exempt_file(path);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_build_script_returns_true() {
        let path = Path::new("/work/project/crates/report/build.rs");

        let exempt = is_exempt_file(path);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_file_named_tests_returns_false() {
        let path = Path::new("/work/project/src/tests.rs");

        let exempt = is_exempt_file(path);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_main_in_bin_directory_returns_true() {
        let path = Path::new("/work/project/src/bin/tool/main.rs");

        let exempt = is_exempt_file(path);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_module_below_bin_directory_returns_false() {
        let path = Path::new("/work/project/src/bin/tool/render.rs");

        let exempt = is_exempt_file(path);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_relative_path_returns_true() {
        let path = Path::new("tests/check.rs");

        let exempt = is_exempt_file(path);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_with_src_below_examples_returns_false() {
        let path = Path::new("/work/project/examples/sample/src/report.rs");

        let exempt = is_exempt_file(path);

        assert!(!exempt);
    }

    #[test]
    fn is_exempt_file_with_tests_below_src_returns_true() {
        let path = Path::new("/work/project/src/tests/fixtures.rs");

        let exempt = is_exempt_file(path);

        assert!(exempt);
    }

    #[test]
    fn is_exempt_file_without_cargo_directory_returns_false() {
        let path = Path::new("render.rs");

        let exempt = is_exempt_file(path);

        assert!(!exempt);
    }

    #[test]
    fn let_binding_after_call_is_flagged() {
        let source = "use std::process::exit;\nfn f() { exit(1); let exit = 2; }";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn let_binding_in_outer_function_is_flagged() {
        let source = "use std::process::exit;\nfn f() { let exit = 2; fn g() { exit(1); } }";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn let_binding_named_exit_is_not_flagged() {
        let source = "use std::process::exit;\nfn f() { let exit = |code| code; exit(1); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn local_function_named_exit_is_not_flagged() {
        let messages = messages("fn exit(code: i32) {}\nfn f() { exit(1); }");

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

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn local_module_named_process_is_not_flagged() {
        let source = "mod process { pub fn exit(_: i32) {} }\nfn f() { process::exit(1); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn main_file_is_not_flagged() {
        let source = "fn main() { eprintln!(\"x\"); std::process::exit(1); }";

        let messages = messages_at(Path::new("/work/project/src/main.rs"), source);

        assert!(messages.is_empty());
    }

    #[test]
    fn match_arm_binding_named_exit_is_not_flagged() {
        let source = "use std::process::exit;\nfn f() { match hook { exit => exit(1) } }";

        let messages = messages(source);

        assert!(messages.is_empty());
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

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn print_from_other_crate_is_not_flagged() {
        let messages = messages("use defmt::println;\nfn f() { println!(\"x\"); }");

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
        let messages = messages("fn main() { println!(\"x\"); }");

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
    fn qualified_print_is_flagged() {
        let messages = messages("fn f() { std::println!(\"x\"); }");

        assert_eq!(messages, vec![print_message("println")]);
    }

    #[test]
    fn stderr_after_import_is_flagged() {
        let messages = messages("use std::io::stderr;\nfn f() { let e = stderr(); }");

        assert_eq!(messages, vec![handle_message("stderr")]);
    }

    #[test]
    fn stdin_is_not_flagged() {
        let messages = messages("fn f() { let input = std::io::stdin(); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn stdout_after_module_import_is_flagged() {
        let source = "use std::io::{self, Write};\nfn f() { io::stdout().flush(); }";

        let messages = messages(source);

        assert_eq!(messages, vec![handle_message("stdout")]);
    }

    #[test]
    fn stdout_full_path_is_flagged() {
        let messages = messages("fn f() { let out = std::io::stdout().lock(); }");

        assert_eq!(messages, vec![handle_message("stdout")]);
    }

    #[test]
    fn stdout_passed_without_call_is_not_flagged() {
        let messages = messages("fn f() { render(std::io::stdout); }");

        assert!(messages.is_empty());
    }

    #[test]
    fn test_attribute_after_doc_comment_is_not_flagged() {
        let source = "/// Checks the output\n#[test]\nfn t() { println!(\"x\"); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn test_attribute_with_path_and_arguments_is_not_flagged() {
        let source = "#[tokio::test(flavor = \"multi_thread\")]\nasync fn t() { println!(\"x\"); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn test_function_is_not_flagged() {
        let source =
            "#[test]\n#[should_panic]\nfn t() { eprintln!(\"x\"); std::process::exit(1); }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<TerminalOutput>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<TerminalOutput>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<TerminalOutput>();
    }

    #[test]
    fn use_after_function_still_resolves() {
        let source = "fn f() { process::exit(1); }\nuse std::process;";

        let messages = messages(source);

        assert_eq!(messages, vec![EXIT_MESSAGE]);
    }

    #[test]
    fn writer_parameter_is_not_flagged() {
        let source =
            "fn render(out: &mut impl Write, n: u8) -> io::Result<()> { out.write_all(b\"x\") }";

        let messages = messages(source);

        assert!(messages.is_empty());
    }
}
