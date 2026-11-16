#[cfg(feature = "plugin")]
use stiles_rust::ReadsTestCode as _;
use stiles_rust::decorations::Resolution;
use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter, is_test_code};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, Severity};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(io_in_pure_function::RULE_ID.as_str(), "lint.io-in-pure-function");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.io-in-pure-function");

/// Function names that promise a value built from inputs alone
const PURE_NAMES: [&str; 3] = ["new", "try_from", "from_str"];

/// Name prefixes that promise a value built from inputs alone
///
/// A prefix counts as the whole name or before an underscore, so
/// `parse_config` is pure and `parser` is not.
const PURE_PREFIXES: [&str; 3] = ["parse", "resolve", "plan"];

/// Definition paths whose items do I/O or hold a resource that does
///
/// A path here names where the item is defined, not where a crate
/// re-exports it, so `std::net::TcpStream` appears as `std::net::tcp::TcpStream`.
/// `std::net` is not here as a whole: an address type such as `Ipv4Addr` is a
/// value. Only the sockets and the trait that resolves a host name reach the
/// network.
const IO_MODULES: [&[&str]; 11] = [
    &["std", "fs"],
    &["std", "process", "Command"],
    &["std", "net", "tcp", "TcpListener"],
    &["std", "net", "tcp", "TcpStream"],
    &["std", "net", "socket_addr", "ToSocketAddrs"],
    &["std", "net", "udp", "UdpSocket"],
    &["reqwest", "async_impl", "client", "Client"],
    &["reqwest", "async_impl", "client", "ClientBuilder"],
    &["reqwest", "blocking"],
    &["gix", "types", "ThreadSafeRepository"],
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
/// The `Resolution` decoration of each name gives the path of its
/// definition. It follows a `use`, a glob, a rename, and a re-export, so
/// `read_to_string` counts after `use std::fs::*;`. A name that resolves to
/// a local binding does not count. A path also counts when a qualifier in
/// it names an I/O type, as `reqwest::Client` does in `reqwest::Client::default`.
/// A name without the decoration does not count.
///
/// Only the function's own body counts, closures included. A call to
/// another local function is invisible to the rule. So is a method call on a
/// value and anything inside a macro invocation. Test code is exempt, which
/// the Rust provider marks with the `TestCode` decoration. A file is test
/// code too when the kinds of its `Target` decoration are only integration
/// tests and benches. The rule reads no options: the names are the
/// convention, not a setting.
///
/// # Examples
///
/// ```text
/// // DON'T
/// use std::fs;
///
/// fn parse_config(path: &str) -> io::Result<Config> {
///     let text = fs::read_to_string(path)?;
///     Ok(Config::from_toml(&text))
/// }
///
/// // DO
/// use std::fs;
///
/// fn load_config(path: &str) -> io::Result<Config> {
///     let text = fs::read_to_string(path)?;
///     Ok(parse_config(&text))
/// }
///
/// fn parse_config(text: &str) -> Config {
///     Config::from_toml(text)
/// }
/// ```
///
/// The rule reads the resolution of each name, which the Rust provider
/// attaches as a decoration. This example attaches it by hand.
///
/// ```
/// use io_in_pure_function::IoInPureFunction;
/// use stiles_rust::decorations::{DefinitionKind, DefinitionOrigin, Resolution, TypePath};
/// use stiles_testing::{decorate, execute, parse};
/// use stiles_types::{DecorationMap, Language};
///
/// let mut tree = parse("fn new() { std::fs::read(path); }", Language::Rust);
/// let function = tree.root_node().named_child(0).unwrap();
/// let statement = function.child_by_field_name("body").unwrap().named_child(0).unwrap();
/// let call = statement.named_child(0).unwrap();
/// let callee = call.child_by_field_name("function").unwrap().id();
/// let mut decorations = DecorationMap::new();
/// let read = TypePath::builder().krate("std").modules(["fs"]).name("read").build();
/// decorations.insert(
///     callee,
///     Resolution::builder()
///         .kind(DefinitionKind::Function)
///         .origin(DefinitionOrigin::OtherCrate)
///         .krate("std")
///         .path(read)
///         .build(),
/// );
/// decorate(&mut tree, decorations);
/// let mut passes = vec![IoInPureFunction::into_lint_pass()];
///
/// let diagnostics = execute(&tree, &mut passes);
///
/// assert_eq!(diagnostics.len(), 1);
/// ```
pub struct IoInPureFunction;

impl IoInPureFunction {
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
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

/// Returns whether a definition path lies within one of the I/O modules or types
fn is_within_io_module(path: &[String]) -> bool {
    IO_MODULES.iter().any(|module| {
        path.len() >= module.len() && path.iter().zip(module.iter()).all(|(a, b)| a == b)
    })
}

/// Returns whether a definition path is one of the I/O functions
fn is_io_function(path: &[String]) -> bool {
    IO_FUNCTIONS
        .iter()
        .any(|function| path.iter().map(String::as_str).eq(function.iter().copied()))
}

/// Returns the segments of the definition a name resolves to, crate first
fn definition_path(node: &DecoratedNode<'_>) -> Option<Vec<String>> {
    let path = node.decoration::<Resolution>()?.path()?;
    let segments = std::iter::once(path.krate())
        .chain(path.modules())
        .chain(std::iter::once(path.name()));

    Some(segments.map(str::to_owned).collect())
}

/// Returns the I/O path that a reference or one of its qualifiers reaches
///
/// The reference itself counts when it lies within an I/O module or is an
/// I/O function. A qualifier counts only within an I/O module. So
/// `gix::open::Options` is no call of the function `gix::open`.
fn io_path(reference: &DecoratedNode<'_>) -> Option<Vec<String>> {
    let own =
        definition_path(reference).filter(|path| is_within_io_module(path) || is_io_function(path));
    let mut qualifiers = std::iter::successors(reference.child_by_field_name("path"), |node| {
        node.child_by_field_name("path")
    });

    own.or_else(|| {
        qualifiers.find_map(|qualifier| {
            definition_path(&qualifier).filter(|path| is_within_io_module(path))
        })
    })
}

/// Collects the names and paths in a function body that may name I/O
///
/// The walk does not enter a nested item, because the rule checks that item
/// on its own. It does not enter a macro invocation, whose tokens carry no
/// resolution.
fn collect_references<'a>(node: &DecoratedNode<'a>, references: &mut Vec<DecoratedNode<'a>>) {
    match node.kind() {
        "scoped_identifier" | "identifier" => {
            references.push(*node);
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

impl RustLintPass for IoInPureFunction {
    fn check_function_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(name) = node.child_by_field_name("name") else {
            return Vec::new();
        };
        let name = name.text();
        let name = name.strip_prefix("r#").unwrap_or(name);
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

        references
            .iter()
            .filter_map(|reference| io_path(reference).map(|path| (reference, path)))
            .map(|(reference, path)| {
                Diagnostic::builder()
                    .rule_id(RULE_ID)
                    .severity(Severity::Error)
                    .message(format!(
                        "`{name}` reaches `{}`, which is I/O; a function with this name builds a \
                     value from its inputs alone, so move the I/O into a `load`, `fetch`, \
                     `build`, or `run` function",
                        path.join("::"),
                    ))
                    .span(reference.span())
                    .build()
            })
            .collect()
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for IoInPureFunction {
    fn rules(&self) -> Vec<RuleId> {
        vec![RuleId::new("lint.io-in-pure-function")]
    }

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::of::<Resolution>().and_test_code()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [IoInPureFunction]];

#[cfg(test)]
mod tests {
    use stiles_rust::decorations::{DefinitionKind, DefinitionOrigin, TargetKind, TypePath};
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use stiles_types::{DecoratedTree, DecorationMap, Language};

    use super::*;

    /// A name as the source spells it, and the path of its definition
    ///
    /// An empty path stands for a local binding, which has none.
    type Resolved<'a> = (&'a str, &'a str);

    const READ: Resolved<'static> = ("std::fs::read", "std::fs::read");

    /// Returns the resolution the provider gives a name with this definition path
    fn resolution(path: &str) -> Resolution {
        let segments: Vec<&str> = path.split("::").collect();
        let Some((name, rest)) = segments.split_last() else {
            unreachable!("split yields at least one segment");
        };
        let Some((krate, modules)) = rest.split_first() else {
            return Resolution::builder()
                .kind(DefinitionKind::LocalBinding)
                .origin(DefinitionOrigin::SameCrate)
                .krate("sample")
                .build();
        };
        let kind = match name.starts_with(char::is_uppercase) {
            true => DefinitionKind::Struct,
            false => DefinitionKind::Function,
        };
        let origin = match *krate == "sample" {
            true => DefinitionOrigin::SameCrate,
            false => DefinitionOrigin::OtherCrate,
        };

        Resolution::builder()
            .kind(kind)
            .origin(origin)
            .krate(krate)
            .path(
                TypePath::builder()
                    .krate(krate)
                    .modules(modules)
                    .name(name)
                    .build(),
            )
            .build()
    }

    fn mark(node: &DecoratedNode<'_>, map: &mut DecorationMap, resolved: &[Resolved<'_>]) {
        let names_something = [
            "identifier",
            "scoped_identifier",
            "type_identifier",
            "scoped_type_identifier",
        ]
        .contains(&node.kind());
        let found = resolved.iter().find(|(text, _)| *text == node.text());
        if let (true, Some((_, path))) = (names_something, found) {
            map.insert(node.id(), resolution(path));
        }
        for child in node.named_children() {
            mark(&child, map, resolved);
        }
    }

    fn run(source: &str, resolved: &[Resolved<'_>]) -> Vec<Diagnostic> {
        run_attaching(source, resolved, |_| {})
    }

    /// Runs the rule after `attach` adds what the provider knows of test code
    fn run_attaching(
        source: &str,
        resolved: &[Resolved<'_>],
        attach: impl FnOnce(&mut DecoratedTree),
    ) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        let mut map = DecorationMap::new();
        mark(&tree.root_node(), &mut map, resolved);
        decorate(&mut tree, map);
        attach(&mut tree);
        let mut passes = vec![IoInPureFunction::into_lint_pass()];
        execute(&tree, &mut passes)
    }

    #[test]
    fn async_block_call_is_flagged() {
        let diagnostics = run("fn new() { let f = async { std::fs::read(p) }; }", &[READ]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bare_call_through_function_import_is_flagged_naming_the_definition() {
        let diagnostics = run(
            "use std::fs::read_to_string;\nfn from_str(s: &str) { read_to_string(s); }",
            &[("read_to_string", "std::fs::read_to_string")],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("`std::fs::read_to_string`");
    }

    #[test]
    fn bare_call_through_function_import_is_flagged_once() {
        let diagnostics = run(
            "use std::fs::read_to_string;\nfn from_str(s: &str) { read_to_string(s); }",
            &[("read_to_string", "std::fs::read_to_string")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bare_name_as_a_value_is_flagged() {
        let diagnostics = run(
            "use std::fs::read_to_string;\nfn parse_all() { let load = read_to_string; }",
            &[("read_to_string", "std::fs::read_to_string")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn bare_name_passed_as_argument_is_flagged() {
        let diagnostics = run(
            "use std::fs::read_to_string;\nfn parse_all() { paths.map(read_to_string); }",
            &[("read_to_string", "std::fs::read_to_string")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    /// A `cfg` that also holds outside a test build gates shipped code
    #[test]
    fn cfg_any_test_or_feature_is_flagged() {
        let diagnostics = run(
            "#[cfg(any(test, feature = \"fixtures\"))]\nfn new() { std::fs::read(p); }",
            &[READ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_attr_test_does_not_exempt() {
        let diagnostics = run(
            "#[cfg_attr(test, allow(dead_code))]\nfn new() { std::fs::read(p); }",
            &[READ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_not_test_function_is_flagged() {
        let diagnostics = run("#[cfg(not(test))]\nfn new() { std::fs::read(p); }", &[READ]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn cfg_test_function_is_not_flagged() {
        let diagnostics = run_attaching(
            "#[cfg(test)]\nfn new() { std::fs::read(p); }",
            &[READ],
            |tree| test_decorations::mark(tree, "fn new"),
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_impl_is_not_flagged() {
        let source = "#[cfg(test)]\nimpl Fixture { fn new() { std::fs::read(p); } }";

        let diagnostics = run_attaching(source, &[READ], |tree| {
            test_decorations::mark(tree, "impl Fixture")
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_module_is_not_flagged() {
        let source =
            "#[cfg(test)]\nmod tests {\n    use std::fs;\n    fn new() { fs::read(p); }\n}";

        let diagnostics = run_attaching(source, &[("fs::read", "std::fs::read")], |tree| {
            test_decorations::mark(tree, "mod tests")
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn cfg_test_with_other_predicate_is_not_flagged() {
        let source = "#[cfg(all(test, unix))]\nmod tests {\n    fn new() { std::fs::read(p); }\n}";

        let diagnostics = run_attaching(source, &[READ], |tree| {
            test_decorations::mark(tree, "mod tests")
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn closure_call_is_flagged() {
        let diagnostics = run("fn resolve() { let f = || std::fs::read(p); }", &[READ]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn command_new_is_flagged_as_an_error_naming_the_call() {
        let diagnostics = run(
            "use std::process::Command;\nfn plan_build() { Command::new(\"cargo\").arg(\"build\"); }",
            &[("Command::new", "std::process::Command::new")],
        );

        assert_diagnostic(&diagnostics[0])
            .has_rule_id("lint.io-in-pure-function")
            .has_severity(Severity::Error)
            .message_contains("`plan_build` reaches `std::process::Command::new`");
    }

    #[test]
    fn command_new_is_flagged_once() {
        let diagnostics = run(
            "use std::process::Command;\nfn plan_build() { Command::new(\"cargo\").arg(\"build\"); }",
            &[("Command::new", "std::process::Command::new")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        pair.assert_example(|source| {
            run(source, &[("fs::read_to_string", "std::fs::read_to_string")])
        });
    }

    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run(
            pair.fix(),
            &[("fs::read_to_string", "std::fs::read_to_string")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn environment_variable_read_is_not_flagged() {
        let diagnostics = run(
            "fn new() { std::env::var(\"HOME\"); std::env::vars(); }",
            &[
                ("std::env::var", "std::env::var"),
                ("std::env::vars", "std::env::vars"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn fully_qualified_call_is_flagged() {
        let diagnostics = run(
            "fn new(path: &Path) { std::fs::read_to_string(path); }",
            &[("std::fs::read_to_string", "std::fs::read_to_string")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn function_path_passed_as_value_is_flagged() {
        let diagnostics = run(
            "fn parse_files() { paths.iter().map(std::fs::read); }",
            &[READ],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn gix_entry_point_is_flagged() {
        let diagnostics = run(
            "fn resolve_repo() { gix::discover(\".\"); }",
            &[("gix::discover", "gix::discover")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    /// `gix::open` is both a function and a module of open options
    ///
    /// Only the function does I/O, so the module as a qualifier does not count.
    #[test]
    fn gix_open_options_are_not_flagged() {
        let diagnostics = run(
            "fn new() { gix::open::Options::isolated(); }",
            &[
                (
                    "gix::open::Options::isolated",
                    "gix::open::Options::isolated",
                ),
                ("gix::open::Options", "gix::open::Options"),
                ("gix::open", "gix::open"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn gix_thread_safe_repository_is_flagged() {
        let diagnostics = run(
            "fn new() { gix::ThreadSafeRepository::open(p); }",
            &[(
                "gix::ThreadSafeRepository::open",
                "gix::types::ThreadSafeRepository::open",
            )],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn gix_value_type_is_not_flagged() {
        let diagnostics = run(
            "fn parse_id(s: &str) { gix::ObjectId::from_hex(s); }",
            &[(
                "gix::ObjectId::from_hex",
                "gix_hash::object_id::ObjectId::from_hex",
            )],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn glob_import_is_flagged() {
        let diagnostics = run(
            "use std::fs::*;\nfn new() { read_to_string(p); }",
            &[("read_to_string", "std::fs::read_to_string")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn import_from_another_crate_is_not_flagged() {
        let diagnostics = run(
            "use tokio::process::Command;\nfn new() { Command::new(c); }",
            &[("Command::new", "tokio::process::Command::new")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn inner_cfg_test_on_file_is_not_flagged() {
        let diagnostics = run_attaching(
            "#![cfg(test)]\nfn new() { std::fs::read(p); }",
            &[READ],
            test_decorations::enclose_file,
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn integration_test_file_is_not_flagged() {
        let diagnostics = run_attaching("fn new() { std::fs::read(p); }", &[READ], |tree| {
            test_decorations::target(tree, [TargetKind::IntegrationTest]);
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn io_function_name_is_not_flagged() {
        let diagnostics = run(
            "fn load() { std::fs::read(p); }\nfn fetch() { reqwest::get(u); }\nfn build() { std::process::Command::new(c); }\nfn run() { std::fs::write(p, b); }",
            &[
                READ,
                ("reqwest::get", "reqwest::get"),
                ("std::process::Command::new", "std::process::Command::new"),
                ("std::fs::write", "std::fs::write"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn leading_colons_are_flagged() {
        let diagnostics = run(
            "fn new() { ::std::fs::read(p); }",
            &[("::std::fs::read", "std::fs::read")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn libloading_is_flagged() {
        let diagnostics = run(
            "use libloading::Library;\nfn new(path: &Path) { unsafe { Library::new(path) }; }",
            &[("Library::new", "libloading::safe::Library::new")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn local_module_named_fs_is_not_flagged() {
        let diagnostics = run(
            "mod fs { pub fn read() {} }\nfn new() { fs::read(); }",
            &[("fs::read", "sample::fs::read")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn macro_body_is_not_flagged() {
        let diagnostics = run(
            "fn new() { vec![std::fs::read(p)]; format!(\"{}\", std::fs::read(p)); }",
            &[READ, ("read", "std::fs::read")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn method_call_on_a_value_is_not_flagged() {
        let diagnostics = run(
            "fn new(path: &Path) { path.exists(); file.read_to_string(&mut s); }",
            &[],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn multiple_calls_are_each_flagged() {
        let diagnostics = run(
            "fn new() { std::fs::read(a); std::fs::write(b, c); }",
            &[READ, ("std::fs::write", "std::fs::write")],
        );

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn name_resolving_to_a_local_binding_is_not_flagged() {
        let diagnostics = run(
            "use std::fs::read;\nfn new(read: impl Fn()) { read(); }\nfn parse() { let read = make(); read(p); }",
            &[("read", "")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn name_with_prefix_and_suffix_is_flagged() {
        let diagnostics = run(
            "fn parse_config() { std::fs::read(p); }\nfn resolve_target() { std::fs::read(p); }\nfn plan_steps() { std::fs::read(p); }",
            &[READ],
        );

        assert_eq!(diagnostics.len(), 3);
    }

    #[test]
    fn name_with_prefix_inside_a_word_is_not_flagged() {
        let diagnostics = run(
            "fn parsed() { std::fs::read(p); }\nfn parser() { std::fs::read(p); }\nfn resolver() { std::fs::read(p); }\nfn planet() { std::fs::read(p); }\nfn renew() { std::fs::read(p); }\nfn new_from_disk() { std::fs::read(p); }",
            &[READ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_function_is_checked_on_its_own() {
        let diagnostics = run(
            "fn new() { fn load() { std::fs::read(p); } load(); }",
            &[READ, ("load", "sample::load")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn nested_pure_function_is_flagged_once() {
        let diagnostics = run("fn load() { fn parse() { std::fs::read(p); } }", &[READ]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn net_address_type_is_not_flagged() {
        let diagnostics = run(
            "use std::net::{IpAddr, Ipv4Addr, SocketAddr};\nfn parse_addr() { Ipv4Addr::new(127, 0, 0, 1); SocketAddr::new(ip, 80); IpAddr::from(ip); }",
            &[
                ("Ipv4Addr::new", "core::net::ip_addr::Ipv4Addr::new"),
                ("SocketAddr::new", "core::net::socket_addr::SocketAddr::new"),
                ("IpAddr::from", "core::convert::From::from"),
                ("IpAddr", "core::net::ip_addr::IpAddr"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn net_socket_is_flagged() {
        let diagnostics = run(
            "use std::net::{self, TcpStream};\nfn new() { TcpStream::connect(a); net::UdpSocket::bind(a); std::net::TcpListener::bind(a); }",
            &[
                ("TcpStream::connect", "std::net::tcp::TcpStream::connect"),
                ("net::UdpSocket::bind", "std::net::udp::UdpSocket::bind"),
                (
                    "std::net::TcpListener::bind",
                    "std::net::tcp::TcpListener::bind",
                ),
            ],
        );

        assert_eq!(diagnostics.len(), 3);
    }

    #[test]
    fn new_is_flagged_naming_the_call() {
        let diagnostics = run(
            "impl CheckPlan {\n    pub fn new(path: &Path) -> anyhow::Result<Self> {\n        let config = std::fs::read_to_string(path.join(\"stiles.toml\"))?;\n        todo!()\n    }\n}",
            &[("std::fs::read_to_string", "std::fs::read_to_string")],
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("`new` reaches `std::fs::read_to_string`");
    }

    #[test]
    fn new_is_flagged_once() {
        let diagnostics = run(
            "impl CheckPlan {\n    pub fn new(path: &Path) -> anyhow::Result<Self> {\n        let config = std::fs::read_to_string(path.join(\"stiles.toml\"))?;\n        todo!()\n    }\n}",
            &[("std::fs::read_to_string", "std::fs::read_to_string")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn process_exit_is_not_flagged() {
        let diagnostics = run(
            "fn parse() { std::process::exit(1); }",
            &[("std::process::exit", "std::process::exit")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn pure_function_without_io_is_not_flagged() {
        let diagnostics = run(
            "fn resolve(config: &Config) -> Plan { Plan::from(config) }",
            &[("Plan::from", "core::convert::From::from")],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn qualifier_naming_an_io_type_is_flagged_naming_the_type() {
        let diagnostics = run(
            "fn new() { reqwest::Client::default(); }",
            &[
                (
                    "reqwest::Client::default",
                    "core::default::Default::default",
                ),
                ("reqwest::Client", "reqwest::async_impl::client::Client"),
            ],
        );

        assert_diagnostic(&diagnostics[0])
            .message_contains("`reqwest::async_impl::client::Client`");
    }

    #[test]
    fn raw_identifier_name_is_flagged() {
        let diagnostics = run("fn r#new() { std::fs::read(p); }", &[READ]);

        assert_eq!(diagnostics.len(), 1);
    }

    #[cfg(feature = "plugin")]
    #[test]
    fn reads_returns_the_resolution_the_target_and_the_test_code() {
        let reads = stiles_plugin::DeclaresRules::reads(&IoInPureFunction);

        assert_eq!(
            reads,
            stiles_plugin::Reads::of::<Resolution>()
                .and::<stiles_rust::decorations::Target>()
                .and::<stiles_rust::decorations::TestCode>()
        );
    }

    #[test]
    fn reexported_function_is_flagged_naming_the_definition() {
        let diagnostics = run(
            "use crate::helpers::slurp;\nfn new() { slurp(p); }",
            &[("slurp", "std::fs::read")],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("`new` reaches `std::fs::read`");
    }

    #[test]
    fn renamed_module_import_is_flagged_naming_the_definition() {
        let diagnostics = run(
            "use std::fs as filesystem;\nfn new() { filesystem::read(p); }",
            &[("filesystem::read", "std::fs::read")],
        );

        assert_diagnostic(&diagnostics[0]).message_contains("`std::fs::read`");
    }

    #[test]
    fn renamed_type_import_is_flagged() {
        let diagnostics = run(
            "use std::process::Command as Cmd;\nfn plan() { Cmd::new(\"git\"); }",
            &[("Cmd::new", "std::process::Command::new")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn reqwest_client_is_flagged() {
        let diagnostics = run(
            "fn new() { reqwest::Client::new(); reqwest::Client::builder(); reqwest::blocking::get(u); reqwest::get(u); }",
            &[
                (
                    "reqwest::Client::new",
                    "reqwest::async_impl::client::Client::new",
                ),
                (
                    "reqwest::Client::builder",
                    "reqwest::async_impl::client::Client::builder",
                ),
                ("reqwest::blocking::get", "reqwest::blocking::get"),
                ("reqwest::get", "reqwest::get"),
            ],
        );

        assert_eq!(diagnostics.len(), 4);
    }

    #[test]
    fn reqwest_value_type_is_not_flagged() {
        let diagnostics = run(
            "fn parse_url(s: &str) { reqwest::Url::parse(s); reqwest::Method::from_bytes(b); reqwest::header::HeaderValue::from_static(v); }",
            &[
                ("reqwest::Url::parse", "url::Url::parse"),
                (
                    "reqwest::Method::from_bytes",
                    "http::method::Method::from_bytes",
                ),
                (
                    "reqwest::header::HeaderValue::from_static",
                    "http::header::value::HeaderValue::from_static",
                ),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn signature_without_body_is_not_flagged() {
        let diagnostics = run("trait Load { fn new() -> Self; }", &[]);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn test_attribute_is_not_flagged() {
        let source = "#[tokio::test]\nasync fn parse() { std::fs::read(p); }";

        let diagnostics = run_attaching(source, &[READ], |tree| {
            test_decorations::mark(tree, "async fn parse")
        });

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn trait_default_body_is_flagged() {
        let diagnostics = run("trait Plan { fn plan() { std::fs::read(p); } }", &[READ]);

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
            &[READ],
        );

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn turbofish_call_is_flagged() {
        let diagnostics = run(
            "use std::fs;\nfn new() { fs::read::<&str>(p); }",
            &[("fs::read", "std::fs::read")],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn type_annotation_is_not_flagged() {
        let diagnostics = run(
            "fn new(file: std::fs::File) -> Self { let f: fs::File = file; }",
            &[
                ("std::fs::File", "std::fs::File"),
                ("fs::File", "std::fs::File"),
            ],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn unresolved_name_is_not_flagged() {
        let diagnostics = run("fn new() { read_to_string(p); }", &[]);

        assert_no_diagnostics(&diagnostics);
    }
}
