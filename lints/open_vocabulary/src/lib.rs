use std::collections::HashMap;

use whisker_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use whisker_types::{DecoratedNode, Diagnostic, LintPass, Severity};

const RULE_ID: RuleId = RuleId::new("lint.open-vocabulary");

/// The variant names that read as a catch-all for whatever the enum lacks
const CATCH_ALL_NAMES: &[&str] = &["Other", "Custom", "Unknown", "Extension", "Unrecognized"];

/// Flags an enum variant or a public field that reopens a closed vocabulary
///
/// An enum is a closed vocabulary. Every consumer matches it exhaustively,
/// and it grows by a new variant that the compiler then walks to every
/// match. A catch-all variant with a payload undoes that. A producer can
/// put anything in it, and a consumer can match none of it.
///
/// The rule reports a variant named `Other`, `Custom`, `Unknown`,
/// `Extension`, or `Unrecognized` that carries a payload. A payload-less
/// `Other` is a closed classification, so the rule leaves it alone.
///
/// The rule also reports a variant field or a `pub` struct field whose type
/// names a slot that holds anything. These are a `Box<dyn Any>`, a
/// `serde_json::Value`, and a `HashMap`, `BTreeMap`, or `IndexMap` keyed by
/// `String`. The type can name the slot anywhere, so `Option<Value>` counts.
/// A variant field is as visible as its enum, so every variant field counts.
/// A struct field with a restricted or no visibility is not reported. A type
/// that erases values behind its own accessors does so by design.
///
/// A bare `Value` counts only when a `use` in scope imports it from
/// `serde_json`, because many crates have a `Value` of their own. A bare
/// `Box`, `Any`, `String`, or map name counts unless a `use` in scope binds
/// it to something else. The rule reads no decoration.
///
/// A type that deserializes an external format holds a `serde_json::Value`
/// for good reason. The rule cannot tell which types those are, so it only
/// warns.
///
/// # Examples
///
/// ```ignore
/// pub enum CheckEvent {
///     FileChecked { file: PathBuf, diagnostics: usize },
///     Other(String),
/// }
/// ```
///
/// The rule reports `Other`. A variant per case, such as
/// `FileSkipped { file: PathBuf, gap: CoverageGap }`, keeps the enum closed.
pub struct OpenVocabulary;

impl OpenVocabulary {
    /// Creates a boxed [`LintPass`] suitable for the whisker pipeline
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let pass = OpenVocabulary::into_lint_pass();
    /// ```
    pub fn into_lint_pass() -> Box<dyn LintPass> {
        Box::new(RustLintPassAdapter::new(Self))
    }
}

/// A type the rule recognizes by its path
///
/// A bare name matches when no `use` in scope binds it. A type that a bare
/// name does not prove, such as `serde_json::Value`, has no bare name.
struct KnownType {
    bare: Option<&'static str>,
    paths: &'static [&'static str],
}

const BOX: KnownType = KnownType {
    bare: Some("Box"),
    paths: &["std::boxed::Box", "alloc::boxed::Box"],
};

const ANY: KnownType = KnownType {
    bare: Some("Any"),
    paths: &["std::any::Any", "core::any::Any"],
};

const JSON_VALUE: KnownType = KnownType {
    bare: None,
    paths: &["serde_json::Value", "serde_json::value::Value"],
};

const STRING: KnownType = KnownType {
    bare: Some("String"),
    paths: &["std::string::String", "alloc::string::String"],
};

const MAPS: &[KnownType] = &[
    KnownType {
        bare: Some("HashMap"),
        paths: &[
            "std::collections::HashMap",
            "std::collections::hash_map::HashMap",
            "hashbrown::HashMap",
        ],
    },
    KnownType {
        bare: Some("BTreeMap"),
        paths: &[
            "std::collections::BTreeMap",
            "std::collections::btree_map::BTreeMap",
            "alloc::collections::BTreeMap",
            "alloc::collections::btree_map::BTreeMap",
        ],
    },
    KnownType {
        bare: Some("IndexMap"),
        paths: &["indexmap::IndexMap", "indexmap::map::IndexMap"],
    },
];

/// A kind of slot that holds a value the type does not name
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum OpenSlot {
    AnyBox,
    JsonValue,
    StringKeyedMap,
}

impl OpenSlot {
    /// Returns how a diagnostic names this slot
    fn description(self) -> &'static str {
        match self {
            OpenSlot::AnyBox => "a `Box<dyn Any>`",
            OpenSlot::JsonValue => "a `serde_json::Value`",
            OpenSlot::StringKeyedMap => "a map keyed by `String`",
        }
    }
}

/// Whether a field is part of the shape a type shows its consumers
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Exposure {
    Public,
    Hidden,
}

/// One field of a struct or a variant, with the type it declares
struct Field<'a> {
    label: String,
    exposure: Exposure,
    declared: DecoratedNode<'a>,
}

/// The names that `use` declarations bind where an item is defined
///
/// Each local name maps to the path segments it stands for. A name that
/// an inner scope binds hides the same name from an outer scope.
#[derive(Default)]
struct Imports {
    bindings: HashMap<String, Vec<String>>,
}

impl Imports {
    /// Collects the imports visible to `item`
    ///
    /// A block sees its own `use` declarations and those of every scope
    /// around it, up to the module. An inline module starts a new scope, so
    /// the walk stops at the first module or at the file.
    ///
    /// A type parameter of `item` hides an import of the same name. It
    /// binds the empty path, which names no type the rule knows.
    fn visible_to(item: &DecoratedNode<'_>) -> Self {
        let mut imports = Self::default();
        if let Some(parameters) = item.child_by_field_name("type_parameters") {
            for parameter in parameters.named_children() {
                let Some(name) = parameter.child_by_field_name("name") else {
                    continue;
                };
                imports.bindings.insert(name.text().to_string(), Vec::new());
            }
        }
        let mut current = item.parent();

        while let Some(scope) = current {
            let module_root = match scope.kind() {
                "source_file" => true,
                "declaration_list" => scope
                    .parent()
                    .is_some_and(|parent| parent.kind() == "mod_item"),
                _ => false,
            };
            if module_root || scope.kind() == "block" {
                imports.add_declarations_in(&scope);
            }
            if module_root {
                break;
            }
            current = scope.parent();
        }

        imports
    }

    /// Adds the bindings of every `use` declaration directly inside `scope`
    fn add_declarations_in(&mut self, scope: &DecoratedNode<'_>) {
        for child in scope.named_children() {
            if child.kind() != "use_declaration" {
                continue;
            }
            let Some(argument) = child.child_by_field_name("argument") else {
                continue;
            };
            self.add_tree(&argument, &[]);
        }
    }

    /// Adds the bindings of one use tree under the path `prefix`
    ///
    /// A glob binds no name the rule can see, so it adds nothing.
    fn add_tree(&mut self, tree: &DecoratedNode<'_>, prefix: &[String]) {
        match tree.kind() {
            "use_list" => {
                for item in tree.named_children() {
                    self.add_tree(&item, prefix);
                }
            }
            "scoped_use_list" => {
                let Some(list) = tree.child_by_field_name("list") else {
                    return;
                };
                let mut nested = prefix.to_vec();
                if let Some(path) = tree.child_by_field_name("path") {
                    nested.extend(segments(path.text()));
                }
                self.add_tree(&list, &nested);
            }
            "use_as_clause" => {
                let (Some(path), Some(alias)) = (
                    tree.child_by_field_name("path"),
                    tree.child_by_field_name("alias"),
                ) else {
                    return;
                };
                self.bind(Some(alias.text()), prefix, &path);
            }
            "identifier" | "scoped_identifier" | "self" => self.bind(None, prefix, tree),
            _ => {}
        }
    }

    /// Binds a name to `path` under `prefix`, unless an inner scope bound it
    ///
    /// The name is `alias` when the import renames, and the last segment of
    /// the path otherwise. A trailing `self` names the module the prefix
    /// ends in, so it is dropped from the path first.
    fn bind(&mut self, alias: Option<&str>, prefix: &[String], path: &DecoratedNode<'_>) {
        let mut target = joined(prefix, path.text());
        if target.last().is_some_and(|segment| segment == "self") {
            target.pop();
        }
        let Some(last) = target.last() else {
            return;
        };
        let local = alias.unwrap_or(last).to_string();
        if local == "_" {
            return;
        }
        self.bindings.entry(local).or_insert(target);
    }

    /// Returns the full path that the path written in `node` resolves to
    ///
    /// A path that starts with `::` names a crate directly, so no import
    /// rewrites it.
    fn resolve(&self, node: &DecoratedNode<'_>) -> Vec<String> {
        let written = segments(node.text());
        let Some(first) = written.first() else {
            return written;
        };
        if first.is_empty() {
            return written[1..].to_vec();
        }
        let Some(target) = self.bindings.get(first) else {
            return written;
        };

        let mut resolved = target.clone();
        resolved.extend_from_slice(&written[1..]);
        resolved
    }

    /// Returns whether the path written in `node` names `known`
    fn names(&self, node: &DecoratedNode<'_>, known: &KnownType) -> bool {
        let resolved = self.resolve(node);
        if let [only] = resolved.as_slice()
            && known.bare == Some(only.as_str())
        {
            return true;
        }

        let resolved = resolved.join("::");
        known.paths.contains(&resolved.as_str())
    }
}

/// Splits a written path into its segments, ignoring whitespace
///
/// A leading `::` yields an empty first segment.
fn segments(text: &str) -> Vec<String> {
    let text: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    text.split("::").map(str::to_string).collect()
}

/// Returns `prefix` followed by the segments written in `text`
fn joined(prefix: &[String], text: &str) -> Vec<String> {
    let mut path = prefix.to_vec();
    path.extend(
        segments(text)
            .into_iter()
            .filter(|segment| !segment.is_empty()),
    );
    path
}

/// Returns the first type argument of a generic type, skipping lifetimes
fn first_type_argument<'a>(generic: &DecoratedNode<'a>) -> Option<DecoratedNode<'a>> {
    let arguments = generic.child_by_field_name("type_arguments")?;
    arguments
        .named_children()
        .into_iter()
        .find(|argument| !matches_kind(argument, &["lifetime", "line_comment", "block_comment"]))
}

/// Returns whether the kind of `node` is one of `kinds`
fn matches_kind(node: &DecoratedNode<'_>, kinds: &[&str]) -> bool {
    kinds.contains(&node.kind())
}

/// Returns whether a trait object type has `Any` among its bounds
///
/// `dyn Any + Send` parses as a bounded type around `dyn Any`, and
/// `dyn Send + Any` puts `Any` beside the `dyn`. Both count.
fn is_any_object(object: &DecoratedNode<'_>, imports: &Imports) -> bool {
    match object.kind() {
        "dynamic_type" => object
            .child_by_field_name("trait")
            .is_some_and(|bound| imports.names(&bound, &ANY)),
        "bounded_type" => object
            .named_children()
            .iter()
            .any(|bound| is_any_bound(bound, imports)),
        _ => false,
    }
}

/// Returns whether one bound of a bounded trait object is `Any`
fn is_any_bound(bound: &DecoratedNode<'_>, imports: &Imports) -> bool {
    match bound.kind() {
        "dynamic_type" | "bounded_type" => is_any_object(bound, imports),
        "type_identifier" | "scoped_type_identifier" => imports.names(bound, &ANY),
        _ => false,
    }
}

/// Returns the open slot that `node` itself is, if any
fn slot_at(node: &DecoratedNode<'_>, imports: &Imports) -> Option<OpenSlot> {
    match node.kind() {
        "generic_type" => {
            let name = node.child_by_field_name("type")?;
            let argument = first_type_argument(node)?;
            if imports.names(&name, &BOX) && is_any_object(&argument, imports) {
                return Some(OpenSlot::AnyBox);
            }
            let is_map = MAPS.iter().any(|map| imports.names(&name, map));
            if is_map && imports.names(&argument, &STRING) {
                return Some(OpenSlot::StringKeyedMap);
            }
            None
        }
        "type_identifier" | "scoped_type_identifier" => imports
            .names(node, &JSON_VALUE)
            .then_some(OpenSlot::JsonValue),
        _ => None,
    }
}

/// Returns the outermost open slot that the type `declared` names
///
/// A scoped path is read whole, so its last segment is never read as a
/// bare name of its own.
fn find_slot<'a>(
    declared: &DecoratedNode<'a>,
    imports: &Imports,
) -> Option<(OpenSlot, DecoratedNode<'a>)> {
    if let Some(slot) = slot_at(declared, imports) {
        return Some((slot, *declared));
    }
    if declared.kind() == "scoped_type_identifier" {
        return None;
    }

    declared
        .named_children()
        .iter()
        .find_map(|child| find_slot(child, imports))
}

/// Returns whether the given node is a bare `pub` visibility modifier
///
/// A restricted form such as `pub(crate)` holds its scope as a named child,
/// so a bare `pub` is the one with no named children.
fn is_bare_pub(node: &DecoratedNode<'_>) -> bool {
    node.kind() == "visibility_modifier" && node.named_child_count() == 0
}

/// Returns the fields of a named-field body
fn named_fields<'a>(body: &DecoratedNode<'a>) -> Vec<Field<'a>> {
    let mut fields = Vec::new();

    for declaration in body.named_children() {
        if declaration.kind() != "field_declaration" {
            continue;
        }
        let (Some(name), Some(declared)) = (
            declaration.child_by_field_name("name"),
            declaration.child_by_field_name("type"),
        ) else {
            continue;
        };
        let exposure = if declaration.named_children().iter().any(is_bare_pub) {
            Exposure::Public
        } else {
            Exposure::Hidden
        };

        fields.push(Field {
            label: format!("`{}`", name.text()),
            exposure,
            declared,
        });
    }

    fields
}

/// Returns the fields of a tuple body
///
/// The body holds no per-field node. A visibility modifier, attributes, and
/// the type sit as flat siblings between two commas. A comma that belongs to
/// a type sits inside that type and never reaches the loop.
fn ordered_fields<'a>(body: &DecoratedNode<'a>) -> Vec<Field<'a>> {
    let mut fields = Vec::new();
    let mut exposure = Exposure::Hidden;

    for position in 0..body.child_count() as u32 {
        let Some(child) = body.child(position) else {
            continue;
        };

        if child.kind() == "," {
            exposure = Exposure::Hidden;
            continue;
        }
        if is_bare_pub(&child) {
            exposure = Exposure::Public;
            continue;
        }
        if !child.is_named()
            || matches_kind(
                &child,
                &[
                    "visibility_modifier",
                    "attribute_item",
                    "line_comment",
                    "block_comment",
                ],
            )
        {
            continue;
        }

        fields.push(Field {
            label: fields.len().to_string(),
            exposure,
            declared: child,
        });
    }

    fields
}

/// Returns the fields of a struct or variant body
fn fields<'a>(body: &DecoratedNode<'a>) -> Vec<Field<'a>> {
    match body.kind() {
        "field_declaration_list" => named_fields(body),
        "ordered_field_declaration_list" => ordered_fields(body),
        _ => Vec::new(),
    }
}

/// Returns a diagnostic for every field in `fields` that holds an open slot
///
/// The diagnostic names the field, then `owner`. A variant passes its own
/// name there, and a struct passes an empty string.
fn slot_diagnostics(fields: Vec<Field<'_>>, owner: &str, imports: &Imports) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for Field {
        label, declared, ..
    } in fields
    {
        let Some((slot, node)) = find_slot(&declared, imports) else {
            continue;
        };

        diagnostics.push(Diagnostic::builder().rule_id(RULE_ID).severity(Severity::Warn).message(format!(
                "field {label}{owner} holds {}, which takes values no consumer can match; give it a type that names them",
                slot.description()
            )).span(node.span()).build());
    }

    diagnostics
}

/// Returns the diagnostics for one enum variant
///
/// A variant field has no visibility of its own. It is as visible as the
/// enum, so every field counts.
fn variant_diagnostics(variant: &DecoratedNode<'_>, imports: &Imports) -> Vec<Diagnostic> {
    let Some(name) = variant.child_by_field_name("name") else {
        return Vec::new();
    };
    let Some(body) = variant.child_by_field_name("body") else {
        return Vec::new();
    };
    let fields = fields(&body);
    if fields.is_empty() {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    if CATCH_ALL_NAMES.contains(&name.text()) {
        diagnostics.push(Diagnostic::builder().rule_id(RULE_ID).severity(Severity::Warn).message(format!(
                "variant `{}` is a catch-all with a payload; give each case it holds a variant of its own",
                name.text()
            )).span(name.span()).build());
    }
    let owner = format!(" of variant `{}`", name.text());
    diagnostics.extend(slot_diagnostics(fields, &owner, imports));

    diagnostics
}

impl RustLintPass for OpenVocabulary {
    fn check_enum_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };
        let imports = Imports::visible_to(node);

        body.named_children()
            .iter()
            .filter(|variant| variant.kind() == "enum_variant")
            .flat_map(|variant| variant_diagnostics(variant, &imports))
            .collect()
    }

    fn check_struct_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };
        let imports = Imports::visible_to(node);

        let public = fields(&body)
            .into_iter()
            .filter(|field| field.exposure == Exposure::Public)
            .collect();

        slot_diagnostics(public, "", &imports)
    }
}

#[cfg(feature = "plugin")]
impl whisker_plugin::DeclaresRules for OpenVocabulary {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }
}

whisker_plugin::export_lints![rust: [OpenVocabulary]];

#[cfg(test)]
mod tests {
    use whisker_testing::{assert_diagnostic, assert_no_diagnostics, execute, parse};
    use whisker_types::{Language, LintPass, Severity};

    use super::*;

    fn adapt() -> Box<dyn LintPass> {
        OpenVocabulary::into_lint_pass()
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        let mut passes = vec![adapt()];
        execute(&tree, &mut passes)
    }

    #[test]
    fn any_box_behind_bounds_flagged_once() {
        let types = [
            "Box<dyn Any>",
            "Box<dyn Any + Send>",
            "Box<dyn Any + Send + Sync>",
            "Box<dyn Any + Send + Sync + 'static>",
            "Box<dyn Send + Any>",
            "Box<dyn Send + Sync + Any>",
            "Box<dyn std::any::Any + Send>",
            "Box<dyn ::std::any::Any>",
            "Box<dyn core::any::Any>",
            "std::boxed::Box<dyn Any>",
            "alloc::boxed::Box<dyn Any + Send>",
        ];

        for declared in types {
            let diagnostics = run(&format!("pub struct S {{ pub slot: {declared} }}"));

            assert_eq!(diagnostics.len(), 1, "{declared}");
        }
    }

    #[test]
    fn any_box_behind_bounds_flagged_with_the_box_message() {
        let types = [
            "Box<dyn Any>",
            "Box<dyn Any + Send>",
            "Box<dyn Any + Send + Sync>",
            "Box<dyn Any + Send + Sync + 'static>",
            "Box<dyn Send + Any>",
            "Box<dyn Send + Sync + Any>",
            "Box<dyn std::any::Any + Send>",
            "Box<dyn ::std::any::Any>",
            "Box<dyn core::any::Any>",
            "std::boxed::Box<dyn Any>",
            "alloc::boxed::Box<dyn Any + Send>",
        ];

        for declared in types {
            let diagnostics = run(&format!("pub struct S {{ pub slot: {declared} }}"));

            assert_diagnostic(&diagnostics[0]).message_contains("holds a `Box<dyn Any>`");
        }
    }

    #[test]
    fn any_box_diagnostic_is_reported_once() {
        let source = "pub struct S { pub slot: Option<Box<dyn Any + Send>> }";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn any_box_diagnostic_spans_the_box() {
        let source = "pub struct S { pub slot: Option<Box<dyn Any + Send>> }";

        let diagnostics = run(source);

        let needle = "Box<dyn Any + Send>";
        let start = source
            .find(needle)
            .expect("the needle should be in the source");
        let end = start + needle.len();
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, end);
    }

    #[test]
    fn any_imported_from_elsewhere_not_flagged() {
        let diagnostics = run("use crate::plugin::Any;\npub struct S { pub slot: Box<dyn Any> }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn any_imported_from_std_flagged() {
        let diagnostics = run("use std::any::Any;\npub struct S { pub slot: Box<dyn Any> }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn box_of_other_trait_objects_not_flagged() {
        let types = [
            "Box<dyn Error>",
            "Box<dyn Error + Send + Sync>",
            "Box<dyn Fn(u32) -> u32>",
            "Box<dyn my::Any>",
            "Box<u32>",
            "Arc<dyn Error>",
            "&'static dyn Any",
        ];

        for declared in types {
            let diagnostics = run(&format!("pub struct S {{ pub slot: {declared} }}"));

            assert_no_diagnostics(&diagnostics);
        }
    }

    #[test]
    fn catch_all_diagnostic_is_reported_once() {
        let source = "pub enum Kind { Known, Other(String) }";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn catch_all_diagnostic_spans_the_variant_name() {
        let source = "pub enum Kind { Known, Other(String) }";

        let diagnostics = run(source);

        let needle = "Other";
        let start = source
            .find(needle)
            .expect("the needle should be in the source");
        let end = start + needle.len();
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start, end);
    }

    #[test]
    fn catch_all_in_function_flagged() {
        let diagnostics = run("fn main() { enum Kind { Known, Other(String) } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn catch_all_in_private_enum_flagged() {
        let diagnostics = run("enum Kind { Known, Other(String) }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn catch_all_name_with_tuple_payload_flagged_as_a_catch_all_warning() {
        for name in CATCH_ALL_NAMES {
            let diagnostics = run(&format!("pub enum Kind {{ Known, {name}(u32) }}"));

            assert_diagnostic(&diagnostics[0])
                .has_rule_id("lint.open-vocabulary")
                .has_severity(Severity::Warn)
                .message_contains(&format!("variant `{name}` is a catch-all with a payload"));
        }
    }

    #[test]
    fn catch_all_name_with_tuple_payload_flagged_once() {
        for name in CATCH_ALL_NAMES {
            let diagnostics = run(&format!("pub enum Kind {{ Known, {name}(u32) }}"));

            assert_eq!(diagnostics.len(), 1, "{name}");
        }
    }

    #[test]
    fn catch_all_with_named_payload_flagged_naming_the_variant() {
        let diagnostics = run("pub enum Kind { Known, Unknown { code: u16 } }");

        assert_diagnostic(&diagnostics[0]).message_contains("variant `Unknown`");
    }

    #[test]
    fn catch_all_with_named_payload_flagged_once() {
        let diagnostics = run("pub enum Kind { Known, Unknown { code: u16 } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn catch_all_with_open_payload_first_diagnostic_names_the_catch_all() {
        let diagnostics = run("pub enum Kind { Known, Custom(Box<dyn Any + Send>) }");

        assert_diagnostic(&diagnostics[0]).message_contains("variant `Custom` is a catch-all");
    }

    #[test]
    fn catch_all_with_open_payload_flagged_twice() {
        let diagnostics = run("pub enum Kind { Known, Custom(Box<dyn Any + Send>) }");

        assert_eq!(diagnostics.len(), 2);
    }

    #[test]
    fn catch_all_with_open_payload_second_diagnostic_names_the_any_box_field() {
        let diagnostics = run("pub enum Kind { Known, Custom(Box<dyn Any + Send>) }");

        assert_diagnostic(&diagnostics[1])
            .message_contains("field 0 of variant `Custom` holds a `Box<dyn Any>`");
    }

    #[test]
    fn closed_vocabulary_from_issue_not_flagged() {
        let source = "pub enum CheckEvent {
    FileChecked { file: PathBuf, diagnostics: usize },
    FileSkipped { file: PathBuf, gap: CoverageGap },
    Diagnostic(Diagnostic),
    Finished(Tally),
}";

        let diagnostics = run(source);

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_catch_all_payload_not_flagged() {
        let diagnostics = run("pub enum Kind { Other(), Unknown {} }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn generic_parameter_hides_imported_name() {
        let diagnostics = run("use serde_json::Value;\npub struct S<Value> { pub data: Value }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn json_value_bare_without_import_not_flagged() {
        let diagnostics = run("pub struct S { pub data: Value }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn json_value_from_another_crate_not_flagged() {
        let sources = [
            "pub struct S { pub data: toml::Value }",
            "use toml::Value;\npub struct S { pub data: Value }",
            "use serde_json::Value;\npub struct S { pub data: toml::Value }",
            "use serde_json::Value as Json;\npub struct S { pub data: Value }",
        ];

        for source in sources {
            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }
    }

    #[test]
    fn json_value_glob_import_not_flagged() {
        let diagnostics = run("use serde_json::*;\npub struct S { pub data: Value }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn json_value_import_in_function_reaches_struct_in_it() {
        let diagnostics = run("fn main() { use serde_json::Value; struct S { pub data: Value } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn json_value_import_in_inner_block_shadows_outer_one() {
        let diagnostics = run(
            "use serde_json::Value;\nfn main() { use toml::Value; struct S { pub data: Value } }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn json_value_import_in_module_reaches_function_body() {
        let diagnostics = run("use serde_json::Value;\nfn main() { struct S { pub data: Value } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn json_value_import_in_other_module_not_flagged() {
        let sources = [
            "mod a { use serde_json::Value; }\nmod b { pub struct S { pub data: Value } }",
            "use serde_json::Value;\nmod b { pub struct S { pub data: Value } }",
        ];

        for source in sources {
            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }
    }

    #[test]
    fn json_value_import_inside_same_module_flagged() {
        let diagnostics = run("mod b { use serde_json::Value; pub struct S { pub data: Value } }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn json_value_scoped_name_is_not_read_as_bare() {
        let diagnostics = run("use serde_json::Value;\npub struct S { pub data: config::Value }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn json_value_spellings_flagged_once() {
        let sources = [
            "pub struct S { pub data: serde_json::Value }",
            "pub struct S { pub data: ::serde_json::Value }",
            "pub struct S { pub data: serde_json :: Value }",
            "pub struct S { pub data: serde_json::value::Value }",
            "use serde_json::Value;\npub struct S { pub data: Value }",
            "use ::serde_json::Value;\npub struct S { pub data: Value }",
            "use serde_json::{json, Value};\npub struct S { pub data: Value }",
            "use serde_json::{value::Value};\npub struct S { pub data: Value }",
            "use serde_json::Value as Json;\npub struct S { pub data: Json }",
            "use serde_json as json;\npub struct S { pub data: json::Value }",
            "use serde_json::{self as json};\npub struct S { pub data: json::Value }",
            "use serde_json::value;\npub struct S { pub data: value::Value }",
            "pub struct S { pub data: Value }\nuse serde_json::Value;",
        ];

        for source in sources {
            let diagnostics = run(source);

            assert_eq!(diagnostics.len(), 1, "{source}");
        }
    }

    #[test]
    fn json_value_spellings_flagged_with_the_field_message() {
        let sources = [
            "pub struct S { pub data: serde_json::Value }",
            "pub struct S { pub data: ::serde_json::Value }",
            "pub struct S { pub data: serde_json :: Value }",
            "pub struct S { pub data: serde_json::value::Value }",
            "use serde_json::Value;\npub struct S { pub data: Value }",
            "use ::serde_json::Value;\npub struct S { pub data: Value }",
            "use serde_json::{json, Value};\npub struct S { pub data: Value }",
            "use serde_json::{value::Value};\npub struct S { pub data: Value }",
            "use serde_json::Value as Json;\npub struct S { pub data: Json }",
            "use serde_json as json;\npub struct S { pub data: json::Value }",
            "use serde_json::{self as json};\npub struct S { pub data: json::Value }",
            "use serde_json::value;\npub struct S { pub data: value::Value }",
            "pub struct S { pub data: Value }\nuse serde_json::Value;",
        ];

        for source in sources {
            let diagnostics = run(source);

            assert_diagnostic(&diagnostics[0])
                .message_contains("field `data` holds a `serde_json::Value`");
        }
    }

    #[test]
    fn nested_slot_flagged() {
        let types = [
            "Option<serde_json::Value>",
            "Vec<Box<dyn Any>>",
            "Option<HashMap<String, u32>>",
            "(u8, serde_json::Value)",
            "[serde_json::Value; 2]",
            "&'a serde_json::Value",
            "Arc<Mutex<HashMap<String, u32>>>",
        ];

        for declared in types {
            let diagnostics = run(&format!("pub struct S<'a> {{ pub slot: {declared} }}"));

            assert_eq!(diagnostics.len(), 1, "{declared}");
        }
    }

    #[test]
    fn open_vocabulary_from_issue_first_diagnostic_names_other() {
        let source = "pub enum CheckEvent {
    FileChecked { file: PathBuf, diagnostics: usize },
    Other(String),
    Custom(Box<dyn Any + Send>),
}";

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0]).message_contains("variant `Other`");
    }

    #[test]
    fn open_vocabulary_from_issue_flagged_three_times() {
        let source = "pub enum CheckEvent {
    FileChecked { file: PathBuf, diagnostics: usize },
    Other(String),
    Custom(Box<dyn Any + Send>),
}";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 3);
    }

    #[test]
    fn open_vocabulary_from_issue_second_diagnostic_names_custom() {
        let source = "pub enum CheckEvent {
    FileChecked { file: PathBuf, diagnostics: usize },
    Other(String),
    Custom(Box<dyn Any + Send>),
}";

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[1]).message_contains("variant `Custom`");
    }

    #[test]
    fn open_vocabulary_from_issue_third_diagnostic_names_the_custom_field() {
        let source = "pub enum CheckEvent {
    FileChecked { file: PathBuf, diagnostics: usize },
    Other(String),
    Custom(Box<dyn Any + Send>),
}";

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[2]).message_contains("field 0 of variant `Custom`");
    }

    #[test]
    fn outermost_slot_diagnostic_names_the_map_over_its_span() {
        let source = "pub struct S { pub extra: HashMap<String, serde_json::Value> }";

        let diagnostics = run(source);

        let needle = "HashMap<String, serde_json::Value>";
        let start = source
            .find(needle)
            .expect("the needle should be in the source");
        let end = start + needle.len();
        assert_diagnostic(&diagnostics[0])
            .message_contains("holds a map keyed by `String`")
            .has_span("<test>", start, end);
    }

    #[test]
    fn outermost_slot_reported_once() {
        let source = "pub struct S { pub extra: HashMap<String, serde_json::Value> }";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn payloadless_catch_all_not_flagged() {
        let diagnostics = run("pub enum Kind { Known, Other, Unknown = 7 }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn private_struct_field_not_flagged() {
        let sources = [
            "pub struct S { data: serde_json::Value }",
            "pub struct S { pub(crate) data: serde_json::Value }",
            "pub struct S { pub(super) data: HashMap<String, u32> }",
            "pub struct S(Box<dyn Any>);",
            "pub struct S(pub(crate) Box<dyn Any>);",
        ];

        for source in sources {
            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }
    }

    #[test]
    fn similar_variant_names_not_flagged() {
        let diagnostics = run("pub enum Kind { Others(u32), OtherKind(u32), Unknowns(u32) }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn string_keyed_map_spellings_flagged_once() {
        let types = [
            "HashMap<String, u32>",
            "HashMap<String, u32, RandomState>",
            "BTreeMap<String, String>",
            "IndexMap<String, Vec<u8>>",
            "std::collections::HashMap<String, u32>",
            "std::collections::btree_map::BTreeMap<String, u32>",
            "indexmap::IndexMap<String, u32>",
            "HashMap<std::string::String, u32>",
        ];

        for declared in types {
            let diagnostics = run(&format!("pub struct S {{ pub map: {declared} }}"));

            assert_eq!(diagnostics.len(), 1, "{declared}");
        }
    }

    #[test]
    fn string_keyed_map_spellings_flagged_with_the_map_message() {
        let types = [
            "HashMap<String, u32>",
            "HashMap<String, u32, RandomState>",
            "BTreeMap<String, String>",
            "IndexMap<String, Vec<u8>>",
            "std::collections::HashMap<String, u32>",
            "std::collections::btree_map::BTreeMap<String, u32>",
            "indexmap::IndexMap<String, u32>",
            "HashMap<std::string::String, u32>",
        ];

        for declared in types {
            let diagnostics = run(&format!("pub struct S {{ pub map: {declared} }}"));

            assert_diagnostic(&diagnostics[0]).message_contains("holds a map keyed by `String`");
        }
    }

    #[test]
    fn string_keyed_map_under_other_name_not_flagged() {
        let diagnostics = run(
            "use crate::cache::Store as HashMap;\npub struct S { pub map: HashMap<String, u32> }",
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn struct_tuple_field_after_comment_and_attribute_flagged_once() {
        let diagnostics = run(
            "pub struct S(\n    /// the payload\n    #[serde(default)]\n    pub serde_json::Value,\n);",
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn struct_tuple_field_after_comment_and_attribute_flagged_with_its_index() {
        let diagnostics = run(
            "pub struct S(\n    /// the payload\n    #[serde(default)]\n    pub serde_json::Value,\n);",
        );

        assert_diagnostic(&diagnostics[0]).message_contains("field 0 holds");
    }

    #[test]
    fn struct_tuple_field_flagged_once() {
        let diagnostics = run("pub struct S(u8, pub serde_json::Value, pub u32);");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn struct_tuple_field_flagged_with_its_index() {
        let diagnostics = run("pub struct S(u8, pub serde_json::Value, pub u32);");

        assert_diagnostic(&diagnostics[0]).message_contains("field 1 holds a `serde_json::Value`");
    }

    #[test]
    fn trait_send_exposure() {
        fn assert_send<T: Send>() {}
        assert_send::<Exposure>();
    }

    #[test]
    fn trait_send_field() {
        fn assert_send<T: Send>() {}
        assert_send::<Field<'static>>();
    }

    #[test]
    fn trait_send_imports() {
        fn assert_send<T: Send>() {}
        assert_send::<Imports>();
    }

    #[test]
    fn trait_send_known_type() {
        fn assert_send<T: Send>() {}
        assert_send::<KnownType>();
    }

    #[test]
    fn trait_send_open_slot() {
        fn assert_send<T: Send>() {}
        assert_send::<OpenSlot>();
    }

    #[test]
    fn trait_send_open_vocabulary() {
        fn assert_send<T: Send>() {}
        assert_send::<OpenVocabulary>();
    }

    #[test]
    fn trait_sync_exposure() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Exposure>();
    }

    #[test]
    fn trait_sync_field() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Field<'static>>();
    }

    #[test]
    fn trait_sync_imports() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Imports>();
    }

    #[test]
    fn trait_sync_known_type() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<KnownType>();
    }

    #[test]
    fn trait_sync_open_slot() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<OpenSlot>();
    }

    #[test]
    fn trait_sync_open_vocabulary() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<OpenVocabulary>();
    }

    #[test]
    fn trait_unpin_exposure() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Exposure>();
    }

    #[test]
    fn trait_unpin_field() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Field<'static>>();
    }

    #[test]
    fn trait_unpin_imports() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<Imports>();
    }

    #[test]
    fn trait_unpin_known_type() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<KnownType>();
    }

    #[test]
    fn trait_unpin_open_slot() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<OpenSlot>();
    }

    #[test]
    fn trait_unpin_open_vocabulary() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<OpenVocabulary>();
    }

    #[test]
    fn types_without_open_slot_not_flagged() {
        let sources = [
            "pub struct S { pub map: HashMap<u32, String> }",
            "pub struct S { pub map: HashMap<&'static str, u32> }",
            "pub struct S { pub map: HashMap<Key, String> }",
            "pub struct S { pub names: Vec<String> }",
            "pub struct S { pub names: HashSet<String> }",
            "pub struct S<T> { pub item: T }",
            "pub struct S(pub T);",
            "pub struct S;",
            "pub union U { pub data: serde_json::Value }",
        ];

        for source in sources {
            let diagnostics = run(source);

            assert_no_diagnostics(&diagnostics);
        }
    }

    #[test]
    fn variant_named_field_flagged_once() {
        let source = "enum Event { Loaded { extra: HashMap<String, u32> } }";

        let diagnostics = run(source);

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn variant_named_field_flagged_with_field_and_variant_names() {
        let source = "enum Event { Loaded { extra: HashMap<String, u32> } }";

        let diagnostics = run(source);

        assert_diagnostic(&diagnostics[0])
            .message_contains("field `extra` of variant `Loaded` holds a map keyed by `String`");
    }

    #[test]
    fn variant_tuple_field_flagged_once() {
        let diagnostics = run("pub enum Event { Payload(u8, serde_json::Value) }");

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn variant_tuple_field_flagged_with_its_index() {
        let diagnostics = run("pub enum Event { Payload(u8, serde_json::Value) }");

        assert_diagnostic(&diagnostics[0])
            .message_contains("field 1 of variant `Payload` holds a `serde_json::Value`");
    }
}
