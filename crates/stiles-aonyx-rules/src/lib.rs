//! The entry point to the Aonyx rules for stiles
//!
//! Each module below is one rule. It holds the rule's pass and its
//! `RULE_ID`. The docs of the pass say what the rule reports and why.
//!
//! This crate also gives the workspace its name. Otto releases the
//! workspace as one lockstep unit, and such a unit needs a crate with the
//! name of its repository. Every crate inherits the version of the
//! workspace, so one release moves the facade and every rule together.
//!
//! A project loads the rules from a commit of this repository, so no crate
//! is published.

pub use anyhow_missing_context;
pub use bool_param;
pub use commented_out_code;
pub use derive_order;
pub use doc_summary_period;
pub use doc_summary_person;
pub use em_dash_aside;
pub use error_enum_shape;
pub use external_type_in_port;
pub use facade_bypass_import;
pub use function_scoped_import;
pub use if_let_with_else;
pub use inline_comment;
pub use inline_doc_link;
pub use io_in_pure_function;
pub use long_doc_sentence;
pub use matches_macro;
pub use missing_examples_doc;
pub use missing_trait_tests;
pub use multi_assert_test;
pub use multi_param_constructor;
pub use open_vocabulary;
pub use outer_module_doc;
pub use pub_anyhow_result;
pub use pub_field;
pub use repeated_field_access;
pub use repeated_primitive_params;
pub use split_binding;
pub use terminal_output;
pub use todo_comment;
pub use unprefixed_submodule_path;
pub use unseparated_test_body;
pub use unsorted_test;
pub use wildcard_match_arm;
