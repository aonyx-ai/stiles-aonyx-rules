use stiles_rust::decorations::{Resolution, TypePathRef};
use stiles_rust::{RuleId, RustLintPass, RustLintPassAdapter};
use stiles_types::{DecoratedNode, Diagnostic, LintPass, Severity};

/// The id that every diagnostic of this rule carries
///
/// A project names the rule by this id when it configures the rule.
///
/// # Examples
///
/// ```
/// assert_eq!(open_vocabulary::RULE_ID.as_str(), "lint.open-vocabulary");
/// ```
pub const RULE_ID: RuleId = RuleId::new("lint.open-vocabulary");

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
/// The rule reads the [`Resolution`] decoration of each type name. A name
/// counts when it resolves to the definition of one of these types. A glob
/// import, a rename, and a re-export all resolve there. A `Value` of another
/// crate resolves elsewhere, so it does not count. A name without the
/// decoration does not count either.
///
/// A type that deserializes an external format may hold a
/// `serde_json::Value`, but the rule cannot tell which types those are. Such
/// a type keeps the value in a private field, which the rule does not report.
///
/// # Examples
///
/// ```text
/// // DON'T
/// pub enum Shape {
///     Circle { radius: f64 },
///     Other(String),
/// }
///
/// // DO
/// pub enum Shape {
///     Circle { radius: f64 },
///     Square { side: f64 },
/// }
/// ```
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
    /// Creates a boxed [`LintPass`] suitable for the stiles pipeline
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

const BOX: &[TypePathRef<'static>] = &[TypePathRef::builder()
    .krate("alloc")
    .modules(&["boxed"])
    .name("Box")
    .build()];

const ANY: &[TypePathRef<'static>] = &[TypePathRef::builder()
    .krate("core")
    .modules(&["any"])
    .name("Any")
    .build()];

const JSON_VALUE: &[TypePathRef<'static>] = &[TypePathRef::builder()
    .krate("serde_json")
    .modules(&["value"])
    .name("Value")
    .build()];

const STRING: &[TypePathRef<'static>] = &[TypePathRef::builder()
    .krate("alloc")
    .modules(&["string"])
    .name("String")
    .build()];

const MAPS: &[TypePathRef<'static>] = &[
    TypePathRef::builder()
        .krate("std")
        .modules(&["collections", "hash", "map"])
        .name("HashMap")
        .build(),
    TypePathRef::builder()
        .krate("hashbrown")
        .modules(&["map"])
        .name("HashMap")
        .build(),
    TypePathRef::builder()
        .krate("alloc")
        .modules(&["collections", "btree", "map"])
        .name("BTreeMap")
        .build(),
    TypePathRef::builder()
        .krate("indexmap")
        .modules(&["map"])
        .name("IndexMap")
        .build(),
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

/// Returns whether the type name `node` resolves to one of the definitions in `known`
///
/// A name without a [`Resolution`] names none of them.
fn names(node: &DecoratedNode<'_>, known: &[TypePathRef<'_>]) -> bool {
    node.decoration::<Resolution>()
        .and_then(Resolution::path)
        .is_some_and(|path| known.iter().any(|candidate| path == candidate))
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
fn is_any_object(object: &DecoratedNode<'_>) -> bool {
    match object.kind() {
        "dynamic_type" => object
            .child_by_field_name("trait")
            .is_some_and(|bound| names(&bound, ANY)),
        "bounded_type" => object.named_children().iter().any(is_any_bound),
        _ => false,
    }
}

/// Returns whether one bound of a bounded trait object is `Any`
fn is_any_bound(bound: &DecoratedNode<'_>) -> bool {
    match bound.kind() {
        "dynamic_type" | "bounded_type" => is_any_object(bound),
        "type_identifier" | "scoped_type_identifier" => names(bound, ANY),
        _ => false,
    }
}

/// Returns the open slot that `node` itself is, if any
fn slot_at(node: &DecoratedNode<'_>) -> Option<OpenSlot> {
    match node.kind() {
        "generic_type" => {
            let name = node.child_by_field_name("type")?;
            let argument = first_type_argument(node)?;
            if names(&name, BOX) && is_any_object(&argument) {
                return Some(OpenSlot::AnyBox);
            }
            if names(&name, MAPS) && names(&argument, STRING) {
                return Some(OpenSlot::StringKeyedMap);
            }
            None
        }
        "type_identifier" | "scoped_type_identifier" => {
            names(node, JSON_VALUE).then_some(OpenSlot::JsonValue)
        }
        _ => None,
    }
}

/// Returns the outermost open slot that the type `declared` names
///
/// A scoped path is read whole, so its last segment is never read as a
/// bare name of its own.
fn find_slot<'a>(declared: &DecoratedNode<'a>) -> Option<(OpenSlot, DecoratedNode<'a>)> {
    if let Some(slot) = slot_at(declared) {
        return Some((slot, *declared));
    }
    if declared.kind() == "scoped_type_identifier" {
        return None;
    }

    declared.named_children().iter().find_map(find_slot)
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
fn slot_diagnostics(fields: Vec<Field<'_>>, owner: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for Field {
        label, declared, ..
    } in fields
    {
        let Some((slot, node)) = find_slot(&declared) else {
            continue;
        };

        diagnostics.push(Diagnostic::builder().rule_id(RULE_ID).severity(Severity::Error).message(format!(
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
fn variant_diagnostics(variant: &DecoratedNode<'_>) -> Vec<Diagnostic> {
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
        diagnostics.push(Diagnostic::builder().rule_id(RULE_ID).severity(Severity::Error).message(format!(
                "variant `{}` is a catch-all with a payload; give each case it holds a variant of its own",
                name.text()
            )).span(name.span()).build());
    }
    let owner = format!(" of variant `{}`", name.text());
    diagnostics.extend(slot_diagnostics(fields, &owner));

    diagnostics
}

impl RustLintPass for OpenVocabulary {
    fn check_enum_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };

        body.named_children()
            .iter()
            .filter(|variant| variant.kind() == "enum_variant")
            .flat_map(|variant| variant_diagnostics(variant))
            .collect()
    }

    fn check_struct_item(&mut self, node: &DecoratedNode<'_>) -> Vec<Diagnostic> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };

        let public = fields(&body)
            .into_iter()
            .filter(|field| field.exposure == Exposure::Public)
            .collect();

        slot_diagnostics(public, "")
    }
}

#[cfg(feature = "plugin")]
impl stiles_plugin::DeclaresRules for OpenVocabulary {
    fn rules(&self) -> Vec<RuleId> {
        vec![RULE_ID]
    }

    fn reads(&self) -> stiles_plugin::Reads {
        stiles_plugin::Reads::of::<Resolution>()
    }
}

#[cfg(feature = "plugin")]
stiles_plugin::export_lints![rust: [OpenVocabulary]];

#[cfg(test)]
mod tests {
    use stiles_rust::decorations::{DefinitionKind, DefinitionOrigin, TypePath};
    use stiles_testing::{assert_diagnostic, assert_no_diagnostics, decorate, execute, parse};
    use stiles_types::{DecorationMap, Language, LintPass, Severity};

    use super::*;

    /// The definitions that the provider finds for the spellings the tests write
    const SPELLINGS: &[(&str, &str)] = &[
        ("Box", "alloc::boxed::Box"),
        ("std::boxed::Box", "alloc::boxed::Box"),
        ("alloc::boxed::Box", "alloc::boxed::Box"),
        ("Any", "core::any::Any"),
        ("std::any::Any", "core::any::Any"),
        ("core::any::Any", "core::any::Any"),
        ("my::Any", "sample::my::Any"),
        ("Error", "core::error::Error"),
        ("Arc", "alloc::sync::Arc"),
        ("String", "alloc::string::String"),
        ("std::string::String", "alloc::string::String"),
        ("HashMap", "std::collections::hash::map::HashMap"),
        (
            "std::collections::HashMap",
            "std::collections::hash::map::HashMap",
        ),
        ("BTreeMap", "alloc::collections::btree::map::BTreeMap"),
        (
            "std::collections::btree_map::BTreeMap",
            "alloc::collections::btree::map::BTreeMap",
        ),
        ("IndexMap", "indexmap::map::IndexMap"),
        ("indexmap::IndexMap", "indexmap::map::IndexMap"),
        ("serde_json::Value", "serde_json::value::Value"),
        ("serde_json::value::Value", "serde_json::value::Value"),
        ("toml::Value", "toml::value::Value"),
        ("config::Value", "config::Value"),
    ];

    /// The definition of `serde_json::Value`
    const JSON: &str = "serde_json::value::Value";

    /// Returns the resolution of a name to the definition at `path`
    fn resolution(path: &str) -> Resolution {
        let mut segments: Vec<&str> = path.split("::").collect();
        let name = segments.pop().expect("the path should have a name");
        let krate = segments.remove(0);

        Resolution::builder()
            .kind(DefinitionKind::Struct)
            .origin(DefinitionOrigin::OtherCrate)
            .krate(krate)
            .path(
                TypePath::builder()
                    .krate(krate)
                    .modules(segments)
                    .name(name)
                    .build(),
            )
            .build()
    }

    /// Returns what the provider attaches to a type name, if anything
    ///
    /// A spelling in `extra` wins over one in [`SPELLINGS`]. A name at the
    /// end of a scoped path gets nothing, because the rule reads the path.
    fn resolve(node: &DecoratedNode<'_>, extra: &[(&str, Resolution)]) -> Option<Resolution> {
        if !matches_kind(node, &["type_identifier", "scoped_type_identifier"])
            || node
                .parent()
                .is_some_and(|parent| parent.kind() == "scoped_type_identifier")
        {
            return None;
        }
        let written: String = node
            .text()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let written = written.trim_start_matches("::");
        if let Some((_, found)) = extra.iter().find(|(spelling, _)| *spelling == written) {
            return Some(found.clone());
        }

        SPELLINGS
            .iter()
            .find(|(spelling, _)| *spelling == written)
            .map(|(_, path)| resolution(path))
    }

    fn mark(node: &DecoratedNode<'_>, map: &mut DecorationMap, extra: &[(&str, Resolution)]) {
        if let Some(found) = resolve(node, extra) {
            map.insert(node.id(), found);
        }
        for child in node.named_children() {
            mark(&child, map, extra);
        }
    }

    /// Runs the rule after resolving names as [`SPELLINGS`] and `extra` say
    fn run_resolving(source: &str, extra: &[(&str, Resolution)]) -> Vec<Diagnostic> {
        let mut tree = parse(source, Language::Rust);
        let mut map = DecorationMap::new();
        mark(&tree.root_node(), &mut map, extra);
        decorate(&mut tree, map);
        let mut passes: Vec<Box<dyn LintPass>> = vec![OpenVocabulary::into_lint_pass()];

        execute(&tree, &mut passes)
    }

    fn run(source: &str) -> Vec<Diagnostic> {
        run_resolving(source, &[])
    }

    /// Runs the rule on a tree that carries no decoration
    fn run_undecorated(source: &str) -> Vec<Diagnostic> {
        let tree = parse(source, Language::Rust);
        execute(&tree, &mut [OpenVocabulary::into_lint_pass()])
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
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start..end);
    }

    #[test]
    fn any_imported_from_elsewhere_not_flagged() {
        let diagnostics = run_resolving(
            "use crate::plugin::Any;\npub struct S { pub slot: Box<dyn Any> }",
            &[("Any", resolution("sample::plugin::Any"))],
        );

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
        assert_diagnostic(&diagnostics[0]).has_span("<test>", start..end);
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
    fn catch_all_name_with_tuple_payload_flagged_as_a_catch_all_error() {
        for name in CATCH_ALL_NAMES {
            let diagnostics = run(&format!("pub enum Kind {{ Known, {name}(u32) }}"));

            assert_diagnostic(&diagnostics[0])
                .has_rule_id("lint.open-vocabulary")
                .has_severity(Severity::Error)
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
    fn documented_example_is_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_undecorated(pair.example());

        assert!(!diagnostics.is_empty());
    }

    #[test]
    fn documented_fix_is_not_flagged() {
        let pair = example_pair::ExamplePair::read(include_str!("lib.rs"));

        let diagnostics = run_undecorated(pair.fix());

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn empty_catch_all_payload_not_flagged() {
        let diagnostics = run("pub enum Kind { Other(), Unknown {} }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn generic_parameter_named_value_not_flagged() {
        let parameter = Resolution::builder()
            .kind(DefinitionKind::TypeParameter)
            .origin(DefinitionOrigin::SameCrate)
            .krate("sample")
            .build();

        let diagnostics = run_resolving(
            "use serde_json::Value;\npub struct S<Value> { pub data: Value }",
            &[("Value", parameter)],
        );

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn json_value_bare_without_resolution_not_flagged() {
        let diagnostics = run("pub struct S { pub data: Value }");

        assert_no_diagnostics(&diagnostics);
    }

    #[test]
    fn json_value_from_another_crate_not_flagged() {
        let sources = [
            ("pub struct S { pub data: toml::Value }", None),
            (
                "use toml::Value;\npub struct S { pub data: Value }",
                Some("toml::value::Value"),
            ),
            (
                "use serde_json::Value;\npub struct S { pub data: toml::Value }",
                None,
            ),
        ];

        for (source, value) in sources {
            let extra: Vec<(&str, Resolution)> = value
                .map(|path| ("Value", resolution(path)))
                .into_iter()
                .collect();

            let diagnostics = run_resolving(source, &extra);

            assert_no_diagnostics(&diagnostics);
        }
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
            let diagnostics = run_resolving(source, &json_aliases());

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
            let diagnostics = run_resolving(source, &json_aliases());

            assert_diagnostic(&diagnostics[0])
                .message_contains("field `data` holds a `serde_json::Value`");
        }
    }

    #[test]
    fn json_value_through_a_glob_import_flagged() {
        let diagnostics = run_resolving(
            "use serde_json::*;\npub struct S { pub data: Value }",
            &[("Value", resolution(JSON))],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn json_value_through_a_reexport_flagged() {
        let diagnostics = run_resolving(
            "use crate::wire::Payload;\npub struct S { pub data: Payload }",
            &[("Payload", resolution(JSON))],
        );

        assert_eq!(diagnostics.len(), 1);
    }

    /// Resolves each name that the spelling tests import `serde_json::Value` as
    fn json_aliases() -> Vec<(&'static str, Resolution)> {
        ["Value", "Json", "json::Value", "value::Value"]
            .into_iter()
            .map(|alias| (alias, resolution(JSON)))
            .collect()
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
            .has_span("<test>", start..end);
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
    fn string_keyed_map_renamed_flagged() {
        let diagnostics = run_resolving(
            "use std::collections::HashMap as Map;\npub struct S { pub map: Map<String, u32> }",
            &[("Map", resolution("std::collections::hash::map::HashMap"))],
        );

        assert_eq!(diagnostics.len(), 1);
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
        let diagnostics = run_resolving(
            "use crate::cache::Store as HashMap;\npub struct S { pub map: HashMap<String, u32> }",
            &[("HashMap", resolution("sample::cache::Store"))],
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
    fn unresolved_open_slots_not_flagged() {
        let types = ["Box<dyn Any>", "HashMap<String, u32>", "serde_json::Value"];

        for declared in types {
            let diagnostics = run_undecorated(&format!("pub struct S {{ pub slot: {declared} }}"));

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
