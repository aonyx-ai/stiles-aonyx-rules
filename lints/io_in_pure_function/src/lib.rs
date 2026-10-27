mod imports;
mod test_code;

use whisker_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use whisker_types::{DecoratedNode, Diagnostic, LintPass, Severity};

use crate::imports::{resolve, strip_raw};
use crate::test_code::is_test_code;

const RULE_ID: RuleId = RuleId::new("lint.io-in-pure-function");

/// Function names that promise a value built from inputs alone
const PURE_NAMES: [&str; 3] = ["new", "try_from", "from_str"];

/// Name prefixes that promise a value built from inputs alone
///
/// A prefix counts as the whole name or before an underscore, so
/// `parse_config` is pure and `parser` is not.
const PURE_PREFIXES: [&str; 3] = ["parse", "resolve", "plan"];

/// Paths whose items do I/O or hold a resource that does
///
/// `std::net` is not here as a whole: an address type such as `Ipv4Addr` is a
/// value. Only the sockets and the trait that resolves a host name reach the
/// network.
const IO_MODULES: [&[&str]; 11] = [
    &["std", "fs"],
    &["std", "process", "Command"],
    &["std", "net", "TcpListener"],
    &["std", "net", "TcpStream"],
    &["std", "net", "ToSocketAddrs"],
    &["std", "net", "UdpSocket"],
    &["reqwest", "Client"],
    &["reqwest", "ClientBuilder"],
    &["reqwest", "blocking"],
    &["gix", "ThreadSafeRepository"],
    &["libloading"],
];

/// Functions that do I/O in a crate whose other items are values
///
/// `reqwest` and `gix` re-export URL, header, hash, and date types that do no
/// I/O, so only these entry points count. `gix::open` is also a module of
/// open options, which is why the match here is exact.
const IO_FUNCTIONS: [&[&str]; 8] = [
    &["reqwest", "get"],
    &["gix", "discover"],
    &["gix", "init"],
    &["gix", "init_bare"],
    &["gix", "open"],
    &["gix", "open_opts"],
    &["gix", "prepare_clone"],
    &["gix", "prepare_clone_bare"],
];

/// Flags I/O in a function whose name says it builds a value from its inputs
///
/// `new`, `try_from`, and `from_str` are pure, and so is a function named
/// `parse`, `resolve`, or `plan`, or starting with one of those and an
/// underscore. I/O lives in a function named for it, such as `load`,
/// `fetch`, `build`, or `run`. The split keeps the pure half testable
/// without a toolchain, and it lets a program fail on its inputs before it
/// spends anything.
///
/// The rule reports each reference in the body to an I/O path. Those paths
/// reach the file system, a subprocess, a socket, an HTTP client, a Git
/// repository, or a dynamic library. Reading an environment variable is not
/// I/O here: it is an input.
///
/// Each path resolves through the `use` declarations around it. After
/// `use std::fs;`, `fs::read` counts. After `use std::fs::read_to_string;`, a
/// bare `read_to_string` counts, unless the function binds that name itself.
/// A glob import hides the name, which the rule accepts as a false negative.
///
/// Only the function's own body counts, closures included. A call to
/// another local function is invisible to the rule. So is a method call on a
/// value and anything inside a macro invocation. Test code is exempt. The
/// rule reads no options: the names are the convention, not a setting.
pub struct IoInPureFunction;

impl IoInPureFunction {
    /// Creates a boxed [`LintPass`] suitable for the whisker pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = IoInPureFunction::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

/// Returns whether a function name promises a pure function
fn is_pure_name(name: &str) -> bool {
    if PURE_NAMES.contains(&name) {
        return true;
    }
    PURE_PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
    })
}

/// Returns whether a resolved path reaches one of the I/O paths
fn is_io_path(path: &[String]) -> bool {
    let within_module = IO_MODULES.iter().any(|module| {
        path.len() >= module.len() && path.iter().zip(module.iter()).all(|(a, b)| a == b)
    });
    let names_function = IO_FUNCTIONS.iter().any(|function| {
        path.len() == function.len() && path.iter().zip(function.iter()).all(|(a, b)| a == b)
    });
    within_module || names_function
}

/// Collects the path references in a function body that may name I/O
///
/// A multi-segment path counts wherever it stands, called or passed as a
/// value. A bare name counts only as the callee of a call or as an
/// argument, because elsewhere it names a local. The walk does not enter a
/// nested item, because the rule checks that item on its own. It does not
/// enter a macro invocation, whose tokens tree-sitter does not parse as paths.
fn collect_references<'a>(node: &DecoratedNode<'a>, references: &mut Vec<DecoratedNode<'a>>) {
    match node.kind() {
        "scoped_identifier" => {
            references.push(*node);
            return;
        }
        "identifier" => {
            if is_called_or_passed(node) {
                references.push(*node);
            }
            return;
        }
        "macro_invocation" | "function_item" | "impl_item" | "trait_item" | "mod_item"
        | "use_declaration" | "const_item" | "static_item" => return,
        _ => {}
    }

    for child in node.named_children() {
        collect_references(&child, references);
    }
}

/// Collects every name a pattern in the function binds
///
/// A parameter, a closure parameter, or a `let` named like an imported
/// function shadows the import, so a bare call of that name is local. The
/// set ignores where in the function each binding is visible. A name bound
/// anywhere is local everywhere, which can only silence the rule.
fn collect_local_names<'a>(node: &DecoratedNode<'a>, names: &mut Vec<&'a str>) {
    match node.kind() {
        "macro_invocation" | "function_item" | "impl_item" | "trait_item" | "mod_item"
        | "const_item" | "static_item" => return,
        "closure_parameters" => {
            collect_identifiers(node, names);
            return;
        }
        "parameter" | "let_declaration" | "for_expression" | "match_arm" | "let_condition" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                collect_identifiers(&pattern, names);
            }
        }
        _ => {}
    }

    for child in node.named_children() {
        collect_local_names(&child, names);
    }
}

/// Collects every identifier under a pattern
fn collect_identifiers<'a>(node: &DecoratedNode<'a>, names: &mut Vec<&'a str>) {
    if node.kind() == "identifier" {
        names.push(strip_raw(node.text()));
    }
    for child in node.named_children() {
        collect_identifiers(&child, names);
    }
}

/// Returns whether a bare name is a callee or a call argument
fn is_called_or_passed(node: &DecoratedNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "arguments" => true,
        "call_expression" => parent
            .child_by_field_name("function")
            .is_some_and(|function| function.id() == node.id()),
        _ => false,
    }
}

impl RustLintPass for IoInPureFunction {
    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(name) = node.child_by_field_name("name") else {
            return Vec::new();
        };
        let name = strip_raw(name.text());
        if !is_pure_name(name) {
            return Vec::new();
        }
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };
        if is_test_code(node) {
            return Vec::new();
        }

        let mut references = Vec::new();
        collect_references(&body, &mut references);

        let mut local_names = Vec::new();
        if let Some(parameters) = node.child_by_field_name("parameters") {
            collect_local_names(&parameters, &mut local_names);
        }
        collect_local_names(&body, &mut local_names);

        let mut diagnostics = Vec::new();
        for reference in references {
            if reference.kind() == "identifier"
                && local_names.contains(&strip_raw(reference.text()))
            {
                continue;
            }
            let Some(path) = resolve(&reference) else {
                continue;
            };
            if !is_io_path(&path) {
                continue;
            }
            diagnostics.push(Diagnostic::new(
                RULE_ID,
                Severity::Warn,
                format!(
                    "`{name}` reaches `{}`, which is I/O; a function with this name builds a \
                     value from its inputs alone, so move the I/O into a `load`, `fetch`, \
                     `build`, or `run` function",
                    path.join("::"),
                ),
                reference.span(),
            ));
        }
        diagnostics
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for IoInPureFunction {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.io-in-pure-function")]
    }
}

whisker_plugin::export_lints![rust: [IoInPureFunction]];

#[cfg(test)]
mod tests {
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::Language;

    use super::*;

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![IoInPureFunction::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    #[test]
    fn aliased_module_import_is_flagged_naming_the_resolved_path() {
        let diagnostics = run("use std::fs as filesystem;\nfn new() { filesystem::read(p); }");

        assert_diagnostic(&diagnostics[0]).message_contains("`std::fs::read`");
    }

    #[test]
    fn aliased_module_import_is_flagged_once() {
        let diagnostics = run("use std::fs as filesystem;\nfn new() { filesystem::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn aliased_type_import_is_flagged() {
        let diagnostics =
            run("use std::process::Command as Cmd;\nfn plan() { Cmd::new(\"git\"); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn async_block_call_is_flagged() {
        let diagnostics = run("fn new() { let f = async { std::fs::read(p) }; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bare_call_through_function_import_is_flagged_naming_the_resolved_path() {
        let diagnostics =
            run("use std::fs::read_to_string;\nfn from_str(s: &str) { read_to_string(s); }");

        assert_diagnostic(&diagnostics[0]).message_contains("`std::fs::read_to_string`");
    }

    #[test]
    fn bare_call_through_function_import_is_flagged_once() {
        let diagnostics =
            run("use std::fs::read_to_string;\nfn from_str(s: &str) { read_to_string(s); }");

        assert_eq!(diagnostics.len(), 1);
    }

    /// A bare name that is neither called nor passed names a local
    ///
    /// `read` here is the binding of the `let`, whatever the file imports.
    #[test]
    fn bare_name_outside_a_call_is_not_flagged() {
        let diagnostics = run("use std::fs::read;\nfn new() { let read = 1; let x = read + 1; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn bare_name_passed_as_argument_is_flagged() {
        let diagnostics =
            run("use std::fs::read_to_string;\nfn parse_all() { paths.map(read_to_string); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bare_name_without_import_is_not_flagged() {
        let diagnostics = run("fn new() { read_to_string(p); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn block_scoped_import_is_flagged() {
        let diagnostics = run("fn new() { use std::fs; fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    /// A `cfg` that also holds outside a test build gates shipped code
    #[test]
    fn cfg_any_test_or_feature_is_flagged() {
        let diagnostics =
            run("#[cfg(any(test, feature = \"fixtures\"))]\nfn new() { std::fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_attr_test_does_not_exempt() {
        let diagnostics =
            run("#[cfg_attr(test, allow(dead_code))]\nfn new() { std::fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_not_test_function_is_flagged() {
        let diagnostics = run("#[cfg(not(test))]\nfn new() { std::fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_test_function_is_not_flagged() {
        let diagnostics = run("#[cfg(test)]\nfn new() { std::fs::read(p); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_impl_is_not_flagged() {
        let diagnostics = run("#[cfg(test)]\nimpl Fixture { fn new() { std::fs::read(p); } }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_module_is_not_flagged() {
        let diagnostics =
            run("#[cfg(test)]\nmod tests {\n    use std::fs;\n    fn new() { fs::read(p); }\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_with_other_predicate_is_not_flagged() {
        let diagnostics =
            run("#[cfg(all(test, unix))]\nmod tests {\n    fn new() { std::fs::read(p); }\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn closure_call_is_flagged() {
        let diagnostics = run("fn resolve() { let f = || std::fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn command_new_is_flagged_as_a_warning_naming_the_call() {
        let diagnostics = run(
            "use std::process::Command;\nfn plan_build() { Command::new(\"cargo\").arg(\"build\"); }",
        );

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.io-in-pure-function")
            .has_severity(Severity::Warn)
            .message_contains("`plan_build` reaches `std::process::Command::new`");
    }

    #[test]
    fn command_new_is_flagged_once() {
        let diagnostics = run(
            "use std::process::Command;\nfn plan_build() { Command::new(\"cargo\").arg(\"build\"); }",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn environment_variable_read_is_not_flagged() {
        let diagnostics = run("fn new() { std::env::var(\"HOME\"); std::env::vars(); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn fully_qualified_call_is_flagged() {
        let diagnostics = run("fn new(path: &Path) { std::fs::read_to_string(path); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn function_path_passed_as_value_is_flagged() {
        let diagnostics = run("fn parse_files() { paths.iter().map(std::fs::read); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn gix_entry_point_is_flagged() {
        let diagnostics = run("fn resolve_repo() { gix::discover(\".\"); }");

        assert_eq!(diagnostics.len(), 1);
    }

    /// `gix::open` is both a function and a module of open options
    ///
    /// Only the function does I/O.
    #[test]
    fn gix_open_options_are_not_flagged() {
        let diagnostics = run("fn new() { gix::open::Options::isolated(); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn gix_thread_safe_repository_is_flagged() {
        let diagnostics = run("fn new() { gix::ThreadSafeRepository::open(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn gix_value_type_is_not_flagged() {
        let diagnostics = run("fn parse_id(s: &str) { gix::ObjectId::from_hex(s); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn glob_import_is_not_flagged() {
        let diagnostics = run("use std::fs::*;\nfn new() { read_to_string(p); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn grouped_import_is_flagged() {
        let diagnostics = run(
            "use std::{fs::{self, File}, process::Command};\nfn new() { fs::read(p); File::open(p); Command::new(c); }",
        );

        assert_eq!(diagnostics.len(), 3);
    }

    #[test]
    fn grouped_self_alias_is_flagged() {
        let diagnostics = run("use std::fs::{self as disk};\nfn new() { disk::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn import_from_another_crate_is_not_flagged() {
        let diagnostics = run("use tokio::process::Command;\nfn new() { Command::new(c); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn import_in_a_sibling_function_is_not_used() {
        let diagnostics = run("fn other() { use std::fs; }\nfn new() { fs::read(p); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn import_in_parent_module_does_not_reach_nested_module() {
        let diagnostics = run("use std::fs;\nmod inner {\n    fn new() { fs::read(p); }\n}");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn import_in_same_nested_module_is_flagged() {
        let diagnostics = run("mod inner {\n    use std::fs;\n    fn new() { fs::read(p); }\n}");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn inner_block_import_shadows_outer() {
        let diagnostics = run("use std::fs;\nfn new() { use crate::fs; fs::read(p); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_cfg_test_on_file_is_not_flagged() {
        let diagnostics = run("#![cfg(test)]\nfn new() { std::fs::read(p); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn io_function_name_is_not_flagged() {
        let diagnostics = run(
            "fn load() { std::fs::read(p); }\nfn fetch() { reqwest::get(u); }\nfn build() { std::process::Command::new(c); }\nfn run() { std::fs::write(p, b); }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn leading_colons_are_flagged() {
        let diagnostics = run("fn new() { ::std::fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    /// A leading `::` names the crate, so a local import cannot shadow it
    #[test]
    fn leading_colons_skip_import_lookup() {
        let diagnostics = run("use crate::shim as std;\nfn new() { ::std::fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn let_binding_shadowing_import_is_not_flagged() {
        let diagnostics = run(
            "use std::fs::read;\nfn new() { let read = make(); read(p); if let Some(read) = r { read(p); } }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn libloading_is_flagged() {
        let diagnostics =
            run("use libloading::Library;\nfn new(path: &Path) { unsafe { Library::new(path) }; }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn local_module_named_fs_is_not_flagged() {
        let diagnostics = run("mod fs { pub fn read() {} }\nfn new() { fs::read(); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn macro_body_is_not_flagged() {
        let diagnostics =
            run("fn new() { vec![std::fs::read(p)]; format!(\"{}\", std::fs::read(p)); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_call_on_a_value_is_not_flagged() {
        let diagnostics =
            run("fn new(path: &Path) { path.exists(); file.read_to_string(&mut s); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn multiple_calls_are_each_flagged() {
        let diagnostics = run("fn new() { std::fs::read(a); std::fs::write(b, c); }");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn name_with_prefix_and_suffix_is_flagged() {
        let diagnostics = run(
            "fn parse_config() { std::fs::read(p); }\nfn resolve_target() { std::fs::read(p); }\nfn plan_steps() { std::fs::read(p); }",
        );

        assert_eq!(diagnostics.len(), 3);
    }

    #[test]
    fn name_with_prefix_inside_a_word_is_not_flagged() {
        let diagnostics = run(
            "fn parsed() { std::fs::read(p); }\nfn parser() { std::fs::read(p); }\nfn resolver() { std::fs::read(p); }\nfn planet() { std::fs::read(p); }\nfn renew() { std::fs::read(p); }\nfn new_from_disk() { std::fs::read(p); }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_function_is_checked_on_its_own() {
        let diagnostics = run("fn new() { fn load() { std::fs::read(p); } load(); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_pure_function_is_flagged_once() {
        let diagnostics = run("fn load() { fn parse() { std::fs::read(p); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn net_address_type_is_not_flagged() {
        let diagnostics = run(
            "use std::net::{IpAddr, Ipv4Addr, SocketAddr};\nfn parse_addr() { Ipv4Addr::new(127, 0, 0, 1); SocketAddr::new(ip, 80); IpAddr::from(ip); }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn net_socket_is_flagged() {
        let diagnostics = run(
            "use std::net::{self, TcpStream};\nfn new() { TcpStream::connect(a); net::UdpSocket::bind(a); std::net::TcpListener::bind(a); }",
        );

        assert_eq!(diagnostics.len(), 3);
    }

    #[test]
    fn new_is_flagged_naming_the_call() {
        let diagnostics = run(
            "impl CheckPlan {\n    pub fn new(path: &Path) -> anyhow::Result<Self> {\n        let config = std::fs::read_to_string(path.join(\"whisker.toml\"))?;\n        todo!()\n    }\n}",
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("`new` reaches `std::fs::read_to_string`");
    }

    #[test]
    fn new_is_flagged_once() {
        let diagnostics = run(
            "impl CheckPlan {\n    pub fn new(path: &Path) -> anyhow::Result<Self> {\n        let config = std::fs::read_to_string(path.join(\"whisker.toml\"))?;\n        todo!()\n    }\n}",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    /// A parameter lives in the value namespace and a module in the type
    /// namespace, so a parameter named `fs` leaves `fs::read` alone
    #[test]
    fn parameter_named_like_a_module_is_flagged() {
        let diagnostics = run("use std::fs;\nfn new(fs: &Fs) { fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn parameter_shadowing_import_is_not_flagged() {
        let diagnostics = run(
            "use std::fs::read;\nfn new(read: impl Fn()) { read(); }\nfn parse() { items.map(|read| read(x)); }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn process_exit_is_not_flagged() {
        let diagnostics = run("fn parse() { std::process::exit(1); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pure_function_without_io_is_not_flagged() {
        let diagnostics = run("fn resolve(config: &Config) -> Plan { Plan::from(config) }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn raw_identifier_name_is_flagged() {
        let diagnostics = run("fn r#new() { std::fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn reqwest_client_is_flagged() {
        let diagnostics = run(
            "fn new() { reqwest::Client::new(); reqwest::Client::builder(); reqwest::blocking::get(u); reqwest::get(u); }",
        );

        assert_eq!(diagnostics.len(), 4);
    }

    #[test]
    fn reqwest_value_type_is_not_flagged() {
        let diagnostics = run(
            "fn parse_url(s: &str) { reqwest::Url::parse(s); reqwest::Method::from_bytes(b); reqwest::header::HeaderValue::from_static(v); }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn signature_without_body_is_not_flagged() {
        let diagnostics = run("trait Load { fn new() -> Self; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn test_attribute_is_not_flagged() {
        let diagnostics = run("#[tokio::test]\nasync fn parse() { std::fs::read(p); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_default_body_is_flagged() {
        let diagnostics = run("trait Plan { fn plan() { std::fs::read(p); } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn trait_send() {
        fn assert_send<T: Send>() {}
        assert_send::<IoInPureFunction>();
    }

    #[test]
    fn trait_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<IoInPureFunction>();
    }

    #[test]
    fn trait_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<IoInPureFunction>();
    }

    #[test]
    fn try_from_and_from_str_are_flagged() {
        let diagnostics = run(
            "impl TryFrom<&Path> for Config {\n    fn try_from(p: &Path) { std::fs::read(p); }\n}\nimpl FromStr for Config {\n    fn from_str(s: &str) { std::fs::read(s); }\n}",
        );

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn turbofish_call_is_flagged() {
        let diagnostics = run("use std::fs;\nfn new() { fs::read::<&str>(p); }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn type_annotation_is_not_flagged() {
        let diagnostics = run("fn new(file: std::fs::File) -> Self { let f: fs::File = file; }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn underscore_alias_binds_nothing() {
        let diagnostics = run("use std::fs::read as _;\nfn new() { read(p); }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn use_list_without_path_is_flagged() {
        let diagnostics = run("use {std::fs, std::io};\nfn new() { fs::read(p); }");

        assert_eq!(diagnostics.len(), 1);
    }
}
